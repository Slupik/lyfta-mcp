use std::{
    fs,
    sync::{Arc, Mutex},
};

use axum::{
    Router,
    body::to_bytes,
    extract::{Request as HttpRequest, State},
    http::{Method as HttpMethod, StatusCode},
    response::{IntoResponse, Response},
};
use tokio::task::JoinHandle;

use super::*;

#[derive(Default)]
struct Mock {
    collection: Value,
    routines: Vec<Value>,
    posts: Vec<(String, Value)>,
    ambiguous: bool,
    drop_collection_response: bool,
    drop_second_routine_response: bool,
    delay_collection_listing: bool,
    rate_limit_third_routine: bool,
}

struct Fixture {
    server: LyftaServer,
    state: Arc<Mutex<Mock>>,
    directory: PathBuf,
    listener: JoinHandle<()>,
}

impl Fixture {
    async fn new(mock: Mock) -> Self {
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).unwrap();
        let directory = std::env::temp_dir().join(format!(
            "lyfta-program-tests-{}",
            u64::from_ne_bytes(random)
        ));
        let state = Arc::new(Mutex::new(mock));
        let app = Router::new().fallback(mock_lyfta).with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let server = LyftaServer::new(LyftaClient::simulated(origin), directory.clone()).unwrap();
        let listener = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            server,
            state,
            directory,
            listener,
        }
    }

    async fn call(&self, name: &str, args: Value) -> Result<Value, String> {
        call(&self.server, name, args).await
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.listener.abort();
        let _ = fs::remove_dir_all(&self.directory);
    }
}

async fn call(server: &LyftaServer, name: &str, args: Value) -> Result<Value, String> {
    let route = server
        .routes
        .iter()
        .find(|route| route.tool.name == format!("lyfta_{name}"))
        .unwrap();
    route
        .run(
            &server.client,
            &server.programs,
            route.validate(args)?,
            CancellationToken::new(),
        )
        .await
}

