use std::{
    fmt::Write as _,
    fs::{self, File, OpenOptions},
    io::{self, Write as _},
    path::{Path, PathBuf},
};

use fs4::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::lyfta::{LyftaClient, Request, numeric_id};

const COLLECTIONS: &str = "/api/v1/collections";
const TEMPLATES: &str = "/api/v1/templates";
const MARKER_PREFIX: &str = " [MCP ";

#[derive(Clone)]
pub struct ProgramStore {
    directory: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct Journal {
    account_id: u64,
    approved: bool,
    collection: Value,
    workouts: Vec<Value>,
    steps: Vec<Step>,
}

#[derive(Serialize, Deserialize, Clone, Copy)]
enum Step {
    Ready,
    InFlight,
    Saved(u64),
}

impl Step {
    fn id(self) -> Option<u64> {
        match self {
            Self::Saved(id) => Some(id),
            _ => None,
        }
    }
}

impl ProgramStore {
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    pub fn prepare(
        &self,
        account_id: u64,
        mut collection: Value,
        mut workouts: Vec<Value>,
    ) -> Result<(String, String, Vec<String>), String> {
        let mut bytes = [0u8; 12];
        getrandom::fill(&mut bytes)
            .map_err(|err| format!("Could not generate a preview ID: {err}"))?;
        let mut id = String::new();
        for byte in bytes {
            write!(id, "{byte:02x}").unwrap();
        }
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&self.directory).map_err(storage_error)?;
        let path = self.directory.join(&id);
        fs::create_dir(&path).map_err(storage_error)?;
        sync_directory(&self.directory).map_err(storage_error)?;
        let collection_title = add_marker(&mut collection, &id, 0);
        let workout_titles = workouts
            .iter_mut()
            .enumerate()
            .map(|(index, workout)| add_marker(workout, &id, index + 1))
            .collect();
        let journal = Journal {
            account_id,
            approved: false,
            collection,
            steps: vec![Step::Ready; workouts.len() + 1],
            workouts,
        };
        persist(&path, &journal).map_err(storage_error)?;
        Ok((id, collection_title, workout_titles))
    }

    pub async fn save(
        &self,
        client: &LyftaClient,
        id: &str,
        approve: bool,
        cancellation: &CancellationToken,
    ) -> Result<Value, String> {
        if !valid_id(id) {
            return Err("Invalid program preview ID.".into());
        }
        let path = self.directory.join(id);
        let lock = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.join("lock"))
            .map_err(|err| {
                if err.kind() == io::ErrorKind::NotFound {
                    "Program preview not found.".into()
                } else {
                    storage_error(err)
                }
            })?;
        match lock.try_lock_exclusive() {
            Ok(()) => {}
            Err(error) if error.raw_os_error() == fs4::lock_contended_error().raw_os_error() => {
                return Ok(json!({"status": "save_in_progress", "preview_id": id}));
            }
            Err(error) => return Err(storage_error(error)),
        }
        // The OS releases this lock if an MCP call or the entire process is interrupted.
        let mut journal: Journal =
            serde_json::from_reader(File::open(path.join("journal.json")).map_err(storage_error)?)
                .map_err(storage_error)?;
        if journal.account_id != client.account_id(cancellation).await? {
            return Err("This preview belongs to another Lyfta account.".into());
        }
        if !journal.approved {
            if !approve {
                return Err("Approve the program preview before saving it.".into());
            }
            journal.approved = true;
            persist(&path, &journal).map_err(storage_error)?;
        }

        for index in 0..journal.steps.len() {
            let next = match journal.steps[index] {
                Step::Saved(_) => continue,
                Step::InFlight => {
                    match recover(client, &journal, index, cancellation).await {
                        Ok(Some(id)) => Ok(id),
                        Ok(None) => return Ok(journal.report(id, "awaiting_reconciliation", Some(index),
                            Some("Lyfta has not returned the item in its library yet. Resume with this preview ID to check again."))),
                        Err(error) => return Ok(journal.report(id, "awaiting_reconciliation", Some(index), Some(&error))),
                    }
                }
                Step::Ready => {
                    if cancellation.is_cancelled() {
                        return Ok(journal.report(id, "interrupted", Some(index), Some("The save was cancelled.")));
                    }
                    let (endpoint, payload) = journal.request(index);
                    journal.steps[index] = Step::InFlight;
                    if let Err(error) = persist(&path, &journal) {
                        return Ok(journal.report(id, "interrupted", Some(index), Some(&storage_error(error))));
                    }
                    match client.exchange(endpoint, Request::Create(payload.as_object().unwrap().clone()), cancellation.clone()).await {
                        Ok(response) => numeric_id(&response["data"]["id"]).filter(|id| *id > 0)
                            .ok_or_else(|| "Lyfta returned an invalid ID for the new item.".into()),
                        Err(error) if error.rejected => {
                            journal.steps[index] = Step::Ready;
                            let message = match persist(&path, &journal) {
                                Ok(()) => error.message,
                                Err(error) => storage_error(error),
                            };
                            return Ok(journal.report(id, "interrupted", Some(index), Some(&message)));
                        }
                        Err(error) => Err(error.message),
                    }
                }
            };
            match next {
                Ok(saved_id) => {
                    journal.steps[index] = Step::Saved(saved_id);
                    if let Err(error) = persist(&path, &journal) {
                        return Ok(journal.report(
                            id,
                            "interrupted",
                            Some(index),
                            Some(&storage_error(error)),
                        ));
                    }
                }
                Err(error) => {
                    return Ok(journal.report(
                        id,
                        "awaiting_reconciliation",
                        Some(index),
                        Some(&error),
                    ));
                }
            }
        }
        Ok(journal.report(id, "saved", None, None))
    }
}

