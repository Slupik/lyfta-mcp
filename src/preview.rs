use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use crate::{
    exercise_metadata,
    lyfta::{LyftaClient, Request, numeric_id},
    tools::MAX_SAFE_INT,
};

pub async fn preview_template(
    client: &LyftaClient,
    args: Map<String, Value>,
    cancellation: CancellationToken,
) -> Result<Value, String> {
    let mut draft = Value::Object(args);
    let mut catalog = Catalog::default();
    let (ready, matches) = catalog
        .match_workout(client, &mut draft["workout"], &cancellation)
        .await?;
    let mut result = json!({"ready": ready, "exercise_matches": matches});
    if ready {
        result["create_template_arguments"] = draft;
    }
    Ok(result)
}

#[derive(Default)]
pub struct Catalog {
    searches: HashMap<String, Vec<Value>>,
    performed: Option<Vec<Value>>,
}

impl Catalog {
    pub async fn match_workout(
        &mut self,
        client: &LyftaClient,
        workout: &mut Value,
        cancellation: &CancellationToken,
    ) -> Result<(bool, Vec<Value>), String> {
        let exercises = workout["exercises"]
            .as_array_mut()
            .expect("validated preview exercises");
        let mut matches = Vec::with_capacity(exercises.len());
        let mut ready = true;

        for (index, exercise) in exercises.iter_mut().enumerate() {
            let name = exercise["name"].as_str().expect("validated exercise name");
            let search = name.split_whitespace().collect::<Vec<_>>().join(" ");
            let exact_name = search.to_lowercase();
            let selected_id = exercise["exercise_id"].as_u64();
            if !self.searches.contains_key(&exact_name) {
                self.searches.insert(
                    exact_name.clone(),
                    search_catalog(client, &search, cancellation).await?,
                );
            }
            let found = &self.searches[&exact_name];
            let matched = match select_exercise(found, &exact_name, selected_id) {
                Ok(candidate) => candidate,
                Err(status) => {
                    ready = false;
                    let candidates: Vec<Value> = found
                        .iter()
                        .filter(|item| status != "ambiguous" || exact_match(item, &exact_name))
                        .take(5)
                        .map(candidate_label)
                        .collect();
                    matches.push(json!({
                        "index": index, "requested_name": name, "status": status,
                        "candidates": candidates,
                    }));
                    continue;
                }
            };
            let mut fields = template_fields(matched);
            let mut fallback = &Value::Null;
            if fields.is_none() {
                if self.performed.is_none() {
                    self.performed = Some(client.performed_exercises(cancellation).await?);
                }
                let id = numeric_id(&matched["id"]);
                if let Some(item) = self
                    .performed
                    .as_ref()
                    .unwrap()
                    .iter()
                    .find(|item| numeric_id(&item["id"]) == id)
                {
                    fields = template_fields(item);
                    fallback = item;
                }
            }
            let Some(fields) = fields else {
                ready = false;
                matches.push(json!({
                    "index": index, "requested_name": name, "status": "missing_catalog_metadata",
                    "candidates": [candidate_label(matched)],
                }));
                continue;
            };
            let mut resolved = exercise_metadata::labels(matched, fallback);
            resolved.extend(Map::from_iter([
                ("index".into(), json!(index)),
                ("requested_name".into(), json!(name)),
                ("status".into(), json!("matched")),
                ("exercise_id".into(), fields["exercise_id"].clone()),
                ("name".into(), fields["excercise_name"].clone()),
            ]));
            matches.push(Value::Object(resolved));
            let exercise = exercise.as_object_mut().expect("validated exercise object");
            exercise.remove("name");
            exercise.extend(fields);
        }

        Ok((ready, matches))
    }
}