async fn mock_lyfta(State(state): State<Arc<Mutex<Mock>>>, request: HttpRequest) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let query = request.uri().query().unwrap_or_default().to_owned();
    let payload = if method == HttpMethod::POST {
        serde_json::from_slice(&to_bytes(request.into_body(), 1_000_000).await.unwrap()).unwrap()
    } else {
        Value::Null
    };
    let mut state = state.lock().unwrap();
    if method == HttpMethod::POST {
        state.posts.push((path.clone(), payload.clone()));
        return match path.as_str() {
            "/api/v1/collections" => {
                state.collection = json!({"id": "101", "title": payload["collection"]["title"]});
                if std::mem::take(&mut state.drop_collection_response) {
                    return (StatusCode::OK, "{").into_response();
                }
                json_response(json!({"status": true, "data": state.collection}))
            }
            "/api/v1/templates" => {
                if state.routines.len() == 2 && std::mem::take(&mut state.rate_limit_third_routine)
                {
                    return StatusCode::TOO_MANY_REQUESTS.into_response();
                }
                let mut workout = payload["workout"].clone();
                workout["id"] = json!(201 + state.routines.len());
                state.routines.push(workout.clone());
                if state.routines.len() == 2
                    && std::mem::take(&mut state.drop_second_routine_response)
                {
                    return (StatusCode::OK, "{").into_response();
                }
                json_response(json!({"status": true, "data": workout}))
            }
            _ => StatusCode::NOT_FOUND.into_response(),
        };
    }

    let response = match path.as_str() {
        "/api/v1/me" => json!({"id": 12345}),
        "/api/v1/exercises/library" => {
            let names: Vec<_> = url_pairs(&query)
                .filter(|(key, _)| key == "search")
                .map(|(_, value)| value)
                .collect();
            let results = match names[0].as_str() {
                "Bench Press" if state.ambiguous => {
                    vec![catalog(2, "Bench Press", 1), catalog(4, "Bench Press", 8)]
                }
                "Bench Press" => vec![catalog(2, "Bench Press", 1)],
                "Dumbbell Bench Press" => vec![catalog(3, "Dumbbell Bench Press", 4)],
                "Full Squat" => vec![catalog(18, "Full Squat", 1)],
                _ => vec![],
            };
            json!({"data":{"results":results,"pagination":{"hasMore":false}}})
        }
        "/api/v1/exercises" => {
            json!({"exercises":[catalog(2,"Bench Press",1),catalog(18,"Full Squat",1)],"total_pages":1})
        }
        "/api/v1/workouts" => {
            let page = url_pairs(&query).find(|(key, _)| key == "page").unwrap().1;
            if page == "1" {
                json!({"total_pages":2,"current_page":1,"workouts":[
                    session(11, "2026-10-05T10:00:00Z", 2, "Bench Press", json!([
                        {"id":"prior","is_completed":true,"weight":"50","reps":"10"}
                    ]))
                ]})
            } else {
                json!({"total_pages":2,"current_page":2,"workouts":[
                    session(12, "2026-11-04T10:00:00Z", 2, "Bench Press", json!([
                        {"id":"one","is_completed":true,"weight":"60","reps":"10","rir":"2","weight_basis":"total"},
                        {"id":"skipped","is_completed":false,"weight":"70","reps":"10"},
                        {"id":"no-reps","is_completed":true,"weight":"80","reps":""}
                    ])),
                    session(13, "2026-11-05T10:00:00Z", 18, "Full Squat", json!([
                        {"id":"squat","is_completed":true,"weight":"100","reps":"8"}
                    ]))
                ]})
            }
        }
        "/api/v1/collections" => {
            let hidden = std::mem::take(&mut state.delay_collection_listing);
            let items = if hidden {
                vec![json!({"id":99,"title":"Block"})]
            } else {
                vec![state.collection.clone()]
            };
            json!({"collections":items,"total_pages":1,"current_page":1})
        }
        "/api/v1/collections/101" => {
            assert_eq!(query, "view=summary");
            json!({"data":{"id":101,"workouts":state.routines}})
        }
        "/api/v1/collections/60" => {
            assert_eq!(query, "view=full");
            json!({"data":{"id":60,"title":"Old Block","description":"Starting point", "workout_ids":[32,31],
                "workouts":[{"id":31,"title":"Bench", "exercises":[{"exercise_id":"2","excercise_name":"Bench Press",
                    "exercise_rest_time":"90","sets":[{"id":"old","set_type_id":"0","reps":8,"weight":"45"}]}]}, {"id":32}]
            }})
        }
        "/api/v1/templates/32" => json!({"data":{"id":32,"title":"Mixed","exercises":[
            {"exercise_id":"18","excercise_name":"Full Squat","sets":[{"reps":"8"}]},
            {"exercise_id":2,"excercise_name":"Bench Press","sets":[{"reps":"8","weight":"50"}]}
        ]}}),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    json_response(response)
}

fn json_response(value: Value) -> Response {
    ([("content-type", "application/json")], value.to_string()).into_response()
}

fn url_pairs(query: &str) -> impl Iterator<Item = (String, String)> + '_ {
    reqwest::Url::parse(&format!("http://localhost/?{query}"))
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect::<Vec<_>>()
        .into_iter()
}

fn catalog(id: u64, name: &str, equipment: u64) -> Value {
    json!({"id":id,"name":name,"image_name":format!("https://cdnlyfta.com/{id}.png"),
        "exercise_type": if equipment == 4 { "db_2_simultaneous" } else { "weight_reps" },
        "equipment_id": format!("[\"{equipment}\"]"),
        "body_part_id": [2], "Target_muscles_id": "[\"25\",999]", "Synergist_muscles_id": ["44"]})
}

fn session(id: u64, date: &str, exercise_id: u64, name: &str, sets: Value) -> Value {
    json!({"id":id,"workout_perform_date":date,"weight_unit":"kg",
        "exercises":[{"exercise_id":exercise_id,"excercise_name":name,"sets":sets}]})
}

