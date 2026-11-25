use std::time::Duration;

use reqwest::{Client, StatusCode, Url, header::ACCEPT, redirect, retry};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

const ORIGIN: &str = "https://my.lyfta.app";
const TIMEOUT: Duration = Duration::from_secs(15);
const VERIFY_WRITE: &str =
    " Check your Lyfta library before retrying, since the item may already exist.";

#[derive(Clone)]
pub struct LyftaClient {
    http: Client,
    origin: Url,
    key: Option<String>,
}

pub enum Request {
    Sample,
    Read(Vec<(String, String)>),
    Create(Map<String, Value>),
}

pub struct ApiError {
    pub message: String,
    pub rejected: bool,
}

impl From<String> for ApiError {
    fn from(message: String) -> Self {
        Self {
            message,
            rejected: false,
        }
    }
}

impl From<&str> for ApiError {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}

impl ApiError {
    fn rejection(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            rejected: true,
        }
    }
}

impl LyftaClient {
    pub fn new(key: Option<String>) -> Result<Self, reqwest::Error> {
        let http = Client::builder()
            .https_only(true)
            .redirect(redirect::Policy::none())
            .retry(retry::never())
            .timeout(TIMEOUT)
            .build()?;
        Ok(Self {
            http,
            origin: Url::parse(ORIGIN).expect("fixed Lyfta origin must be a valid URL"),
            key: key
                .map(|key| key.trim().to_owned())
                .filter(|key| !key.is_empty()),
        })
    }

    pub async fn request(
        &self,
        path: &str,
        request: Request,
        cancellation: CancellationToken,
    ) -> Result<Value, String> {
        self.exchange(path, request, cancellation)
            .await
            .map_err(|error| error.message)
    }

    #[cfg(test)]
    pub fn simulated(origin: Url) -> Self {
        Self {
            http: Client::builder()
                .no_proxy()
                .retry(retry::never())
                .build()
                .unwrap(),
            origin,
            key: Some("mock".into()),
        }
    }

    pub async fn exchange(
        &self,
        path: &str,
        request: Request,
        cancellation: CancellationToken,
    ) -> Result<Value, ApiError> {
        let creating = matches!(&request, Request::Create(_));
        let sample = matches!(&request, Request::Sample);
        let detail = if creating { VERIFY_WRITE } else { "" };
        let error = |message: &str| format!("{message}{detail}");
        let url = self
            .origin
            .join(path)
            .map_err(|_| "Invalid Lyfta API path.")?;
        let mut request = match request {
            Request::Sample => self.http.get(url),
            Request::Read(query) => self.http.get(url).query(&query),
            Request::Create(body) => self.http.post(url).json(&body),
        };
        request = request.header(ACCEPT, "application/json");
        if !sample {
            let key = self.key.as_deref().ok_or(
                "Set LYFTA_API_KEY to access your account, or use lyfta_sample_workouts to try sample data.",
            )?;
            request = request.bearer_auth(key);
        }

        if cancellation.is_cancelled() {
            return Err(ApiError::rejection(
                "The Lyfta request was cancelled before sending.",
            ));
        }

        let exchange = async {
            let response = request.send().await.map_err(|err| {
                if err.is_connect() {
                    ApiError::rejection("Could not connect to the Lyfta API.")
                } else if err.is_timeout() {
                    error("The Lyfta request timed out.").into()
                } else {
                    error("Could not complete the request to the Lyfta API.").into()
                }
            })?;
            match response.status() {
                status @ (StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) => {
                    return Err(ApiError::rejection(format!(
                        "Lyfta denied access (HTTP {}). Check that LYFTA_API_KEY is valid.",
                        status.as_u16()
                    )));
                }
                StatusCode::TOO_MANY_REQUESTS => {
                    return Err(ApiError::rejection(
                        "Lyfta’s request limit was reached. Wait before retrying.",
                    ));
                }
                status if !status.is_success() => {
                    if creating
                        && matches!(
                            status,
                            StatusCode::BAD_REQUEST
                                | StatusCode::NOT_FOUND
                                | StatusCode::UNPROCESSABLE_ENTITY
                        )
                        && let Ok(data) = response.json::<Value>().await
                        && let Some(rejection) = write_rejection(&data)
                    {
                        return Err(rejection);
                    }
                    return Err(error(&format!("Lyfta returned HTTP {}.", status.as_u16())).into());
                }
                _ => {}
            }
            let data = response.json::<Value>().await.map_err(|err| {
                if err.is_timeout() {
                    error("The Lyfta request timed out.")
                } else {
                    error("Lyfta returned an unreadable response.")
                }
            })?;
            if !data.is_object() {
                return Err(error("Lyfta returned an unexpected response.").into());
            }
            if data["status"] == false {
                if creating && let Some(rejection) = write_rejection(&data) {
                    return Err(rejection);
                }
                return Err(error("Lyfta reported that the request failed.").into());
            }
            if creating && (data["status"] != true || !has_created_id(&data["data"]["id"])) {
                return Err(error("Lyfta did not confirm the new item’s ID.").into());
            }
            Ok(data)
        };

        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(error("The Lyfta request was cancelled.").into()),
            outcome = exchange => outcome,
        }
    }

    pub async fn account_id(&self, cancellation: &CancellationToken) -> Result<u64, String> {
        let response = self
            .request("/api/v1/me", Request::Read(vec![]), cancellation.clone())
            .await?;
        numeric_id(&response["id"])
            .filter(|id| *id > 0)
            .ok_or("Lyfta returned no account ID.".into())
    }

    pub async fn performed_exercises(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<Vec<Value>, String> {
        let mut exercises = Vec::new();
        for page in 1.. {
            let mut response = self
                .request(
                    "/api/v1/exercises",
                    Request::Read(vec![
                        ("limit".into(), "100".into()),
                        ("page".into(), page.to_string()),
                    ]),
                    cancellation.clone(),
                )
                .await?;
            let batch = response["exercises"]
                .as_array_mut()
                .ok_or("Lyfta returned no performed exercise list.")?;
            let count = batch.len() as u64;
            exercises.append(batch);
            if count == 0
                || response["total_pages"].as_u64().map_or_else(
                    || count < response["limit"].as_u64().unwrap_or(100),
                    |total_pages| page >= total_pages,
                )
            {
                return Ok(exercises);
            }
        }
        unreachable!()
    }
}

fn write_rejection(data: &Value) -> Option<ApiError> {
    let message = data["message"].as_str()?;
    (data["status"] == false
        && matches!(
            message,
            "Title is required"
                | "workout is required"
                | "collectionId is required"
                | "Collection not found"
                | "Invalid API key"
                | "Not allowed to view this user"
        ))
    .then(|| ApiError::rejection(format!("Lyfta rejected the write: {message}.")))
}

fn has_created_id(id: &Value) -> bool {
    id.as_u64().is_some_and(|number| number > 0)
        || id.as_str().is_some_and(|text| !text.trim().is_empty())
}

pub fn numeric_id(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}
