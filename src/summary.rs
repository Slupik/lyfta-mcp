use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::NaiveDate;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::lyfta::{LyftaClient, Request};

const BODY_PARTS: [&str; 20] = [
    "Thighs",
    "Chest",
    "Hips",
    "Back",
    "Upper Arms",
    "Shoulders",
    "Forearms",
    "Calves",
    "Neck",
    "Cardio",
    "Full body",
    "Waist",
    "Plyometrics",
    "Weightlifting",
    "Yoga",
    "Stretching",
    "Biceps",
    "Triceps",
    "Quadriceps",
    "Hamstrings",
];

pub async fn training_summary(
    client: &LyftaClient,
    from: &str,
    to: &str,
    cancellation: CancellationToken,
) -> Result<Value, String> {
    let mut report = Summary::default();
    let mut body_parts = None;
    for page in 1.. {
        let response = client
            .request(
                "/api/v1/workouts",
                Request::Read(vec![
                    ("from".into(), from.into()),
                    ("to".into(), to.into()),
                    ("limit".into(), "100".into()),
                    ("page".into(), page.to_string()),
                ]),
                cancellation.clone(),
            )
            .await?;
        let workouts = response["workouts"]
            .as_array()
            .ok_or("Lyfta returned no workout list.")?;
        let total_pages = response["total_pages"]
            .as_u64()
            .ok_or("Lyfta returned no workout page count.")?;
        if !workouts.is_empty() && response["current_page"].as_u64() != Some(page) {
            return Err("Lyfta returned an unexpected workout page.".into());
        }
        if !workouts.is_empty() && body_parts.is_none() {
            body_parts = Some(
                client
                    .performed_exercises(&cancellation)
                    .await?
                    .iter()
                    .filter_map(|exercise| {
                        Some((
                            numeric_id(&exercise["id"])?,
                            catalog_body_parts(&exercise["body_part_id"]),
                        ))
                    })
                    .collect(),
            );
        }
        if let Some(body_parts) = &body_parts {
            for workout in workouts {
                report.add_workout(workout, body_parts);
            }
        }
        if page >= total_pages {
            return Ok(report.finish(from, to));
        }
    }
    unreachable!()
}

#[derive(Default)]
struct Summary {
    weeks: BTreeMap<NaiveDate, Week>,
    sessions: u64,
    sessions_without_date: u64,
    sets_by_body_part: BTreeMap<String, u64>,
    sets_without_body_part: u64,
    sets_inferred_from_values: u64,
}

#[derive(Default)]
struct Week {
    sessions: u64,
    volume_by_unit: BTreeMap<&'static str, f64>,
    sessions_without_volume: u64,
}

impl Summary {
    fn add_workout(&mut self, workout: &Value, body_parts: &HashMap<u64, BTreeSet<String>>) {
        self.sessions += 1;
        let date = workout["workout_perform_date"].as_str().and_then(|text| {
            chrono::DateTime::parse_from_rfc3339(text)
                .ok()
                .map(|date| date.to_utc().date_naive())
                .or_else(|| NaiveDate::parse_from_str(text.get(..10)?, "%Y-%m-%d").ok())
        });
        if let Some(date) = date {
            let week = self
                .weeks
                .entry(date.week(chrono::Weekday::Mon).first_day())
                .or_default();
            week.sessions += 1;
            let volume = &workout["total_volume"];
            let amount = volume
                .as_f64()
                .or_else(|| volume.as_str()?.trim().parse().ok());
            if let Some(amount) = amount.filter(|amount| amount.is_finite() && *amount >= 0.0) {
                let unit = match workout["weight_unit"].as_str() {
                    Some("kg") => "kg",
                    Some("lb") => "lb",
                    _ => "unspecified",
                };
                *week.volume_by_unit.entry(unit).or_default() += amount;
            } else {
                week.sessions_without_volume += 1;
            }
        } else {
            self.sessions_without_date += 1;
        }

        let exercises = workout["exercises"].as_array().into_iter().flatten();
        let has_completion_flags = exercises
            .clone()
            .flat_map(exercise_sets)
            .any(|set| set["is_completed"] == true);
        for exercise in exercises {
            let mut completed_sets = 0;
            for set in exercise_sets(exercise) {
                match set["is_completed"].as_bool() {
                    Some(true) => completed_sets += 1,
                    Some(false) if has_completion_flags => {}
                    _ if has_logged_values(set) => {
                        completed_sets += 1;
                        self.sets_inferred_from_values += 1;
                    }
                    _ => {}
                }
            }
            if completed_sets == 0 {
                continue;
            }
            let parts = numeric_id(&exercise["exercise_id"]).and_then(|id| body_parts.get(&id));
            if let Some(parts) = parts.filter(|parts| !parts.is_empty()) {
                for part in parts {
                    *self.sets_by_body_part.entry(part.clone()).or_default() += completed_sets;
                }
            } else {
                self.sets_without_body_part += completed_sets;
            }
        }
    }