fn draft() -> Value {
    json!({
        "collection":{"title":"Block"},
        "days":[
            {"title":"Day A","exercises":[{"name":"Bench Press","sets":[{"reps":10,"weight":"45"},{"reps":10}]}]},
            {"title":"Day B","exercises":[{"name":"Dumbbell Bench Press","sets":[{"reps":12}]}]}
        ]
    })
}

#[tokio::test]
async fn exercise_tools_add_labels_and_keep_lyfta_ids() {
    let fixture = Fixture::new(Mock::default()).await;
    let catalog = fixture
        .call(
            "search_exercise_library",
            json!({"search":"Dumbbell Bench Press"}),
        )
        .await
        .unwrap();
    let exercise = &catalog["data"]["results"][0];
    assert_eq!(exercise["id"], 3);
    assert_eq!(exercise["equipment_id"], "[\"4\"]");
    assert_eq!(exercise["equipment"], json!(["Dumbbell"]));
    assert_eq!(exercise["body_parts"], json!(["Chest"]));
    assert_eq!(
        exercise["target_muscles"],
        json!(["Pectoralis Major Sternal Head", "Lyfta muscle #999"])
    );
    assert_eq!(exercise["synergist_muscles"], json!(["Triceps Brachii"]));

    let performed = fixture
        .call("list_performed_exercises", json!({}))
        .await
        .unwrap();
    assert_eq!(performed["exercises"][0]["equipment"], json!(["Barbell"]));
    assert_eq!(performed["exercises"][0]["body_parts"], json!(["Chest"]));
}

#[tokio::test]
async fn previews_all_days_resolves_ambiguity_and_preserves_entered_loads() {
    let fixture = Fixture::new(Mock {
        ambiguous: true,
        ..Mock::default()
    })
    .await;
    let mut plan = draft();
    let result = fixture.call("preview_program", plan.clone()).await.unwrap();
    assert_eq!(result["ready"], false);
    assert!(result.get("preview_id").is_none());
    assert_eq!(result["days"].as_array().unwrap().len(), 2);
    let ambiguous = &result["days"][0]["exercise_matches"][0];
    assert_eq!(ambiguous["status"], "ambiguous");
    assert_eq!(
        ambiguous["candidates"][1]["equipment"],
        json!(["Smith machine"])
    );

    plan["days"][0]["exercises"][0]["exercise_id"] = json!(2);
    plan["use_history"] = json!(true);
    plan["weight_unit"] = json!("kg");
    let result = fixture.call("preview_program", plan).await.unwrap();
    assert_eq!(result["ready"], true);
    let day = &result["days"][0];
    assert_eq!(day["workout"]["exercises"][0]["sets"][0]["weight"], "45");
    assert_eq!(day["workout"]["exercises"][0]["sets"][1]["weight"], "60");
    assert_eq!(
        day["set_loads"][0][1]["source_set"]["date"],
        "2026-11-04T10:00:00Z"
    );
    assert_eq!(day["set_loads"][0][1]["source_set"]["reps"], 10);
    assert_eq!(
        result["days"][1]["set_loads"][0][0]["kind"],
        "no_same_exercise_history"
    );
    assert!(fixture.state.lock().unwrap().posts.is_empty());
}

