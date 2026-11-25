use std::{error::Error, path::PathBuf, sync::Arc};

use base64::{Engine, engine::general_purpose::STANDARD};
use jsonschema::Validator;
use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorCode,
        Implementation, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig,
        Tool, ToolAnnotations,
    },
    service::RequestContext,
};
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use crate::{
    exercise_metadata,
    lyfta::{LyftaClient, Request},
    program_store::ProgramStore,
};

pub const MAX_SAFE_INT: u64 = 9_007_199_254_740_991;

enum Method {
    Sample,
    List,
    GetById,
    Create,
    TrainingSummary,
    PreviewTemplate,
    PreviewProgram,
    LoadProgramDraft,
    ComparableSets,
    SaveProgram,
    ResumeProgramSave,
}

struct Route {
    tool: Tool,
    method: Method,
    path: &'static str,
    validator: Validator,
}

impl Route {
    fn new(
        name: &'static str,
        description: &'static str,
        method: Method,
        path: &'static str,
        schema: Value,
    ) -> Result<Self, Box<dyn Error>> {
        let validator = jsonschema::options()
            .should_validate_formats(true)
            .with_format("base64", |text| STANDARD.decode(text).is_ok())
            .build(&schema)
            .map_err(|err| err.to_string())?;
        let Value::Object(schema) = schema else {
            return Err("Expected a tool schema object.".into());
        };
        let writes = matches!(
            method,
            Method::Create | Method::SaveProgram | Method::ResumeProgramSave
        );
        let mut annotations = ToolAnnotations::default()
            .read_only(!writes)
            .open_world(true);
        if writes {
            annotations = annotations
                .destructive(false)
                .idempotent(!matches!(method, Method::Create));
        }
        let tool =
            Tool::new(format!("lyfta_{name}"), description, schema).with_annotations(annotations);
        Ok(Self {
            tool,
            method,
            path,
            validator,
        })
    }

    fn validate(&self, mut args: Value) -> Result<Map<String, Value>, String> {
        for pointer in ["/search", "/collection/title"] {
            if let Some(value) = args.pointer_mut(pointer)
                && let Some(text) = value.as_str()
            {
                *value = Value::String(text.trim().to_owned());
            }
        }

        self.validator
            .validate(&args)
            .map_err(|error| format!("Invalid tool arguments: {error}"))?;

        if let (Some(from), Some(to)) = (args["from"].as_str(), args["to"].as_str())
            && from > to
        {
            return Err("The end date must be on or after the start date.".into());
        }
        let Value::Object(mut args) = args else {
            return Err("Tool arguments must be an object.".into());
        };
        if let Some(properties) = self.tool.input_schema["properties"].as_object() {
            for (name, value) in properties {
                if let Some(default) = value.get("default") {
                    args.entry(name.clone()).or_insert_with(|| default.clone());
                }
            }
        }

        Ok(args)
    }

    async fn run(
        &self,
        client: &LyftaClient,
        programs: &ProgramStore,
        mut args: Map<String, Value>,
        cancellation: CancellationToken,
    ) -> Result<Value, String> {
        let mut path = self.path.to_owned();
        let request = match self.method {
            Method::Sample => Request::Sample,
            Method::Create => Request::Create(args),
            Method::List | Method::GetById => {
                if matches!(self.method, Method::GetById) {
                    let id = args.remove("id").ok_or("Missing required ID.")?;
                    path.push('/');
                    path.push_str(&query_value(&id));
                }
                let query = args
                    .into_iter()
                    .map(|(name, value)| (name, query_value(&value)))
                    .collect();
                Request::Read(query)
            }
            Method::TrainingSummary => {
                return crate::summary::training_summary(
                    client,
                    args["from"].as_str().expect("validated start date"),
                    args["to"].as_str().expect("validated end date"),
                    cancellation,
                )
                .await;
            }
            Method::PreviewTemplate => {
                return crate::preview::preview_template(client, args, cancellation).await;
            }
            Method::PreviewProgram => {
                return crate::program::preview(client, programs, args, &cancellation).await;
            }
            Method::LoadProgramDraft => {
                return crate::program::load_draft(client, args, &cancellation).await;
            }
            Method::ComparableSets => {
                return crate::history::comparable_sets(client, args, &cancellation).await;
            }
            Method::SaveProgram | Method::ResumeProgramSave => {
                return programs
                    .save(
                        client,
                        args["preview_id"].as_str().unwrap(),
                        matches!(self.method, Method::SaveProgram),
                        &cancellation,
                    )
                    .await;
            }
        };
        let mut result = client.request(&path, request, cancellation).await?;
        let exercises = match self.path {
            "/api/v1/exercises" => result.pointer_mut("/exercises"),
            "/api/v1/exercises/library" => result.pointer_mut("/data/results"),
            _ => None,
        };
        if let Some(exercises) = exercises.and_then(Value::as_array_mut) {
            for exercise in exercises {
                let labels = exercise_metadata::labels(exercise, &Value::Null);
                if let Some(fields) = exercise.as_object_mut() {
                    fields.extend(labels);
                }
            }
        }
        Ok(result)
    }
}

