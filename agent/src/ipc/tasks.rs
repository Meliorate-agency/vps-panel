use chrono::{DateTime, Utc};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::db::PanelDb;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
}

impl TaskStatus {
    fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::Pending => "pending",
            TaskStatus::Running => "running",
            TaskStatus::Succeeded => "succeeded",
            TaskStatus::Failed => "failed",
        }
    }
    fn parse(s: &str) -> TaskStatus {
        match s {
            "running" => TaskStatus::Running,
            "succeeded" => TaskStatus::Succeeded,
            "failed" => TaskStatus::Failed,
            _ => TaskStatus::Pending,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskState {
    pub id: String,
    pub kind: String,
    pub status: TaskStatus,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub log: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "lowercase")]
pub enum TaskEvent {
    Line { text: String },
    Status { status: TaskStatus },
    Done { status: TaskStatus },
}

struct LiveTask {
    state: TaskState,
    log: BoundedLog,
    tx: broadcast::Sender<TaskEvent>,
    /// When the log was last written to panel.db. See TaskHandle::log.
    last_flush: std::time::Instant,
    /// `log.pushed` as of the last write that reached panel.db: a log with
    /// nothing new since then is not written again.
    saved: u64,
}

/// One line of a task log is cut after this many bytes (a minified bundle or
/// a progress bar printed as one line).
const LOG_MAX_LINE_BYTES: usize = 4 * 1024;
/// A task log keeps its beginning (the command, the environment, the first
/// steps) and its end (the error), and leaves out the middle of a long one.
/// Every line used to be kept for the life of the task and written to
/// panel.db again every LOG_FLUSH_INTERVAL: a chatty build held hundreds of
/// megabytes and rewrote them every two seconds.
const LOG_HEAD_LINES: usize = 1_000;
const LOG_HEAD_BYTES: usize = 256 * 1024;
const LOG_TAIL_LINES: usize = 5_000;
const LOG_TAIL_BYTES: usize = 1024 * 1024;

/// The lines of one task's log, within the limits above.
#[derive(Default)]
struct BoundedLog {
    head: Vec<String>,
    head_bytes: usize,
    /// Set once the head is full: every later line goes to the tail.
    head_full: bool,
    tail: std::collections::VecDeque<String>,
    tail_bytes: usize,
    /// Lines dropped between the head and the tail.
    omitted: u64,
    /// Lines ever pushed.
    pushed: u64,
}

impl BoundedLog {
    fn push(&mut self, line: String) {
        self.pushed += 1;
        if !self.head_full {
            if self.head.len() < LOG_HEAD_LINES && self.head_bytes + line.len() <= LOG_HEAD_BYTES {
                self.head_bytes += line.len();
                self.head.push(line);
                return;
            }
            self.head_full = true;
        }
        self.tail_bytes += line.len();
        self.tail.push_back(line);
        while self.tail.len() > LOG_TAIL_LINES || (self.tail_bytes > LOG_TAIL_BYTES && self.tail.len() > 1) {
            if let Some(dropped) = self.tail.pop_front() {
                self.tail_bytes -= dropped.len();
                self.omitted += 1;
            }
        }
    }

    /// The log as it is stored and replayed: the head, a line saying how many
    /// lines are missing when some are, the tail.
    fn lines(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(self.head.len() + self.tail.len() + 1);
        out.extend(self.head.iter().cloned());
        if self.omitted > 0 {
            out.push(format!(
                "[... {} lines left out here: a long log keeps its beginning and its end ...]",
                self.omitted
            ));
        }
        out.extend(self.tail.iter().cloned());
        out
    }

    fn blob(&self) -> String {
        self.lines().join("\n")
    }

    /// The last `n` lines, for failure notifications.
    fn last(&self, n: usize) -> String {
        let all: Vec<&str> = self.head.iter().chain(self.tail.iter()).map(String::as_str).collect();
        all[all.len().saturating_sub(n)..].join("\n")
    }
}

/// `line` cut to LOG_MAX_LINE_BYTES, saying how much is missing.
fn cap_line(line: &str) -> String {
    if line.len() <= LOG_MAX_LINE_BYTES {
        return line.to_string();
    }
    let mut cut = LOG_MAX_LINE_BYTES;
    while !line.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{} [... {} more bytes]", &line[..cut], line.len() - cut)
}

/// How often a running task's log is saved to panel.db. Without this the log
/// existed only in memory until finish(), so a task killed mid-run (panel
/// self-update restarts the agent part-way through) lost every line.
const LOG_FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// How long finished tasks (and their logs) are kept.
pub const TASK_RETENTION_DAYS: i64 = 30;

#[derive(Clone)]
pub struct TaskTracker {
    db: PanelDb,
    live: Arc<Mutex<HashMap<String, Arc<Mutex<LiveTask>>>>>,
}

pub struct TaskHandle {
    tracker: TaskTracker,
    inner: Arc<Mutex<LiveTask>>,
    pub id: String,
    finished: bool,
}

impl TaskTracker {
    pub fn new(db: PanelDb) -> Self {
        TaskTracker {
            db,
            live: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn create(&self, kind: &str) -> TaskHandle {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let (tx, _rx) = broadcast::channel::<TaskEvent>(256);
        let state = TaskState {
            id: id.clone(),
            kind: kind.to_string(),
            status: TaskStatus::Pending,
            started_at: now,
            finished_at: None,
            log: Vec::new(),
        };

        if let Err(e) = self.db.with(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO tasks (id, kind, status, started_at, finished_at, log_blob)
                 VALUES (?1, ?2, ?3, ?4, NULL, '')",
                params![id, kind, "pending", now.to_rfc3339()],
            )
            .map(|_| ())
        }) {
            tracing::warn!("task create persist failed: {e}");
        }

        let live = Arc::new(Mutex::new(LiveTask {
            state,
            log: BoundedLog::default(),
            tx,
            last_flush: std::time::Instant::now(),
            saved: 0,
        }));
        self.live.lock().unwrap().insert(id.clone(), live.clone());

        TaskHandle {
            tracker: self.clone(),
            inner: live,
            id,
            finished: false,
        }
    }

    pub fn get(&self, id: &str) -> Option<TaskState> {
        if let Some(live) = self.live.lock().unwrap().get(id).cloned() {
            let g = live.lock().unwrap();
            let mut state = g.state.clone();
            state.log = g.log.lines();
            return Some(state);
        }
        self.db
            .with(|conn| {
                conn.query_row(
                    "SELECT id, kind, status, started_at, finished_at, log_blob
                     FROM tasks WHERE id = ?1",
                    params![id],
                    row_to_state,
                )
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })
            })
            .ok()
            .flatten()
    }

    /// The log so far and every line from here on. The snapshot is what
    /// `get` and the stored row hold (see BoundedLog::lines); the lines that
    /// follow are the ones `log` keeps, cut the same way.
    pub fn subscribe(&self, id: &str) -> Option<(TaskState, broadcast::Receiver<TaskEvent>)> {
        let live = self.live.lock().unwrap().get(id).cloned()?;
        let g = live.lock().unwrap();
        let mut state = g.state.clone();
        state.log = g.log.lines();
        Some((state, g.tx.subscribe()))
    }

    /// The id of a still-running task of exactly this kind.
    pub fn live_task_id(&self, kind: &str) -> Option<String> {
        self.live
            .lock()
            .unwrap()
            .values()
            .map(|t| t.lock().unwrap())
            .find(|t| t.state.kind == kind)
            .map(|t| t.state.id.clone())
    }

    /// True when a still-running task's kind starts with `prefix`. Used as a
    /// cheap mutual-exclusion check for operations that must not overlap
    /// (e.g. two concurrent SQL-dump imports racing each other's DROP/CREATE).
    pub fn any_live_with_prefix(&self, prefix: &str) -> bool {
        self.live
            .lock()
            .unwrap()
            .values()
            .any(|t| t.lock().unwrap().state.kind.starts_with(prefix))
    }

    /// Kinds of every task still running in this agent.
    pub fn live_kinds(&self) -> Vec<String> {
        self.live
            .lock()
            .unwrap()
            .values()
            .map(|t| t.lock().unwrap().state.kind.clone())
            .collect()
    }

    /// Self-update tasks a previous agent left pending/running. update.sh
    /// restarts the agent part-way through, so their outcome is settled by
    /// `system::finalize_self_update` once update.sh itself has exited.
    pub fn unfinished_self_updates(&self) -> Vec<String> {
        self.db
            .with(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT id FROM tasks
                      WHERE status IN ('pending', 'running')
                        AND kind LIKE 'system:self-update%'",
                )?;
                let ids = stmt
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, rusqlite::Error>(ids)
            })
            .unwrap_or_default()
    }

    /// Settle a task that is no longer live in this agent: set its status and
    /// append `appended` to the log saved so far.
    pub fn finalize_persisted(&self, id: &str, success: bool, appended: &str) {
        let status = if success { "succeeded" } else { "failed" };
        let _ = self.db.with(|conn| {
            conn.execute(
                "UPDATE tasks
                    SET status = ?1, finished_at = ?2, log_blob = log_blob || ?3
                  WHERE id = ?4",
                params![status, Utc::now().to_rfc3339(), appended, id],
            )
            .map(|_| ())
        });
    }

    /// Exact kind match, for names where a prefix would also match a longer
    /// sibling (`apps.update:shop` vs `apps.update:shop-test`).
    pub fn any_live_kind(&self, kind: &str) -> bool {
        self.live
            .lock()
            .unwrap()
            .values()
            .any(|t| t.lock().unwrap().state.kind == kind)
    }

    /// Newest 200 tasks without their logs (GET /tasks/:id has the log).
    /// Shipping every row's full build log made this response megabytes.
    pub fn list(&self) -> Vec<TaskState> {
        self.db
            .with(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT id, kind, status, started_at, finished_at, '' AS log_blob
                     FROM tasks ORDER BY started_at DESC LIMIT 200",
                )?;
                let rows = stmt.query_map([], row_to_state)?;
                Ok(rows.filter_map(Result::ok).collect())
            })
            .unwrap_or_default()
    }

    /// Delete finished tasks older than TASK_RETENTION_DAYS. Each row keeps
    /// its whole log (full npm/cargo build output), so panel.db grew with
    /// every deploy and never shrank.
    pub fn prune_old(&self) {
        let cutoff = (Utc::now() - chrono::Duration::days(TASK_RETENTION_DAYS)).to_rfc3339();
        let _ = self.db.with(|conn| {
            let n = conn.execute(
                "DELETE FROM tasks
                  WHERE status NOT IN ('pending', 'running') AND started_at < ?1",
                params![cutoff],
            )?;
            if n > 0 {
                tracing::info!(pruned = n, "deleted task rows older than the retention window");
            }
            Ok::<_, rusqlite::Error>(())
        });
    }

    /// Reconcile rows left in `pending` / `running` state from a previous
    /// agent process — the in-memory TaskHandle that was tracking them is
    /// gone, so they would otherwise stay "running" forever in the panel UI.
    ///
    /// Two cases get different treatment:
    /// - `system:self-update*` tasks → marked `succeeded`. The whole point
    ///   of the self-update flow is to bounce the agent at the end, so
    ///   reaching the next boot IS the success condition. If the build had
    ///   failed, the agent would have called `task.finish(false)` and never
    ///   issued the restart — meaning this row wouldn't be 'running'
    ///   anymore. (False positive: hard OOM kill mid-build → marked
    ///   succeeded. Acceptable; operator re-runs.)
    /// - Everything else → marked `failed` with a note. The agent died or
    ///   was force-restarted mid-task; we can't know the outcome, so we
    ///   pick the conservative answer.
    ///
    /// Idempotent: re-running on a clean DB is a no-op.
    pub fn sweep_interrupted(&self) {
        let now = Utc::now().to_rfc3339();
        let failed_note = "\n[interrupted by agent restart]".to_string();
        let _ = self.db.with(|conn| {
            // Self-updates are left alone: update.sh restarts the agent on
            // purpose and keeps running, so they are settled from its exit
            // code by system::finalize_self_update, not assumed to succeed.
            let n_f = conn.execute(
                "UPDATE tasks
                    SET status = 'failed',
                        finished_at = ?1,
                        log_blob = log_blob || ?2
                  WHERE status IN ('pending', 'running')
                    AND kind NOT LIKE 'system:self-update%'",
                params![now, failed_note],
            )?;
            if n_f > 0 {
                tracing::info!(
                    interrupted_failed = n_f,
                    "swept interrupted tasks from previous agent boot",
                );
            }
            Ok::<_, rusqlite::Error>(())
        });
    }
}