#[tokio::test]
async fn shows_sources_and_requires_a_selected_estimate_before_save() {
    let fixture = Fixture::new(Mock::default()).await;
    let evidence = fixture
        .call(
            "get_comparable_sets",
            json!({
                "target_exercise_id":3,"comparable_exercise_ids":[2,3]
            }),
        )
        .await
        .unwrap();
    assert_eq!(evidence["status"], "evidence_available");
    let sets = evidence["source_sets"].as_array().unwrap();
    assert_eq!(sets.len(), 2);
    let source = &sets[0];
    assert_eq!(source["name"], "Bench Press");
    assert_eq!(source["equipment"], json!(["Barbell"]));
    assert_eq!(source["synergist_muscles"], json!(["Triceps Brachii"]));
    assert_eq!(source["weight"], 60.0);
    assert_eq!(source["unit"], "kg");
    assert_eq!(source["reps"], 10);
    assert_eq!(source["rir"], 2.0);
    assert_eq!(source["load_basis"], "total");

    let mut plan = draft();
    plan["weight_unit"] = json!("kg");
    let estimate = json!({
        "lower":14,"upper":18,"unit":"kg","load_basis":"per_hand",
        "reason":"Start conservatively for 12 reps with independent dumbbells and greater stability demands.",
        "source_refs":[source["ref"]]
    });
    let estimate_path = "/days/1/exercises/0/sets/0";
    plan.pointer_mut(estimate_path).unwrap()["estimate"] = estimate;
    let result = fixture.call("preview_program", plan.clone()).await.unwrap();
    assert_eq!(result["ready"], false);
    assert_eq!(
        result["days"][1]["set_loads"][0][0]["status"],
        "choose_weight"
    );
    plan.pointer_mut(estimate_path).unwrap()["estimate"]["selected_weight"] = json!(16);
    let result = fixture.call("preview_program", plan.clone()).await.unwrap();
    let day = &result["days"][1];
    assert_eq!(result["ready"], true);
    assert_eq!(day["set_loads"][0][0]["kind"], "estimate");
    assert_eq!(day["set_loads"][0][0]["proposal"]["load_basis"], "per_hand");
    assert_eq!(day["set_loads"][0][0]["source_sets"], json!([source]));
    assert_eq!(
        day["workout"]["exercises"][0]["sets"][0],
        json!({"reps":"12","weight":"16"})
    );
    assert!(fixture.state.lock().unwrap().posts.is_empty());

    plan.pointer_mut(estimate_path).unwrap()["estimate"]["source_refs"] = json!(["12/2/skipped"]);
    let result = fixture.call("preview_program", plan).await.unwrap();
    assert_eq!(result["ready"], false);
    assert_eq!(
        result["days"][1]["set_loads"][0][0]["status"],
        "source_unavailable"
    );
    assert!(result["days"][1]["workout"]["exercises"][0]["sets"][0]["weight"].is_null());
}

#[tokio::test]
async fn no_comparable_sets_leaves_load_blank_and_can_save() {
    let fixture = Fixture::new(Mock::default()).await;
    let evidence = fixture
        .call(
            "get_comparable_sets",
            json!({
                "target_exercise_id":3,"comparable_exercise_ids":[3,88]
            }),
        )
        .await
        .unwrap();
    assert_eq!(evidence["status"], "no_comparable_sets");
    assert_eq!(evidence["source_sets"], json!([]));
    let result = fixture.call("preview_program", draft()).await.unwrap();
    assert_eq!(result["ready"], true);
    assert!(result["days"][1]["workout"]["exercises"][0]["sets"][0]["weight"].is_null());
    fixture
        .call(
            "save_program",
            json!({"preview_id":result["preview_id"],"approved":true}),
        )
        .await
        .unwrap();
    assert!(
        fixture.state.lock().unwrap().routines[1]["exercises"][0]["sets"][0]
            .get("weight")
            .is_none()
    );
}