fn query_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) if number.is_f64() => number
            .as_f64()
            .expect("JSON number is representable")
            .to_string(),
        _ => value.to_string(),
    }
}

#[derive(Clone)]
pub struct LyftaServer {
    client: LyftaClient,
    routes: Arc<Vec<Route>>,
    programs: ProgramStore,
}

impl LyftaServer {
    pub fn new(client: LyftaClient, state_directory: PathBuf) -> Result<Self, Box<dyn Error>> {
        use Method::{Create, GetById, List, Sample};

        let routes = vec![
            Route::new(
                "sample_workouts",
                "Read Lyfta’s fixed public sample workouts. No API key required.",
                Sample,
                "/api/sandbox/v1/workouts",
                object(json!({}), &[]),
            )?,
            Route::new(
                "list_workouts",
                "List completed workouts with exercises, sets, reps, and weights. Use limit=1 for the latest workout.",
                List,
                "/api/v1/workouts",
                object(with_dates(pagination(100, 10)), &[]),
            )?,
            Route::new(
                "list_workout_summaries",
                "List completed workout headers with dates, duration, and volume, ordered by workout ID descending.",
                List,
                "/api/v1/workouts/summary",
                object(Value::Object(pagination(1000, 20)), &[]),
            )?,
            Route::new(
                "get_training_summary",
                "Summarize completed training for an inclusive UTC date range. Returns session counts, weekly lifted volume by unit, and completed sets by body part.",
                Method::TrainingSummary,
                "",
                object(with_dates(Map::new()), &["from", "to"]),
            )?,
            Route::new(
                "list_performed_exercises",
                "List exercises you have logged with catalog IDs and Lyfta equipment, body part and muscle labels.",
                List,
                "/api/v1/exercises",
                object(Value::Object(pagination(100, 20)), &[]),
            )?,
            Route::new(
                "search_exercise_library",
                "Search Lyfta’s exercise catalog by name and see equipment, body part and muscle labels.",
                List,
                "/api/v1/exercises/library",
                object(
                    json!({
                        "search": {"type": "string", "minLength": 1},
                        "limit": {"type": "integer", "minimum": 1, "maximum": 100, "default": 20},
                        "offset": {"type": "integer", "minimum": 0, "maximum": MAX_SAFE_INT, "default": 0, "description": "Number of results to skip"}
                    }),
                    &["search"],
                ),
            )?,
            Route::new(
                "get_exercise_progress",
                "Get best-set progress and estimated one-rep max over a lookback period.",
                List,
                "/api/v1/exercises/progress",
                object(
                    json!({
                        "exercise_id": id("ID from the performed exercise list"),
                        "duration": {"type": "integer", "minimum": 1, "maximum": MAX_SAFE_INT, "description": "Lookback period in days"}
                    }),
                    &["exercise_id", "duration"],
                ),
            )?,
            Route::new(
                "list_schedule",
                "List scheduled workouts. By default Lyfta returns incomplete sessions for the next 90 days.",
                List,
                "/api/v1/schedule",
                object(
                    with_dates(Map::from_iter([(
                        "include_completed".into(),
                        json!({"type": "boolean"}),
                    )])),
                    &[],
                ),
            )?,
            Route::new(
                "list_collections",
                "List your saved programs and collections.",
                List,
                "/api/v1/collections",
                object(Value::Object(pagination(100, 20)), &[]),
            )?,
            Route::new(
                "get_collection",
                "Read a saved collection. The full view includes complete workout templates.",
                GetById,
                "/api/v1/collections",
                object(
                    json!({
                        "id": id("Collection ID"),
                        "view": {"type": "string", "enum": ["summary", "full"], "default": "summary"}
                    }),
                    &["id"],
                ),
            )?,
            Route::new(
                "list_templates",
                "List your saved workout templates and their IDs.",
                List,
                "/api/v1/templates",
                object(Value::Object(pagination(100, 20)), &[]),
            )?,
            Route::new(
                "get_template",
                "Read a saved workout template including exercises and sets.",
                GetById,
                "/api/v1/templates",
                object(json!({"id": id("Template ID")}), &["id"]),
            )?,
            Route::new(
                "preview_program",
                "Preview all days in a training program. Choose catalog candidates and resolve estimated weight ranges, then preview again. Present all days and their load sources to the user. Save this preview ID only after the user explicitly approves it.",
                Method::PreviewProgram,
                "",
                program_schema(),
            )?,
            Route::new(
                "load_program_draft",
                "Read an existing Lyfta collection into an editable draft for lyfta_preview_program. Change exercises, order or set parameters in the returned draft, then preview it and get explicit approval before saving as a new program.",
                Method::LoadProgramDraft,
                "",
                object(
                    json!({
                        "collection_id": id("Existing collection ID"),
                        "title": {"type": "string", "pattern": "\\S", "description": "Title for the new program"}
                    }),
                    &["collection_id", "title"],
                ),
            )?,
            Route::new(
                "get_comparable_sets",
                "Read recent completed weighted sets for exercises you judge biomechanically comparable to a new target. Choose source IDs from the user's performed exercises. Reference actual source sets in an estimate. If the sets are unsuitable or unavailable, leave the weight blank for a light trial set.",
                Method::ComparableSets,
                "",
                object(
                    json!({
                        "target_exercise_id": catalog_id("Catalog ID of the new exercise"),
                        "comparable_exercise_ids": {
                            "type":"array", "uniqueItems":true, "maxItems":20,
                            "items": catalog_id("ID of a comparable performed exercise")
                        },
                        "sets_per_exercise": {"type":"integer", "minimum":1, "maximum":20, "default":5},
                        "from": {"type":"string", "format":"date", "description":"Optional inclusive UTC start date"}
                    }),
                    &["target_exercise_id", "comparable_exercise_ids"],
                ),
            )?,
            Route::new(
                "save_program",
                "Create the program and routines from an immutable ready preview. Call only after explicit user approval of that exact preview. Returns IDs and a resume tool if interrupted.",
                Method::SaveProgram,
                "",
                save_schema(true),
            )?,
            Route::new(
                "resume_program_save",
                "Continue a previously approved program save with the same preview ID. Reconcile uncertain writes with the Lyfta library before continuing. Returns the collection ID and previously saved routine IDs.",
                Method::ResumeProgramSave,
                "",
                save_schema(false),
            )?,
            Route::new(
                "create_collection",
                "Create a new training program in your Lyfta library and return its collection ID. Each successful call adds a collection.",
                Create,
                "/api/v1/collections",
                collection_schema(),
            )?,
            Route::new(
                "preview_template",
                "Preview a routine using catalog exercise names. Review exact matches or choose from suggested exercises. When ready, use create_template_arguments with lyfta_create_template after user approval.",
                Method::PreviewTemplate,
                "",
                template_schema(true),
            )?,
            Route::new(
                "create_template",
                "Create a new workout template and append it to the specified collection. For a revised routine, use a new title or collection. Each successful call adds a template.",
                Create,
                "/api/v1/templates",
                template_schema(false),
            )?,
        ];
        Ok(Self {
            client,
            routes: Arc::new(routes),
            programs: ProgramStore::new(state_directory),
        })
    }
}

