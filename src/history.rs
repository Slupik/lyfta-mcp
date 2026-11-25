use std::collections::{HashMap, HashSet};

use chrono::{DateTime, NaiveDate, NaiveDateTime};
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use crate::{
    exercise_metadata,
    lyfta::{LyftaClient, Request, numeric_id},
};

pub async fn comparable_sets(
    client: &LyftaClient,
    args: Map<String, Value>,
    cancellation: &CancellationToken,
) -> Result<Value, String> {
    let target = args["target_exercise_id"].as_u64().unwrap();
    let ids: HashSet<_> = args["comparable_exercise_ids"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_u64)
        .filter(|id| *id != target)
        .collect();
    let history = read_sets(
        client,
        &ids,
        args.get("from").and_then(Value::as_str),
        cancellation,
    )
    .await?;
    let limit = args["sets_per_exercise"].as_u64().unwrap() as usize;
    let mut counts = HashMap::new();
    let sources: Vec<_> = history
        .into_iter()
        .filter(|set| {
            // A set without a unit can be read, but cannot support a numeric estimate.
            let count = counts
                .entry(set["exercise_id"].as_u64().unwrap())
                .or_insert(0);
            *count += 1;
            *count <= limit
        })
        .collect();
    Ok(json!({
        "target_exercise_id": target,
        "status": if sources.iter().any(|set| !set["unit"].is_null() && nonnegative(&set["weight"]).is_some_and(|weight| weight > 0.0)) { "evidence_available" } else { "no_comparable_sets" },
        "source_sets": sources,
        "guidance": "Check the movement, equipment, load basis, target reps and RIR yourself. Treat any chosen load as a first-session estimate. If the available sets cannot support an estimate, leave the weight blank and suggest a light trial set."
    }))
}

pub async fn read_sets(
    client: &LyftaClient,
    ids: &HashSet<u64>,
    from: Option<&str>,
    cancellation: &CancellationToken,
) -> Result<Vec<Value>, String> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let metadata: HashMap<_, _> = client
        .performed_exercises(cancellation)
        .await?
        .into_iter()
        .filter_map(|item| Some((numeric_id(&item["id"])?, item)))
        .collect();
    let mut sets = Vec::new();
    let mut seen = HashSet::new();
    for page in 1.. {
        let mut query = vec![
            ("limit".into(), "100".into()),
            ("page".into(), page.to_string()),
        ];
        if let Some(from) = from {
            query.push(("from".into(), from.into()));
        }
        let response = client
            .request(
                "/api/v1/workouts",
                Request::Read(query),
                cancellation.clone(),
            )
            .await?;
        let workouts = response["workouts"]
            .as_array()
            .ok_or("Lyfta returned no workout list.")?;
        let pages =
            numeric_id(&response["total_pages"]).ok_or("Lyfta returned no workout page count.")?;
        if !workouts.is_empty() && numeric_id(&response["current_page"]) != Some(page) {
            return Err("Lyfta returned an unexpected workout page.".into());
        }
        for workout in workouts {
            let Some(workout_id) = numeric_id(&workout["id"]) else {
                continue;
            };
            let Some(date) = workout["workout_perform_date"].as_str() else {
                continue;
            };
            let Some(timestamp) = workout_timestamp(date) else {
                continue;
            };
            for exercise in workout["exercises"].as_array().into_iter().flatten() {
                let Some(id) = numeric_id(&exercise["exercise_id"]).filter(|id| ids.contains(id))
                else {
                    continue;
                };
                let item = metadata.get(&id).unwrap_or(exercise);
                let labels = exercise_metadata::labels(item, exercise);
                for (index, set) in exercise["sets"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    if set["is_completed"] != true {
                        continue;
                    }
                    let Some(weight) = nonnegative(&set["weight"]) else {
                        continue;
                    };
                    let Some(reps) = numeric_id(&set["reps"]).filter(|count| *count > 0) else {
                        continue;
                    };
                    let Some(set_id) = set["id"]
                        .as_str()
                        .map(str::to_owned)
                        .or_else(|| set["id"].as_u64().map(|id| id.to_string()))
                    else {
                        continue;
                    };
                    if set_id.is_empty() {
                        continue;
                    }
                    let reference = format!("{workout_id}/{id}/{set_id}");
                    if !seen.insert(reference.clone()) {
                        continue;
                    }
                    let unit = [
                        &set["weight_unit"],
                        &exercise["weight_unit"],
                        &workout["weight_unit"],
                    ]
                    .into_iter()
                    .find_map(|value| value.as_str())
                    .filter(|unit| matches!(*unit, "kg" | "lb"));
                    let basis = [
                        &set["weight_basis"],
                        &exercise["weight_basis"],
                        &item["weight_basis"],
                    ]
                    .into_iter()
                    .find_map(|value| value.as_str())
                    .filter(|basis| matches!(*basis, "per_hand" | "total"));
                    let mut source = labels.clone();
                    source.extend(Map::from_iter([
                        ("ref".into(), json!(reference)),
                        ("workout_id".into(), json!(workout_id)),
                        ("exercise_id".into(), json!(id)),
                        (
                            "name".into(),
                            json!(
                                exercise["excercise_name"]
                                    .as_str()
                                    .or_else(|| item["name"].as_str())
                            ),
                        ),
                        ("weight".into(), json!(weight)),
                        ("unit".into(), json!(unit)),
                        ("load_basis".into(), json!(basis.unwrap_or("unknown"))),
                        ("reps".into(), json!(reps)),
                        ("date".into(), json!(date)),
                        ("rir".into(), json!(nonnegative(&set["rir"]))),
                    ]));
                    sets.push(((timestamp, workout_id, index), Value::Object(source)));
                }
            }
        }
        if page >= pages {
            break;
        }
    }
    sets.sort_by_key(|set| std::cmp::Reverse(set.0));
    Ok(sets.into_iter().map(|(_, set)| set).collect())
}

fn workout_timestamp(date: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(date)
        .ok()
        .map(|date| date.timestamp_millis())
        .or_else(|| {
            NaiveDateTime::parse_from_str(date, "%Y-%m-%d %H:%M:%S")
                .ok()
                .map(|date| date.and_utc().timestamp_millis())
        })
        .or_else(|| {
            NaiveDate::parse_from_str(date, "%Y-%m-%d")
                .ok()?
                .and_hms_opt(0, 0, 0)
                .map(|date| date.and_utc().timestamp_millis())
        })
}

pub fn nonnegative(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse().ok())
        .filter(|number| number.is_finite() && *number >= 0.0)
}
