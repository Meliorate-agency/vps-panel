//! Update all: one queue of app updates, run one after another so two builds
//! never compete for the CPU, the disk or a shared dependency cache.
//!
//! Update all puts every app the panel manages in the queue. For each one the
//! worker first checks for something new (new commits, or a newer image on the
//! registry) and updates only the apps that have it. A single Update click
//! made while the queue runs joins it (apps::update) and is updated without
//! the check, as a single click always is. Each update runs as the app's own
//! `apps.update:<name>` task, so its log, its lock and its failure alert are
//! the same as a single update's; the queue's task lists what happened to
//! each app. The queue lives in memory: an agent restart drops what was
//! still waiting (the queue's task then shows as interrupted).

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard, OnceLock};

use axum::{
    extract::{Path, State},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::error::{AgentError, AgentResult};
use crate::ipc::tasks::TaskHandle;
use crate::state::AppState;
use crate::validate::validate_name;

use super::{apps, docker_apps};

/// The queue's own task: its log says what happened to each app.
pub const QUEUE_TASK_KIND: &str = "apps.update-all";

#[derive(Clone, Debug, PartialEq)]
struct Entry {
    name: String,
    /// Update whatever the check would say (a single Update click that
    /// joined the queue). Update all only updates an app with something new.
    force: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Current {
    pub name: String,
    /// "checking" or "updating"
    pub step: &'static str,
    /// The app's own update task, once it started.
    pub task_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Outcome {
    pub name: String,
    /// "updated", "up_to_date", "failed" or "skipped"
    pub result: &'static str,
    pub detail: String,
    /// The app's own update task, when one ran.
    pub task_id: Option<String>,
}

#[derive(Default)]
struct Queue {
    running: bool,
    /// The running queue's task, else the last run's.
    task_id: Option<String>,
    current: Option<Current>,
    waiting: VecDeque<Entry>,
    /// What the current (or last) run did so far, in order.
    done: Vec<Outcome>,
}

#[derive(Debug, Serialize)]
pub struct QueueView {
    pub running: bool,
    pub task_id: Option<String>,
    pub current: Option<Current>,
    pub waiting: Vec<String>,
    pub done: Vec<Outcome>,
}

fn queue() -> MutexGuard<'static, Queue> {
    static QUEUE: OnceLock<Mutex<Queue>> = OnceLock::new();
    QUEUE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn view(q: &Queue) -> QueueView {
    QueueView {
        running: q.running,
        task_id: q.task_id.clone(),
        current: q.current.clone(),
        waiting: q.waiting.iter().map(|e| e.name.clone()).collect(),
        done: q.done.clone(),
    }
}

/// GET /apps-update-queue
pub async fn get() -> Json<QueueView> {
    Json(view(&queue()))
}

#[derive(Deserialize)]
pub struct AddBody {
    /// The apps to check and update. Empty = every app the panel manages.
    #[serde(default)]
    pub names: Vec<String>,
}

#[derive(Serialize)]
pub struct Added {
    /// The queue's task.
    pub task_id: String,
    /// The apps this call put in the queue (the others were in it already).
    pub added: Vec<String>,
}

/// POST /apps-update-queue: check the apps one at a time and update each one
/// that has something new. Joins a queue that is already running.
pub async fn add(State(state): State<AppState>, Json(body): Json<AddBody>) -> AgentResult<Json<Added>> {
    let managed = panel_apps(&state)?;
    let names = if body.names.is_empty() {
        managed
    } else {
        for name in &body.names {
            validate_name(name)?;
            if !managed.contains(name) {
                return Err(AgentError::Invalid(format!("{name} is not an app the panel manages")));
            }
        }
        body.names
    };
    if names.is_empty() {
        return Err(AgentError::Invalid("there are no apps to update".into()));
    }
    let entries = names.into_iter().map(|name| Entry { name, force: false }).collect();
    let (task_id, added) = enqueue(&state, entries);
    Ok(Json(Added { task_id, added }))
}

/// DELETE /apps-update-queue: take the apps still waiting out of the queue.
/// The one being checked or updated finishes.
pub async fn clear() -> Json<QueueView> {
    let mut q = queue();
    let dropped: Vec<Entry> = q.waiting.drain(..).collect();
    for e in dropped {
        q.done.push(Outcome {
            name: e.name,
            result: "skipped",
            detail: "taken out of the queue".into(),
            task_id: None,
        });
    }
    Json(view(&q))
}

/// The single Update of `name` (apps::update), while the queue runs: it waits
/// its turn in the queue instead of building next to the queue's update.
/// Returns the queue's task id, or None when no queue runs.
pub fn join(state: &AppState, name: &str) -> AgentResult<Option<String>> {
    let mut q = queue();
    if !q.running {
        return Ok(None);
    }
    if q.current.as_ref().is_some_and(|c| c.name == name) {
        return Err(AgentError::Conflict(format!(
            "Update all is checking or updating {name} right now - wait for it to finish"
        )));
    }
    match q.waiting.iter_mut().find(|e| e.name == name) {
        Some(e) => e.force = true,
        None => q.waiting.push_back(Entry {
            name: name.to_string(),
            force: true,
        }),
    }
    if let Some(id) = q.task_id.as_deref() {
        state
            .tasks
            .log_live(id, &format!("{name}: Update was clicked for it - it waits its turn in this queue"));
    }
    Ok(q.task_id.clone())
}

/// Every app with a panel.db row, by name.
fn panel_apps(state: &AppState) -> AgentResult<Vec<String>> {
    state.panel_db.with(|conn| {
        let mut stmt = conn.prepare("SELECT name FROM apps ORDER BY name")?;
        let names = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(names)
    })
}

/// Puts the entries not already waiting or in progress at the end of the
/// queue and starts the worker when none runs. Returns the queue's task id
/// and the names added.
fn enqueue(state: &AppState, entries: Vec<Entry>) -> (String, Vec<String>) {
    let mut q = queue();
    if !q.running {
        q.done.clear();
    }
    let added = push_new(&mut q, entries);
    if q.running {
        return (q.task_id.clone().unwrap_or_default(), added);
    }
    let task = state.tasks.create(QUEUE_TASK_KIND);
    let task_id = task.id.clone();
    q.running = true;
    q.task_id = Some(task_id.clone());
    drop(q);
    tokio::spawn(work(state.clone(), task));
    (task_id, added)
}

fn push_new(q: &mut Queue, entries: Vec<Entry>) -> Vec<String> {
    let mut added = Vec::new();
    for e in entries {
        let in_progress = q.current.as_ref().is_some_and(|c| c.name == e.name);
        if in_progress || q.waiting.iter().any(|w| w.name == e.name) {
            continue;
        }
        added.push(e.name.clone());
        q.waiting.push_back(e);
    }
    added
}

/// Takes the next app off the queue and marks it in progress. None = the
/// queue is empty: it is marked stopped in the same step, so an app added
/// from now on starts a new run.
fn next(q: &mut Queue) -> Option<Entry> {
    match q.waiting.pop_front() {
        Some(e) => {
            q.current = Some(Current {
                name: e.name.clone(),
                step: "checking",
                task_id: None,
            });
            Some(e)
        }
        None => {
            q.current = None;
            q.running = false;
            None
        }
    }
}

async fn work(state: AppState, task: TaskHandle) {
    task.start();
    task.log("Update all: checking the apps one at a time and updating each one that has something new");
    let (summary, all_ok) = loop {
        let entry = {
            let mut q = queue();
            match next(&mut q) {
                Some(e) => e,
                None => break summarize(&q.done),
            }
        };
        task.log(format!("{}: checking", entry.name));
        let outcome = run_one(&state, &entry, &task).await;
        task.log(describe(&outcome));
        {
            let mut q = queue();
            q.current = None;
            q.done.push(outcome);
        }
    };
    task.log(summary);
    task.finish(all_ok);
}

enum Check {
    /// Something new, said how.
    Behind(String),
    UpToDate,
    NotFromGit,
    Failed(String),
}

async fn run_one(state: &AppState, entry: &Entry, queue_task: &TaskHandle) -> Outcome {
    let name = entry.name.clone();
    let outcome = |result: &'static str, detail: String, task_id: Option<String>| Outcome {
        name: name.clone(),
        result,
        detail,
        task_id,
    };
    if apps::lookup_app(state, &name).await.is_none() {
        return outcome("skipped", "the app no longer exists".into(), None);
    }
    if let Some((what, id)) = apps::running_app_task(state, &name) {
        return outcome("skipped", format!("a {what} of it was already running"), Some(id));
    }
    let why = if entry.force {
        "Update was clicked for it".to_string()
    } else {
        match check(state, &name).await {
            Check::Behind(why) => why,
            Check::UpToDate => return outcome("up_to_date", "nothing new".into(), None),
            Check::NotFromGit => {
                return outcome("skipped", "not deployed from Git, so there is nothing to pull".into(), None)
            }
            Check::Failed(e) => return outcome("failed", format!("could not check for updates: {e}"), None),
        }
    };
    let job = match apps::prepare_update(state, &name).await {
        Ok(job) => job,
        Err(e) => return outcome("failed", e.to_string(), None),
    };
    let app_task = state.tasks.create(&format!("apps.update:{name}"));
    let app_task_id = app_task.id.clone();
    {
        let mut q = queue();
        if let Some(c) = q.current.as_mut() {
            c.step = "updating";
            c.task_id = Some(app_task_id.clone());
        }
    }
    queue_task.log(format!("{name}: {why} - updating (its log: task {app_task_id})"));
    if job.run(app_task).await {
        outcome("updated", why, Some(app_task_id))
    } else {
        let reason = state
            .tasks
            .get(&app_task_id)
            .and_then(|t| t.log.iter().rev().find_map(|l| l.strip_prefix("error: ").map(str::to_string)))
            .unwrap_or_else(|| "the update failed".into());
        outcome("failed", reason, Some(app_task_id))
    }
}

/// Does the app have something new? Docker apps from a registry ask the
/// registry; Node apps and Docker apps built from Git fetch their repository.
async fn check(state: &AppState, name: &str) -> Check {
    if docker_apps::is_docker_app(name) {
        let built = docker_apps::read_config(name)
            .await
            .is_some_and(|spec| spec.build.is_some());
        if !built {
            return match docker_apps::image_status(Path(name.to_string())).await {
                Err(e) => Check::Failed(e.to_string()),
                Ok(Json(s)) => match s.update_available {
                    Some(true) => Check::Behind(format!("a newer {} is on its registry", s.image)),
                    Some(false) => Check::UpToDate,
                    None => Check::Failed(s.error.unwrap_or_else(|| "the registry did not say".into())),
                },
            };
        }
    }
    match apps::git_status(State(state.clone()), Path(name.to_string())).await {
        Err(e) => Check::Failed(e.to_string()),
        Ok(Json(s)) => git_check(s.has_repo, s.behind, s.ahead, s.error),
    }
}

fn git_check(has_repo: bool, behind: u32, ahead: u32, error: Option<String>) -> Check {
    if !has_repo {
        return Check::NotFromGit;
    }
    if let Some(e) = error {
        return Check::Failed(e);
    }
    if behind == 0 {
        return Check::UpToDate;
    }
    if ahead > 0 {
        return Check::Failed(format!(
            "it has {ahead} local commit(s) the repository does not, so the {behind} new one(s) cannot be pulled \
             (a pull only fast-forwards)"
        ));
    }
    Check::Behind(format!("{behind} new commit(s)"))
}

fn describe(o: &Outcome) -> String {
    let what = match o.result {
        "updated" => "updated",
        "up_to_date" => "up to date",
        "failed" => "FAILED",
        _ => "skipped",
    };
    format!("{}: {what} - {}", o.name, o.detail)
}

/// The closing log line, and whether every app came through (nothing failed).
fn summarize(done: &[Outcome]) -> (String, bool) {
    let count = |r: &str| done.iter().filter(|o| o.result == r).count();
    let failed: Vec<&str> = done
        .iter()
        .filter(|o| o.result == "failed")
        .map(|o| o.name.as_str())
        .collect();
    let mut line = format!(
        "finished: {} updated, {} up to date, {} skipped, {} failed",
        count("updated"),
        count("up_to_date"),
        count("skipped"),
        failed.len()
    );
    if !failed.is_empty() {
        line.push_str(&format!(" ({})", failed.join(", ")));
    }
    (line, failed.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str) -> Entry {
        Entry {
            name: name.into(),
            force: false,
        }
    }

    #[test]
    fn an_app_already_waiting_or_in_progress_is_not_queued_twice() {
        let mut q = Queue::default();
        assert_eq!(push_new(&mut q, vec![entry("api"), entry("web")]), ["api", "web"]);
        let first = next(&mut q).unwrap();
        assert_eq!(first.name, "api");
        assert_eq!(q.current.as_ref().unwrap().step, "checking");
        // api is in progress, web waits: only blog is new.
        assert_eq!(push_new(&mut q, vec![entry("api"), entry("web"), entry("blog")]), ["blog"]);
        let order: Vec<&str> = q.waiting.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(order, ["web", "blog"]);
    }

    #[test]
    fn an_empty_queue_stops_in_the_same_step() {
        let mut q = Queue {
            running: true,
            ..Default::default()
        };
        assert!(push_new(&mut q, vec![entry("api")]).len() == 1);
        assert!(next(&mut q).is_some());
        assert!(q.running);
        assert!(next(&mut q).is_none());
        assert!(!q.running);
        assert!(q.current.is_none());
    }

    #[test]
    fn git_status_decides_what_is_updated() {
        assert!(matches!(git_check(false, 0, 0, None), Check::NotFromGit));
        assert!(matches!(git_check(true, 0, 0, None), Check::UpToDate));
        assert!(matches!(git_check(true, 0, 2, None), Check::UpToDate));
        assert!(matches!(git_check(true, 3, 0, None), Check::Behind(ref s) if s == "3 new commit(s)"));
        assert!(matches!(git_check(true, 3, 1, None), Check::Failed(_)));
        assert!(matches!(git_check(true, 3, 0, Some("git fetch timed out".into())), Check::Failed(ref e) if e == "git fetch timed out"));
    }

    #[test]
    fn the_summary_names_the_apps_that_failed() {
        let o = |name: &str, result: &'static str| Outcome {
            name: name.into(),
            result,
            detail: String::new(),
            task_id: None,
        };
        let (line, ok) = summarize(&[o("api", "updated"), o("web", "up_to_date"), o("blog", "failed")]);
        assert_eq!(line, "finished: 1 updated, 1 up to date, 0 skipped, 1 failed (blog)");
        assert!(!ok);
        let (line, ok) = summarize(&[o("api", "skipped")]);
        assert_eq!(line, "finished: 0 updated, 0 up to date, 1 skipped, 0 failed");
        assert!(ok);
    }
}