impl ServerHandler for LyftaServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")),
        )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(
            self.routes.iter().map(|route| route.tool.clone()).collect(),
        ))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let route = self
            .routes
            .iter()
            .find(|route| route.tool.name == request.name)
            .ok_or_else(|| {
                ErrorData::new(ErrorCode::METHOD_NOT_FOUND, "Unknown Lyfta tool.", None)
            })?;
        let result = match route.validate(Value::Object(request.arguments.unwrap_or_default())) {
            Ok(args) => {
                route
                    .run(&self.client, &self.programs, args, context.ct)
                    .await
            }
            Err(error) => Err(error),
        };
        Ok(match result {
            Ok(data) => CallToolResult::structured(data).into(),
            Err(error) => CallToolResult::error(vec![ContentBlock::text(error)]).into(),
        })
    }
}

fn object(properties: Value, required: &[&str]) -> Value {
    let mut schema = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": properties,
    });
    if !required.is_empty() {
        schema["required"] = json!(required);
    }
    schema
}

fn pagination(max: u64, default_limit: u64) -> Map<String, Value> {
    Map::from_iter([
        (
            "limit".into(),
            json!({"type": "integer", "minimum": 1, "maximum": max, "default": default_limit, "description": "Results per page"}),
        ),
        (
            "page".into(),
            json!({"type": "integer", "minimum": 1, "maximum": MAX_SAFE_INT, "default": 1, "description": "Page number, starting at 1"}),
        ),
    ])
}