impl TaskHandle {
    pub fn start(&self) {
        {
            let mut g = self.inner.lock().unwrap();
            g.state.status = TaskStatus::Running;
            let _ = g.tx.send(TaskEvent::Status {
                status: TaskStatus::Running,
            });
        }
        let _ = self.tracker.db.with(|conn| {
            conn.execute(
                "UPDATE tasks SET status = ?1 WHERE id = ?2",
                params!["running", self.id],
            )
            .map(|_| ())
        });
    }

    pub fn log(&self, line: impl Into<String>) {
        let line = line.into();
        let flush = {
            let mut g = self.inner.lock().unwrap();
            // A message of several lines (an error with its hint, a
            // command's stderr) is that many log lines: it is stored joined
            // by newlines and read back split on them anyway, and the limits
            // then apply to each line, not to the message as a whole.
            for part in line.split('\n') {
                let part = cap_line(part);
                let _ = g.tx.send(TaskEvent::Line { text: part.clone() });
                g.log.push(part);
            }
            if g.last_flush.elapsed() >= LOG_FLUSH_INTERVAL {
                g.last_flush = std::time::Instant::now();
                Some((g.log.blob(), g.log.pushed))
            } else {
                None
            }
        };
        // Outside the task lock: a slow write must not stall log producers.
        if let Some((log_blob, pushed)) = flush {
            let written = self.tracker.db.with(|conn| {
                conn.execute(
                    "UPDATE tasks SET log_blob = ?1 WHERE id = ?2",
                    params![log_blob, self.id],
                )
                .map(|_| ())
            });
            if written.is_ok() {
                let mut g = self.inner.lock().unwrap();
                g.saved = g.saved.max(pushed);
            }
        }
    }