    fn finish(self, from: &str, to: &str) -> Value {
        let weeks: Vec<Value> = self
            .weeks
            .into_iter()
            .map(|(start, week)| {
                json!({
                    "week_start": start.to_string(),
                    "sessions": week.sessions,
                    "volume_by_unit": week.volume_by_unit,
                    "sessions_without_volume": week.sessions_without_volume,
                })
            })
            .collect();
        json!({
            "from": from,
            "to": to,
            "sessions": self.sessions,
            "sessions_without_date": self.sessions_without_date,
            "weeks": weeks,
            "sets_by_body_part": self.sets_by_body_part,
            "sets_without_body_part": self.sets_without_body_part,
            "sets_inferred_from_values": self.sets_inferred_from_values,
        })
    }
}

fn exercise_sets(exercise: &Value) -> impl Iterator<Item = &Value> {
    exercise["sets"].as_array().into_iter().flatten()
}

fn has_logged_values(set: &Value) -> bool {
    ["reps", "duration", "distance"].into_iter().any(|key| {
        let value = &set[key];
        let numeric = value
            .as_f64()
            .or_else(|| value.as_str()?.trim().parse().ok());
        if numeric.is_some_and(|amount| amount.is_finite() && amount > 0.0) {
            return true;
        }
        key == "duration"
            && value.as_str().is_some_and(|text| {
                let mut parts = text.trim().split(':');
                let count = parts.clone().count();
                (2..=3).contains(&count)
                    && parts.clone().all(|part| part.parse::<u64>().is_ok())
                    && parts.any(|part| part.parse::<u64>().is_ok_and(|part| part > 0))
            })
    })
}

fn numeric_id(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}

fn catalog_body_parts(value: &Value) -> BTreeSet<String> {
    let decoded = value
        .as_str()
        .and_then(|text| serde_json::from_str(text).ok());
    decoded
        .as_ref()
        .unwrap_or(value)
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(numeric_id)
        .map(|id| {
            if (1..=BODY_PARTS.len() as u64).contains(&id) {
                BODY_PARTS[id as usize - 1].to_owned()
            } else {
                format!("Lyfta body part #{id}")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_mixed_units_and_legacy_sets_without_double_counting_tags() {
        let mut report = Summary::default();
        let parts = HashMap::from([(2, catalog_body_parts(&json!("[\"2\",2,6]")))]);
        let workouts = [
            json!({
                "workout_perform_date": "2026-08-23T19:00:00.000Z", "weight_unit": "kg",
                "total_volume": "100.5", "exercises": [
                    {"exercise_id": 2, "sets": [{"reps":"8"}, {"is_completed": false, "reps":"8"}]},
                    {"exercise_id": 99, "sets": [{"is_completed": true}]}
                ]
            }),
            json!({
                "workout_perform_date": "2026-08-24T00:30:00+01:00", "weight_unit": "lb", "total_volume": 50,
                "exercises": [{"exercise_id": "2", "sets": [
                    {"is_completed": false, "reps": "8"},
                    {"is_completed": false, "duration": "00:00:12"},
                    {"is_completed": false, "duration": "00:00:00"}
                ]}]
            }),
            json!({"workout_perform_date": "2026-08-24 18:00:00", "total_volume": 400}),
            json!({"workout_perform_date": "2026-08-24", "total_volume": null}),
            json!({}),
        ];
        for workout in workouts {
            report.add_workout(&workout, &parts);
        }
        let report = report.finish("2026-08-23", "2026-08-24");
        assert_eq!(report["sessions"], 5);
        assert_eq!(report["sessions_without_date"], 1);
        assert_eq!(
            report["weeks"],
            json!([
                {"week_start": "2026-08-17", "sessions": 2, "volume_by_unit": {"kg": 100.5, "lb": 50.0}, "sessions_without_volume": 0},
                {"week_start": "2026-08-24", "sessions": 2, "volume_by_unit": {"unspecified": 400.0}, "sessions_without_volume": 1},
            ])
        );
        assert_eq!(
            report["sets_by_body_part"],
            json!({"Chest": 3, "Shoulders": 3})
        );
        assert_eq!(report["sets_without_body_part"], 1);
        assert_eq!(report["sets_inferred_from_values"], 3);
    }
}