fn with_dates(mut properties: Map<String, Value>) -> Value {
    properties.insert("from".into(), json!({"type": "string", "format": "date", "description": "Inclusive UTC start date (YYYY-MM-DD)"}));
    properties.insert("to".into(), json!({"type": "string", "format": "date", "description": "Inclusive UTC end date (YYYY-MM-DD)"}));
    Value::Object(properties)
}

fn id(description: &str) -> Value {
    json!({
        "description": description,
        "anyOf": [
            {"type": "integer", "minimum": 1, "maximum": MAX_SAFE_INT},
            {"type": "string", "pattern": "^[1-9][0-9]*$"}
        ]
    })
}

fn collection_schema() -> Value {
    let collection = object(
        json!({
            "title": {"type": "string", "minLength": 1},
            "description": {"type": "string"},
            "goal": {"type": "string"},
            "image": {"type": "string", "format": "base64", "minLength": 1, "description": "Base64-encoded cover image"}
        }),
        &["title"],
    );
    object(json!({"collection": collection}), &["collection"])
}

pub fn template_schema(preview: bool) -> Value {
    let set = object(
        json!({
            "set_type_id": {"type": "integer", "minimum": 0, "maximum": MAX_SAFE_INT, "description": "0 for a normal set"},
            "reps": {"type": "string"},
            "from_reps": {"type": "string"},
            "to_reps": {"type": "string"},
            "weight": {"type": "string"},
            "rir": {"type": "string", "description": "Reps in reserve"},
            "duration": {"type": "string"},
            "distance": {"type": "string"}
        }),
        &[],
    );
    let mut exercise_properties = json!({
        "exercise_id": {"type": "integer", "minimum": 1, "maximum": MAX_SAFE_INT, "description": "Catalog ID. In a preview, include it to select among matches."},
        "exercise_note": {"type": "string"},
        "exercise_rest_time": {"type": "integer", "minimum": 0, "maximum": MAX_SAFE_INT, "description": "Rest time in seconds"},
        "exercise_superset_id": {"type": "integer", "minimum": 0, "maximum": MAX_SAFE_INT, "description": "Use the same positive ID for exercises in one superset"},
        "is_rep_range_active": {"type": "boolean"},
        "sets": {"type": "array", "items": set}
    });
    let exercise = if preview {
        exercise_properties["name"] = json!({"type": "string", "pattern": "\\S", "description": "Exercise name as it appears in Lyfta, or a search term to find candidates"});
        object(exercise_properties, &["name"])
    } else {
        exercise_properties["excercise_name"] = json!({"type": "string", "minLength": 1, "description": "Exact catalog name. Spelling of the field is required by Lyfta."});
        exercise_properties["exercise_type"] =
            json!({"type": "string", "description": "Exact catalog exercise_type"});
        exercise_properties["exercise_image"] =
            json!({"type": "string", "description": "Exact catalog image_name"});
        object(
            exercise_properties,
            &[
                "exercise_id",
                "excercise_name",
                "exercise_type",
                "exercise_image",
            ],
        )
    };
    let mut workout_properties = json!({
        "title": {"type": "string"},
        "description": {"type": "string"},
        "note": {"type": "string"},
        "color": {"type": "string", "pattern": "^#[0-9a-fA-F]{6}$"},
        "picture": {"type": "string", "description": "Cover image as a base64 image data URI or existing Lyfta CDN URL"},
        "exercises": {"type": "array", "items": exercise, "description": "Exercise order is preserved."}
    });
    let workout = if preview {
        workout_properties["exercises"]["minItems"] = json!(1);
        object(workout_properties, &["exercises"])
    } else {
        object(workout_properties, &[])
    };
    object(
        json!({
            "collectionId": {"type": "integer", "minimum": 1, "maximum": MAX_SAFE_INT, "description": "ID of an existing collection in your library"},
            "workout": workout
        }),
        &["collectionId", "workout"],
    )
}