    pub fn finish(mut self, success: bool) {
        self.finished = true;
        let status = if success { TaskStatus::Succeeded } else { TaskStatus::Failed };
        let now = Utc::now();
        let (log_blob, kind, tail) = {
            let mut g = self.inner.lock().unwrap();
            g.state.status = status.clone();
            g.state.finished_at = Some(now);
            let _ = g.tx.send(TaskEvent::Done {
                status: status.clone(),
            });
            (unsaved_log(&g), g.state.kind.clone(), g.log.last(NOTIFY_TAIL_LINES))
        };
        if !success {
            crate::notify::task_failed(&self.tracker.db, &kind, &self.id, &tail);
        }
        self.tracker.settle(&self.id, status.as_str(), now, log_blob);
        self.tracker.live.lock().unwrap().remove(&self.id);
    }
}

/// How many of a failed task's last log lines go into its notification.
const NOTIFY_TAIL_LINES: usize = 15;

/// The log to store when a task ends, or None when panel.db already has
/// every line of it.
fn unsaved_log(task: &LiveTask) -> Option<String> {
    (task.saved != task.log.pushed).then(|| task.log.blob())
}

impl TaskTracker {
    /// Store a task's final status, and its log unless the stored one is
    /// already complete.
    fn settle(&self, id: &str, status: &str, at: DateTime<Utc>, log_blob: Option<String>) {
        let _ = self.db.with(|conn| {
            match &log_blob {
                Some(log_blob) => conn.execute(
                    "UPDATE tasks SET status = ?1, finished_at = ?2, log_blob = ?3 WHERE id = ?4",
                    params![status, at.to_rfc3339(), log_blob, id],
                ),
                None => conn.execute(
                    "UPDATE tasks SET status = ?1, finished_at = ?2 WHERE id = ?3",
                    params![status, at.to_rfc3339(), id],
                ),
            }
            .map(|_| ())
        });
    }
}

