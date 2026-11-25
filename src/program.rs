use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use crate::{
    history::{nonnegative, read_sets},
    lyfta::{LyftaClient, Request, numeric_id},
    preview::Catalog,
    program_store::ProgramStore,
    tools::template_schema,
};

pub async fn preview(
    client: &LyftaClient,
    store: &ProgramStore,
    args: Map<String, Value>,
    cancellation: &CancellationToken,
) -> Result<Value, String> {
    let draft = Value::Object(args);
    let mut collection = draft["collection"].clone();
    trim_title(&mut collection);
    let mut workouts = draft["days"].as_array().unwrap().clone();
    let mut catalog = Catalog::default();
    let mut matches = Vec::new();
    let mut ready = true;

    for workout in &mut workouts {
        trim_title(workout);
        let (matched, exercises) = catalog.match_workout(client, workout, cancellation).await?;
        ready &= matched;
        matches.push(exercises);
    }

    let use_history = draft["use_history"] == true;
    let unit = draft["weight_unit"].as_str();
    if use_history && unit.is_none() {
        return Err(
            "Choose the Lyfta weight unit (kg or lb) before filling weights from history.".into(),
        );
    }
    let mut ids = HashSet::new();
    for workout in &workouts {
        for exercise in workout["exercises"].as_array().unwrap() {
            if exercise.get("name").is_some() {
                continue; // Still awaiting a catalog match.
            }
            for set in exercise["sets"].as_array().into_iter().flatten() {
                if has_weight(set) {
                    continue;
                }
                if use_history && weighted(exercise) {
                    ids.insert(exercise["exercise_id"].as_u64().unwrap());
                }
                for reference in set["estimate"]["source_refs"]
                    .as_array()
                    .into_iter()
                    .flatten()
                {
                    let (_, id, _) = parse_ref(reference.as_str().unwrap())?;
                    ids.insert(id);
                }
            }
        }
    }
    let history = read_sets(client, &ids, draft["history_from"].as_str(), cancellation).await?;
    let mut by_ref = HashMap::new();
    let mut recent: HashMap<u64, Vec<&Value>> = HashMap::new();
    for set in &history {
        by_ref.insert(set["ref"].as_str().unwrap(), set);
        let recent_sets = recent
            .entry(set["exercise_id"].as_u64().unwrap())
            .or_default();
        if recent_sets
            .first()
            .is_none_or(|latest| latest["workout_id"] == set["workout_id"])
        {
            recent_sets.push(set);
        }
    }

    let mut days = Vec::new();
    for (workout, mut exercise_matches) in workouts.iter_mut().zip(matches) {
        let mut loads = Vec::new();
        for (exercise, matched) in workout["exercises"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .zip(&mut exercise_matches)
        {
            let can_load = weighted(exercise);
            let mut estimates = Vec::new();
            let id = exercise["exercise_id"]
                .as_u64()
                .filter(|_| matched["status"] == "matched");
            for set in exercise["sets"].as_array_mut().into_iter().flatten() {
                let estimate = set.as_object_mut().unwrap().remove("estimate");
                let basis = set.as_object_mut().unwrap().remove("load_basis");
                normalize_set(set);
                let load = if has_weight(set) {
                    json!({"kind": "entered", "unit": unit, "load_basis": basis.as_ref().and_then(Value::as_str).unwrap_or("unknown")})
                } else if let Some(id) = id {
                    match estimate {
                        Some(estimate) => {
                            let (valid, details) =
                                apply_estimate(set, &estimate, id, unit, can_load, &by_ref);
                            ready &= valid;
                            details
                        }
                        None if can_load && use_history => {
                            let source_sets =
                                recent.get(&id).map(Vec::as_slice).unwrap_or_default();
                            apply_history(
                                set,
                                source_sets,
                                unit,
                                basis.as_ref().and_then(Value::as_str),
                            )
                        }
                        None => json!({"kind": "blank"}),
                    }
                } else {
                    json!({"kind": "awaiting_catalog_selection"})
                };
                estimates.push(load);
            }
            loads.push(estimates);
        }
        days.push(
            json!({"workout": workout, "exercise_matches": exercise_matches, "set_loads": loads}),
        );
    }

    let mut result = json!({"ready": ready, "draft": draft, "days": days});
    if ready {
        let account_id = client.account_id(cancellation).await?;
        let (preview_id, collection_title, day_titles) =
            store.prepare(account_id, collection, workouts)?;
        result["preview_id"] = json!(preview_id);
        result["approval_required"] = json!(true);
        result["saved_collection_title"] = json!(collection_title);
        for (day, title) in result["days"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .zip(day_titles)
        {
            day["saved_title"] = json!(title);
        }
    }
    Ok(result)
}

fn trim_title(object: &mut Value) {
    if let Some(title) = object["title"].as_str() {
        object["title"] = json!(title.trim());
    }
}

fn weighted(exercise: &Value) -> bool {
    exercise["exercise_type"].as_str().is_some_and(|kind| {
        // The live catalog also uses dedicated types for dumbbell variants.
        kind.starts_with("db_") || (kind.contains("weight") && kind.contains("reps"))
    })
}

fn has_weight(set: &Value) -> bool {
    !set["weight"].is_null()
        && set["weight"]
            .as_str()
            .is_none_or(|weight| !weight.trim().is_empty())
}

fn parse_ref(reference: &str) -> Result<(u64, u64, &str), String> {
    let mut parts = reference.splitn(3, '/');
    parts
        .next()
        .and_then(|id| id.parse().ok())
        .zip(parts.next().and_then(|id| id.parse().ok()))
        .zip(parts.next().filter(|id| !id.is_empty()))
        .map(|((workout_id, exercise_id), set_id)| (workout_id, exercise_id, set_id))
        .ok_or("Use a source ref returned by lyfta_get_comparable_sets.".into())
}

fn normalize_set(set: &mut Value) {
    let set = set.as_object_mut().unwrap();
    set.retain(|_, value| !value.is_null());
    for (key, value) in set.iter_mut() {
        if key != "set_type_id" && value.is_number() {
            *value = json!(value.to_string());
        }
    }
    if let Some(weight) = set.get_mut("weight")
        && weight
            .as_str()
            .is_some_and(|weight| weight.trim().is_empty())
    {
        *weight = json!("");
    }
}

fn apply_history(
    set: &mut Value,
    sources: &[&Value],
    unit: Option<&str>,
    basis: Option<&str>,
) -> Value {
    let Some(latest) = sources.first() else {
        return json!({"kind": "no_same_exercise_history", "next_tool": "lyfta_get_comparable_sets"});
    };
    let reps = numeric_id(&set["reps"])
        .map(|reps| (reps, reps))
        .or_else(|| {
            numeric_id(&set["from_reps"])
                .zip(numeric_id(&set["to_reps"]))
                .filter(|(lower, upper)| lower <= upper)
        });
    let best = sources
        .iter()
        .copied()
        .filter(|source| {
            source["unit"].as_str() == unit
                && basis.is_none_or(|basis| basis == "unknown" || source["load_basis"] == basis)
        })
        .min_by_key(|source| {
            let count = source["reps"].as_u64().unwrap();
            reps.map_or(0, |(lower, upper)| {
                count.abs_diff(count.clamp(lower, upper))
            })
        });
    match best {
        Some(source) => {
            set["weight"] = json!(source["weight"].as_f64().unwrap().to_string());
            json!({"kind": "history", "source_set": source})
        }
        None if latest["unit"].as_str() != unit => {
            json!({"kind": "history_unit_unconfirmed", "source_set": latest})
        }
        None => json!({"kind": "history_load_basis_unconfirmed", "source_set": latest}),
    }
}

fn apply_estimate(
    set: &mut Value,
    estimate: &Value,
    target: u64,
    unit: Option<&str>,
    can_load: bool,
    history: &HashMap<&str, &Value>,
) -> (bool, Value) {
    let mut details = json!({"kind": "estimate", "proposal": estimate});
    let sources: Option<Vec<_>> = estimate["source_refs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|reference| {
            history
                .get(reference.as_str().unwrap())
                .copied()
                .filter(|source| {
                    source["exercise_id"].as_u64() != Some(target)
                        && !source["unit"].is_null()
                        && nonnegative(&source["weight"]).is_some_and(|weight| weight > 0.0)
                })
        })
        .collect();
    let Some(sources) = sources else {
        details["status"] = json!("source_unavailable");
        return (false, details);
    };
    details["source_sets"] = json!(sources);
    if !can_load || unit != estimate["unit"].as_str() {
        details["status"] = json!("check_exercise_type_and_plan_unit");
        return (false, details);
    }
    let weight = if let Some(weight) = estimate["weight"].as_f64() {
        weight
    } else {
        let lower = estimate["lower"].as_f64().unwrap();
        let upper = estimate["upper"].as_f64().unwrap();
        if lower > upper {
            details["status"] = json!("invalid_range");
            return (false, details);
        }
        let Some(selected) = estimate["selected_weight"].as_f64() else {
            details["status"] = json!("choose_weight");
            return (false, details);
        };
        if !(lower..=upper).contains(&selected) {
            details["status"] = json!("selected_weight_outside_range");
            return (false, details);
        }
        selected
    };
    set["weight"] = json!(weight.to_string());
    details["status"] = json!("selected");
    (true, details)
}

pub async fn load_draft(
    client: &LyftaClient,
    args: Map<String, Value>,
    cancellation: &CancellationToken,
) -> Result<Value, String> {
    let id = numeric_id(&args["collection_id"]).unwrap();
    let response = client
        .request(
            &format!("/api/v1/collections/{id}"),
            Request::Read(vec![("view".into(), "full".into())]),
            cancellation.clone(),
        )
        .await?;
    let data = &response["data"];
    let workouts = data["workouts"]
        .as_array()
        .ok_or("Lyfta returned no collection routines.")?;
    let mut by_id = HashMap::new();
    let mut card_ids = Vec::new();
    for workout in workouts {
        let id = numeric_id(&workout["id"]).ok_or("Lyfta returned a routine without an ID.")?;
        if by_id.insert(id, workout).is_none() {
            card_ids.push(id);
        }
    }
    let mut ids = Vec::new();
    let mut seen = HashSet::new();
    for value in data["workout_ids"].as_array().into_iter().flatten() {
        let id = numeric_id(value).ok_or("Lyfta returned an invalid routine ID.")?;
        if seen.insert(id) {
            ids.push(id);
        }
    }
    ids.extend(card_ids.into_iter().filter(|id| seen.insert(*id)));

    let workout_schema = &template_schema(true)["properties"]["workout"];
    let mut days = Vec::new();
    for id in ids {
        let mut workout = by_id.get(&id).copied().unwrap_or(&Value::Null).clone();
        if !workout["exercises"].is_array() {
            workout = client
                .request(
                    &format!("/api/v1/templates/{id}"),
                    Request::Read(vec![]),
                    cancellation.clone(),
                )
                .await?["data"]
                .clone();
        }
        let mut day = copy_known(&workout, workout_schema, &["exercises"]);
        let title = workout["title"]
            .as_str()
            .filter(|title| !title.trim().is_empty())
            .ok_or("Lyfta returned a routine without a title.")?;
        day["title"] = json!(crate::program_store::original_title(title));
        let schema = &workout_schema["properties"]["exercises"]["items"];
        let mut exercises = Vec::new();
        for exercise in workout["exercises"]
            .as_array()
            .ok_or("Lyfta returned a routine without exercises.")?
        {
            let mut copy = copy_known(exercise, schema, &["sets", "name"]);
            copy["name"] = json!(
                exercise["excercise_name"]
                    .as_str()
                    .filter(|name| !name.trim().is_empty())
                    .ok_or("Lyfta returned an exercise without a name.")?
            );
            copy["exercise_id"] = json!(
                numeric_id(&exercise["exercise_id"])
                    .filter(|id| *id > 0)
                    .ok_or("Lyfta returned an exercise without a catalog ID.")?
            );
            let set_schema = &schema["properties"]["sets"]["items"];
            copy["sets"] = json!(
                exercise["sets"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|set| { copy_known(set, set_schema, &[]) })
                    .collect::<Vec<_>>()
            );
            exercises.push(copy);
        }
        day["exercises"] = json!(exercises);
        days.push(day);
    }
    let mut collection = json!({"title": args["title"]});
    for key in ["description", "goal"] {
        if data[key].is_string() {
            collection[key] = data[key].clone();
        }
    }
    Ok(json!({
        "source_collection_id": id,
        "draft": {"collection": collection, "days": days},
        "next_tool": "lyfta_preview_program",
    }))
}

fn copy_known(source: &Value, schema: &Value, skip: &[&str]) -> Value {
    Value::Object(
        schema["properties"]
            .as_object()
            .unwrap()
            .iter()
            .filter_map(|(key, rule)| {
                let value = source.get(key)?;
                if skip.contains(&key.as_str()) || value.is_null() {
                    return None;
                }
                let converted = match rule["type"].as_str() {
                    Some("integer") => json!(numeric_id(value)?),
                    Some("string") if value.is_number() => json!(value.to_string()),
                    _ => value.clone(),
                };
                Some((key.clone(), converted))
            })
            .collect(),
    )
}
