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
    log: Vec<String>,
    tx: broadcast::Sender<TaskEvent>,
    /// When the log was last written to panel.db. See TaskHandle::log.
    last_flush: std::time::Instant,
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
            log: Vec::new(),
            tx,
            last_flush: std::time::Instant::now(),
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
            state.log = g.log.clone();
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

    pub fn subscribe(&self, id: &str) -> Option<(TaskState, broadcast::Receiver<TaskEvent>)> {
        let live = self.live.lock().unwrap().get(id).cloned()?;
        let g = live.lock().unwrap();
        let mut state = g.state.clone();
        state.log = g.log.clone();
        Some((state, g.tx.subscribe()))
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
            g.log.push(line.clone());
            let _ = g.tx.send(TaskEvent::Line { text: line });
            if g.last_flush.elapsed() >= LOG_FLUSH_INTERVAL {
                g.last_flush = std::time::Instant::now();
                Some(g.log.join("\n"))
            } else {
                None
            }
        };
        // Outside the task lock: a slow write must not stall log producers.
        if let Some(log_blob) = flush {
            let _ = self.tracker.db.with(|conn| {
                conn.execute(
                    "UPDATE tasks SET log_blob = ?1 WHERE id = ?2",
                    params![log_blob, self.id],
                )
                .map(|_| ())
            });
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
            (g.log.join("\n"), g.state.kind.clone(), log_tail(&g.log))
        };
        if !success {
            crate::notify::task_failed(&self.tracker.db, &kind, &self.id, &tail);
        }

        let _ = self.tracker.db.with(|conn| {
            conn.execute(
                "UPDATE tasks SET status = ?1, finished_at = ?2, log_blob = ?3 WHERE id = ?4",
                params![status.as_str(), now.to_rfc3339(), log_blob, self.id],
            )
            .map(|_| ())
        });

        self.tracker.live.lock().unwrap().remove(&self.id);
    }
}

/// The last lines of a task log, for failure notifications.
fn log_tail(log: &[String]) -> String {
    log[log.len().saturating_sub(15)..].join("\n")
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
            (g.log.join("\n"), g.state.kind.clone(), log_tail(&g.log))
        };
        crate::notify::task_failed(&self.tracker.db, &kind, &self.id, &tail);
        let _ = self.tracker.db.with(|conn| {
            conn.execute(
                "UPDATE tasks SET status = 'failed', finished_at = ?1, log_blob = ?2 WHERE id = ?3",
                params![now.to_rfc3339(), log_blob, self.id],
            )
            .map(|_| ())
        });
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