impl Journal {
    fn request(&self, index: usize) -> (&'static str, Value) {
        if index == 0 {
            (COLLECTIONS, json!({"collection": self.collection}))
        } else {
            (
                TEMPLATES,
                json!({"collectionId": self.steps[0].id().unwrap(), "workout": self.workouts[index - 1]}),
            )
        }
    }

    fn report(&self, id: &str, status: &str, pending: Option<usize>, error: Option<&str>) -> Value {
        let routines: Vec<_> = self
            .workouts
            .iter()
            .zip(&self.steps[1..])
            .enumerate()
            .filter_map(|(index, (workout, step))| {
                Some(json!({"day_index": index, "title": workout["title"], "id": step.id()?}))
            })
            .collect();
        let mut report = json!({
            "status": status, "preview_id": id, "collection_id": self.steps[0].id(), "saved_routines": routines,
        });
        if let Some(pending) = pending {
            report["pending_item"] = if pending == 0 {
                json!({"kind": "collection", "title": self.collection["title"]})
            } else {
                json!({"kind": "routine", "day_index": pending - 1, "title": self.workouts[pending - 1]["title"]})
            };
            report["resume_tool"] = json!("lyfta_resume_program_save");
        }
        if let Some(error) = error {
            report["message"] = json!(error);
        }
        report
    }
}

async fn recover(
    client: &LyftaClient,
    journal: &Journal,
    index: usize,
    cancellation: &CancellationToken,
) -> Result<Option<u64>, String> {
    if index > 0 {
        let collection_id = journal.steps[0].id().unwrap();
        let response = client
            .request(
                &format!("{COLLECTIONS}/{collection_id}"),
                Request::Read(vec![("view".into(), "summary".into())]),
                cancellation.clone(),
            )
            .await?;
        if numeric_id(&response["data"]["id"]) != Some(collection_id) {
            return Err("Lyfta returned an unexpected collection.".into());
        }
        let workouts = response["data"]["workouts"]
            .as_array()
            .ok_or("Lyfta returned no collection routines.")?;
        return find_title(workouts, &journal.workouts[index - 1]["title"]);
    }
    let mut found = None;
    for page in 1.. {
        let response = client
            .request(
                COLLECTIONS,
                Request::Read(vec![
                    ("limit".into(), "100".into()),
                    ("page".into(), page.to_string()),
                ]),
                cancellation.clone(),
            )
            .await?;
        let collections = response["collections"]
            .as_array()
            .ok_or("Lyfta returned no collection list.")?;
        let pages = numeric_id(&response["total_pages"])
            .ok_or("Lyfta returned no collection page count.")?;
        if !collections.is_empty() && numeric_id(&response["current_page"]) != Some(page) {
            return Err("Lyfta returned an unexpected collection page.".into());
        }
        if let Some(id) = find_title(collections, &journal.collection["title"])?
            && found.replace(id).is_some_and(|previous| previous != id)
        {
            return Err("More than one Lyfta item has the saved title.".into());
        }
        if page >= pages {
            break;
        }
    }
    Ok(found)
}

fn find_title(items: &[Value], title: &Value) -> Result<Option<u64>, String> {
    let mut matching = items.iter().filter(|item| &item["title"] == title);
    let Some(item) = matching.next() else {
        return Ok(None);
    };
    if matching.next().is_some() {
        return Err("More than one Lyfta item has the saved title.".into());
    }
    numeric_id(&item["id"])
        .filter(|id| *id > 0)
        .map(Some)
        .ok_or("Lyfta returned an item without an ID.".into())
}

fn valid_id(id: &str) -> bool {
    id.len() == 24
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn add_marker(value: &mut Value, id: &str, step: usize) -> String {
    let title = value["title"].as_str().unwrap();
    let title = format!("{title}{MARKER_PREFIX}{id}-{step}]");
    value["title"] = json!(title);
    title
}

pub fn original_title(title: &str) -> &str {
    if let Some((name, marker)) = title.rsplit_once(MARKER_PREFIX)
        && let Some((id, step)) = marker
            .strip_suffix(']')
            .and_then(|text| text.split_once('-'))
        && valid_id(id)
        && step.parse::<usize>().is_ok()
    {
        name
    } else {
        title
    }
}

fn persist(path: &Path, journal: &Journal) -> io::Result<()> {
    let pending = path.join("journal.tmp");
    let mut file = File::create(&pending)?;
    serde_json::to_writer(&mut file, journal)?;
    file.flush()?;
    file.sync_all()?;
    drop(file);
    fs::rename(pending, path.join("journal.json"))?;
    sync_directory(path)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn storage_error(error: impl std::fmt::Display) -> String {
    format!("Could not access the local Lyfta program journal: {error}")
}