impl Drop for TaskHandle {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let now = Utc::now();
        let (log_blob, kind, tail) = {
            let mut g = self.inner.lock().unwrap();
            if g.state.finished_at.is_some() {
                return;
            }
            g.state.status = TaskStatus::Failed;
            g.state.finished_at = Some(now);
            let _ = g.tx.send(TaskEvent::Done {
                status: TaskStatus::Failed,
            });
            (unsaved_log(&g), g.state.kind.clone(), g.log.last(NOTIFY_TAIL_LINES))
        };
        crate::notify::task_failed(&self.tracker.db, &kind, &self.id, &tail);
        self.tracker.settle(&self.id, "failed", now, log_blob);
        self.tracker.live.lock().unwrap().remove(&self.id);
    }
}

fn row_to_state(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskState> {
    let id: String = row.get(0)?;
    let kind: String = row.get(1)?;
    let status: String = row.get(2)?;
    let started_at: String = row.get(3)?;
    let finished_at: Option<String> = row.get(4)?;
    let log_blob: String = row.get(5)?;
    Ok(TaskState {
        id,
        kind,
        status: TaskStatus::parse(&status),
        started_at: DateTime::parse_from_rfc3339(&started_at)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
        finished_at: finished_at
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc)),
        log: if log_blob.is_empty() {
            Vec::new()
        } else {
            log_blob.split('\n').map(String::from).collect()
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A TaskTracker on a throwaway panel.db.
    fn tracker() -> (tempfile::TempDir, TaskTracker) {
        let dir = tempfile::tempdir().unwrap();
        let db = PanelDb::open(&dir.path().join("panel.db")).unwrap();
        db.with(|c| {
            c.execute_batch(
                "CREATE TABLE tasks (id TEXT PRIMARY KEY, kind TEXT NOT NULL, status TEXT NOT NULL,
                 started_at TEXT NOT NULL, finished_at TEXT, log_blob TEXT NOT NULL DEFAULT '');",
            )
        })
        .unwrap();
        (dir, TaskTracker::new(db))
    }

    fn stored_log(tasks: &TaskTracker, id: &str) -> String {
        tasks
            .db
            .with(|c| c.query_row("SELECT log_blob FROM tasks WHERE id = ?1", params![id], |r| r.get(0)))
            .unwrap()
    }

    /// Make the next `log` call write the log to panel.db.
    fn flush_due(task: &TaskHandle) {
        task.inner.lock().unwrap().last_flush = std::time::Instant::now() - LOG_FLUSH_INTERVAL * 2;
    }

    #[test]
    fn a_long_log_keeps_its_beginning_and_its_end() {
        let (_dir, tasks) = tracker();
        let task = tasks.create("apps.update:web");
        let total = LOG_HEAD_LINES + LOG_TAIL_LINES + 4_321;
        for i in 0..total {
            task.log(format!("line {i}"));
        }
        let live = tasks.get(&task.id).unwrap().log;
        assert_eq!(live.len(), LOG_HEAD_LINES + LOG_TAIL_LINES + 1);
        assert_eq!(live[0], "line 0");
        assert_eq!(live[LOG_HEAD_LINES - 1], format!("line {}", LOG_HEAD_LINES - 1));
        assert!(live[LOG_HEAD_LINES].contains("4321 lines left out"), "{}", live[LOG_HEAD_LINES]);
        assert_eq!(live[LOG_HEAD_LINES + 1], format!("line {}", total - LOG_TAIL_LINES));
        assert_eq!(live.last().unwrap(), &format!("line {}", total - 1));

        // A stream that connects now replays the same lines, and so does the
        // stored row once the task has ended.
        let (snapshot, _rx) = tasks.subscribe(&task.id).unwrap();
        assert_eq!(snapshot.log, live);
        let id = task.id.clone();
        task.finish(true);
        assert_eq!(tasks.get(&id).unwrap().log, live);
    }

    #[test]
    fn a_short_log_is_kept_whole() {
        let (_dir, tasks) = tracker();
        let task = tasks.create("ssl.issue:example.com");
        for i in 0..50 {
            task.log(format!("line {i}"));
        }
        let live = tasks.get(&task.id).unwrap().log;
        assert_eq!(live.len(), 50);
        assert!(!live.iter().any(|l| l.contains("left out")));
    }

    #[test]
    fn the_bytes_a_log_holds_are_bounded_too() {
        let mut log = BoundedLog::default();
        let line = "x".repeat(LOG_MAX_LINE_BYTES);
        for _ in 0..20_000 {
            log.push(cap_line(&line));
        }
        let held: usize = log.lines().iter().map(String::len).sum();
        assert!(held <= LOG_HEAD_BYTES + LOG_TAIL_BYTES + 200, "{held}");
        assert!(log.head.len() < LOG_HEAD_LINES && log.tail.len() < LOG_TAIL_LINES);
        assert_eq!(log.head.len() as u64 + log.tail.len() as u64 + log.omitted, 20_000);
    }

    #[test]
    fn one_line_is_cut_the_same_in_the_stream_and_in_the_stored_log() {
        let (_dir, tasks) = tracker();
        let task = tasks.create("apps.create:web");
        let (_, mut rx) = tasks.subscribe(&task.id).unwrap();
        // 'é' is two bytes: the cut must not land inside one.
        let long = "é".repeat(LOG_MAX_LINE_BYTES);
        task.log(format!("first\n{long}\nlast"));
        let mut streamed = Vec::new();
        while let Ok(TaskEvent::Line { text }) = rx.try_recv() {
            streamed.push(text);
        }
        assert_eq!(streamed.len(), 3);
        assert_eq!((streamed[0].as_str(), streamed[2].as_str()), ("first", "last"));
        assert!(streamed[1].len() < LOG_MAX_LINE_BYTES + 40);
        assert!(streamed[1].ends_with(&format!("[... {LOG_MAX_LINE_BYTES} more bytes]")), "{}", streamed[1]);
        assert_eq!(tasks.get(&task.id).unwrap().log, streamed);
        let id = task.id.clone();
        task.finish(true);
        assert_eq!(tasks.get(&id).unwrap().log, streamed);
    }

    #[test]
    fn a_log_with_nothing_new_is_not_written_again() {
        let (_dir, tasks) = tracker();
        let task = tasks.create("apps.update:web");
        task.log("one");
        assert_eq!(stored_log(&tasks, &task.id), "");
        flush_due(&task);
        task.log("two");
        assert_eq!(stored_log(&tasks, &task.id), "one\ntwo");
        // Nothing was logged since: the row's log is left as it is.
        let id = task.id.clone();
        tasks
            .db
            .with(|c| c.execute("UPDATE tasks SET log_blob = 'untouched' WHERE id = ?1", params![id]))
            .unwrap();
        task.finish(true);
        assert_eq!(stored_log(&tasks, &id), "untouched");
        assert_eq!(tasks.get(&id).unwrap().status, TaskStatus::Succeeded);

        // A line after the last write is stored when the task ends.
        let task = tasks.create("apps.update:web");
        flush_due(&task);
        task.log("one");
        task.log("two");
        let id = task.id.clone();
        task.finish(true);
        assert_eq!(stored_log(&tasks, &id), "one\ntwo");
    }

    #[test]
    fn a_running_task_is_found_by_its_kind() {
        let (_dir, tasks) = tracker();
        let task = tasks.create("apps.create:shop");
        assert_eq!(tasks.live_task_id("apps.create:shop"), Some(task.id.clone()));
        assert_eq!(tasks.live_task_id("apps.create:shop-test"), None);
        task.finish(true);
        assert_eq!(tasks.live_task_id("apps.create:shop"), None);
    }
}