fn catalog_id(description: &str) -> Value {
    json!({"type": "integer", "minimum": 1, "maximum": MAX_SAFE_INT, "description": description})
}

fn save_schema(approval: bool) -> Value {
    let mut properties = json!({"preview_id": {"type": "string", "pattern": "^[a-f0-9]{24}$"}});
    if approval {
        properties["approved"] = json!({"type": "boolean", "const": true, "description": "The user has explicitly approved this exact preview."});
        object(properties, &["preview_id", "approved"])
    } else {
        object(properties, &["preview_id"])
    }
}

fn program_schema() -> Value {
    let mut collection = collection_schema()["properties"]["collection"].clone();
    collection["properties"]["title"]["pattern"] = json!("\\S");
    let mut day = template_schema(true)["properties"]["workout"].clone();
    day["required"] = json!(["title", "exercises"]);
    day["properties"]["exercises"]
        .as_object_mut()
        .unwrap()
        .remove("minItems");
    day["properties"]["title"]["pattern"] = json!("\\S");
    let sets = &mut day["properties"]["exercises"]["items"]["properties"]["sets"];
    let set = &mut sets["items"]["properties"];
    for key in ["reps", "from_reps", "to_reps"] {
        set[key] = json!({"anyOf": [{"type": "integer", "minimum": 0}, {"type": "string"}]});
    }
    set["weight"] = json!({"anyOf": [
        {"type": "number", "minimum": 0}, {"type":"null"},
        {"type": "string", "pattern": "^\\s*(?:[0-9]+(?:\\.[0-9]+)?)?\\s*$"}
    ]});
    let basis = json!({"enum": ["per_hand", "total", "unknown"]});
    set["load_basis"] = basis.clone();
    let load = json!({"type":"number", "exclusiveMinimum":0});
    set["estimate"] = object(
        json!({
            "unit": {"enum": ["kg", "lb"]},
            "load_basis": basis,
            "weight": load,
            "lower": load,
            "upper": load,
            "selected_weight": load,
            "reason": {"type":"string", "pattern":"\\S", "description":"Explain how the source movements, equipment, reps, and RIR inform this initial estimate."},
            "source_refs": {"type":"array", "minItems":1, "maxItems":10, "uniqueItems":true, "items":{"type":"string", "minLength":1}}
        }),
        &["unit", "load_basis", "reason", "source_refs"],
    );
    set["estimate"]["oneOf"] = json!([
        {"required":["weight"], "not":{"anyOf":[{"required":["lower"]}, {"required":["upper"]}, {"required":["selected_weight"]}]}},
        {"required":["lower","upper"], "not":{"required":["weight"]}}
    ]);
    object(
        json!({
            "collection": collection,
            "days": {"type":"array", "minItems":1, "items":day, "description":"Routine days in order. Use one set object per planned set."},
            "weight_unit": {"enum":["kg","lb"], "description":"Unit used by Lyfta on the destination account. Required for weight estimates and automatic history."},
            "use_history": {"type":"boolean", "default":false, "description":"Fill blank loads from the latest completed sets of the same catalog exercise."},
            "history_from": {"type":"string", "format":"date", "description":"Optional inclusive UTC start date for history and estimate sources."}
        }),
        &["collection", "days"],
    )
}

#[cfg(test)]
#[path = "program_tests.rs"]
mod program_tests;