async fn search_catalog(
    client: &LyftaClient,
    search: &str,
    cancellation: &CancellationToken,
) -> Result<Vec<Value>, String> {
    let mut found = Vec::new();
    let mut ids = HashSet::new();
    let mut offset = 0;
    loop {
        let mut response = client
            .request(
                "/api/v1/exercises/library",
                Request::Read(vec![
                    ("search".into(), search.into()),
                    ("limit".into(), "100".into()),
                    ("offset".into(), offset.to_string()),
                ]),
                cancellation.clone(),
            )
            .await?;
        let batch = response["data"]["results"]
            .as_array_mut()
            .ok_or("Lyfta returned no exercise search results.")?;
        let count = batch.len();
        let before = found.len();
        found.extend(batch.drain(..).filter(|item| {
            item["name"]
                .as_str()
                .is_some_and(|name| !name.trim().is_empty())
                && numeric_id(&item["id"])
                    .is_some_and(|id| (1..=MAX_SAFE_INT).contains(&id) && ids.insert(id))
        }));
        let more = response["data"]["pagination"]["hasMore"]
            .as_bool()
            .ok_or("Lyfta returned no exercise search pagination.")?;
        if !more {
            return Ok(found);
        }
        if before == found.len() {
            return Err("Lyfta returned no new exercises on a search page.".into());
        }
        offset += count;
    }
}

fn select_exercise<'a>(
    found: &'a [Value],
    exact_name: &str,
    selected_id: Option<u64>,
) -> Result<&'a Value, &'static str> {
    if let Some(id) = selected_id {
        return found
            .iter()
            .find(|item| numeric_id(&item["id"]) == Some(id))
            .ok_or("selected_id_not_found");
    }
    let mut exact = found.iter().filter(|item| exact_match(item, exact_name));
    let first = exact.next().ok_or("no_exact_match")?;
    if exact.next().is_some() {
        return Err("ambiguous");
    }
    Ok(first)
}

fn exact_match(item: &Value, exact_name: &str) -> bool {
    item["name"].as_str().is_some_and(|name| {
        name.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
            == exact_name
    })
}

fn candidate_label(item: &Value) -> Value {
    let mut candidate = exercise_metadata::labels(item, &Value::Null);
    candidate.insert("exercise_id".into(), json!(numeric_id(&item["id"])));
    candidate.insert("name".into(), item["name"].clone());
    Value::Object(candidate)
}

fn template_fields(item: &Value) -> Option<Map<String, Value>> {
    let id = numeric_id(&item["id"]).filter(|id| (1..=MAX_SAFE_INT).contains(id))?;
    let name = item["name"]
        .as_str()
        .filter(|name| !name.trim().is_empty())?;
    let exercise_type = item["exercise_type"]
        .as_str()
        .filter(|kind| !kind.trim().is_empty())?;
    let image = item["image_name"].as_str()?;
    Some(Map::from_iter([
        ("exercise_id".into(), json!(id)),
        ("excercise_name".into(), json!(name)),
        ("exercise_type".into(), json!(exercise_type)),
        ("exercise_image".into(), json!(image)),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_an_exact_name_or_explicit_selection() {
        let found = vec![
            json!({"id": "3", "name": "Band Bench Press"}),
            json!({"id": 4, "name": "Bench Press"}),
        ];
        assert_eq!(select_exercise(&found, "bench press", None), Ok(&found[1]));
        assert_eq!(
            select_exercise(&found, "bench press", Some(3)),
            Ok(&found[0])
        );
        assert_eq!(
            select_exercise(&found, "bench press", Some(99)),
            Err("selected_id_not_found")
        );
        assert_eq!(
            select_exercise(&found, "bench", None),
            Err("no_exact_match")
        );
        assert_eq!(
            select_exercise(
                &[found[1].clone(), json!({"id": 5, "name": "Bench Press"})],
                "bench press",
                None
            ),
            Err("ambiguous")
        );
    }

    #[test]
    fn only_builds_arguments_with_complete_catalog_metadata() {
        let mut exercise = json!({"id":"9", "name":"Bench Press", "image_name":"https://example.com/image.png", "exercise_type":null});
        assert_eq!(template_fields(&exercise), None);
        exercise["exercise_type"] = json!("weight_reps");
        assert_eq!(
            template_fields(&exercise).map(Value::Object),
            Some(json!({
                "exercise_id":9, "excercise_name":"Bench Press",
                "exercise_image":"https://example.com/image.png", "exercise_type":"weight_reps"
            }))
        );
    }
}