#[tokio::test]
async fn resumes_after_lost_collection_and_routine_responses_without_duplicates() {
    let fixture = Fixture::new(Mock {
        drop_collection_response: true,
        drop_second_routine_response: true,
        delay_collection_listing: true,
        rate_limit_third_routine: true,
        ..Mock::default()
    })
    .await;
    let mut plan = draft();
    let extra_day = plan["days"][0].clone(); // Same title still identifies a distinct routine.
    plan["days"].as_array_mut().unwrap().push(extra_day);
    let result = fixture.call("preview_program", plan).await.unwrap();
    let id = &result["preview_id"];
    let resume = json!({"preview_id":id});
    assert!(
        fixture
            .call("resume_program_save", resume.clone())
            .await
            .is_err()
    );
    assert!(
        fixture
            .call("save_program", json!({"preview_id":id,"approved":false}))
            .await
            .is_err()
    );
    assert!(fixture.state.lock().unwrap().posts.is_empty());
    let approved = json!({"preview_id":id,"approved":true});
    let first = fixture
        .call("save_program", approved.clone())
        .await
        .unwrap();
    assert_eq!(first["status"], "awaiting_reconciliation");
    assert!(first["collection_id"].is_null());

    let reopened =
        LyftaServer::new(fixture.server.client.clone(), fixture.directory.clone()).unwrap();
    let delayed = call(&reopened, "resume_program_save", resume.clone())
        .await
        .unwrap();
    assert_eq!(delayed["status"], "awaiting_reconciliation");
    assert_eq!(fixture.state.lock().unwrap().posts.len(), 1);
    let partial = call(&reopened, "resume_program_save", resume.clone())
        .await
        .unwrap();
    assert_eq!(partial["status"], "awaiting_reconciliation");
    assert_eq!(partial["collection_id"], 101);
    assert_eq!(partial["saved_routines"][0]["id"], 201);
    assert_eq!(partial["saved_routines"].as_array().unwrap().len(), 1);

    let rejected = call(&reopened, "resume_program_save", resume.clone())
        .await
        .unwrap();
    assert_eq!(rejected["status"], "interrupted");
    assert_eq!(rejected["saved_routines"][1]["id"], 202);
    let saved = call(&reopened, "resume_program_save", resume)
        .await
        .unwrap();
    assert_eq!(saved["status"], "saved");
    assert_eq!(saved["saved_routines"][2]["id"], 203);
    assert_eq!(fixture.call("save_program", approved).await.unwrap(), saved);
    let state = fixture.state.lock().unwrap();
    assert_eq!(state.posts.len(), 5); // One collection, three routines and one rejected attempt.
    assert_eq!(
        state.posts[0].1["collection"]["title"],
        result["saved_collection_title"]
    );
    assert_eq!(state.routines[2]["title"], result["days"][2]["saved_title"]);
}

#[tokio::test]
async fn loads_a_collection_and_saves_the_edited_block_as_new() {
    let fixture = Fixture::new(Mock::default()).await;
    let loaded = fixture
        .call(
            "load_program_draft",
            json!({"collection_id":60,"title":"Next Block"}),
        )
        .await
        .unwrap();
    let mut plan = loaded["draft"].clone();
    assert_eq!(plan["days"][0]["title"], "Mixed");
    assert_eq!(plan["days"][1]["title"], "Bench");
    assert_eq!(plan["days"][1]["exercises"][0]["exercise_rest_time"], 90);
    assert_eq!(
        plan["days"][1]["exercises"][0]["sets"][0],
        json!({"set_type_id":0,"reps":"8","weight":"45"})
    );
    let exercises = plan["days"][0]["exercises"].as_array_mut().unwrap();
    exercises.swap(0, 1);
    exercises[1] = json!({"name":"Dumbbell Bench Press","sets":[{"reps":12},{"reps":10}]});
    let reviewed = fixture.call("preview_program", plan).await.unwrap();
    assert_eq!(reviewed["ready"], true);
    assert!(fixture.state.lock().unwrap().posts.is_empty());
    fixture
        .call(
            "save_program",
            json!({"preview_id":reviewed["preview_id"],"approved":true}),
        )
        .await
        .unwrap();
    let state = fixture.state.lock().unwrap();
    assert_eq!(state.posts[1].1["collectionId"], 101);
    assert_eq!(
        state.posts[1].1["workout"]["exercises"][0]["exercise_id"],
        2
    );
    assert_eq!(
        state.posts[1].1["workout"]["exercises"][1]["exercise_id"],
        3
    );
    assert_eq!(
        state.posts[1].1["workout"]["exercises"][1]["sets"][1]["reps"],
        "10"
    );
}
