//! Making a server snapshot: the export task, the size estimate and the
//! clean-up after an interrupted export.

use super::archive::{SnapshotWriter, TreeStats};
use super::format::{self, EntryHash, Index, Manifest, SectionDone, SectionInfo, Sidecar};
use super::plan::{self, Excludes, PlanOptions, Section};
use super::{dumps, inventory};
use crate::error::{AgentError, AgentResult};
use crate::ipc::tasks::TaskHandle;
use crate::state::AppState;
use mysql_async::prelude::*;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// What must stay free on the disk while a snapshot is written, the same
/// floor an upload keeps (files.rs MIN_FREE_BYTES).
const MIN_FREE_BYTES: u64 = 1024 * 1024 * 1024;

/// Free space is looked at again after this much has been written.
const SPACE_CHECK_EVERY: u64 = 256 * 1024 * 1024;

/// One progress line per this long.
const PROGRESS_EVERY: Duration = Duration::from_secs(10);

/// Containers frozen for the copy of their volumes. Written before the
/// first `docker pause`, removed after the last `docker unpause`; an agent
/// that dies in between unpauses them when it starts again.
fn paused_marker() -> PathBuf {
    Path::new(format::SNAPSHOT_DIR).join(".paused.json")
}

#[derive(Deserialize)]
pub struct ExportReq {
    pub passphrase: String,
    #[serde(default)]
    pub include_drive: bool,
    #[serde(default = "yes")]
    pub pause_docker: bool,
}

fn yes() -> bool {
    true
}

/// A snapshot or a restic backup at the same time would race for the disk
/// and the dumps; the import / pull of milestone 2 and 3 use the same kinds.
pub fn refuse_overlap(state: &AppState) -> AgentResult<()> {
    if state.tasks.any_live_with_prefix("snapshots.") {
        return Err(AgentError::Conflict("a server snapshot is already being made, pulled or imported".into()));
    }
    if state.tasks.any_live_with_prefix("backups.") {
        return Err(AgentError::Conflict("a backup or restore is running; wait for it to finish".into()));
    }
    Ok(())
}

pub async fn start(state: AppState, req: ExportReq) -> AgentResult<String> {
    format::check_passphrase(&req.passphrase).map_err(AgentError::Invalid)?;
    refuse_overlap(&state)?;
    ensure_private_dir(Path::new(format::SNAPSHOT_DIR)).await?;
    let task = state.tasks.create("snapshots.export");
    let task_id = task.id.clone();
    tokio::spawn(async move {
        task.start();
        let staging = Path::new(format::SNAPSHOT_DIR).join(format!(".staging-{}", task.id));
        let (task, outcome) = run(&state, req, task, &staging).await;
        let _ = tokio::fs::remove_dir_all(&staging).await;
        match (task, outcome) {
            (Some(task), Ok(())) => task.finish(true),
            (Some(task), Err(e)) => {
                task.log(format!("the snapshot was not made: {e}"));
                task.finish(false);
            }
            // The handle went down with a panicked writer thread; its Drop
            // already marked the task failed.
            (None, outcome) => tracing::error!("snapshot export stopped: {:?}", outcome.err()),
        }
    });
    Ok(task_id)
}

async fn run(
    state: &AppState,
    req: ExportReq,
    task: TaskHandle,
    staging: &Path,
) -> (Option<TaskHandle>, AgentResult<()>) {
    let job = match prepare(state, &req, &task, staging).await {
        Ok(job) => job,
        Err(e) => return (Some(task), Err(e)),
    };
    let part = job.part.clone();
    let passphrase = req.passphrase;
    let joined = tokio::task::spawn_blocking(move || {
        let r = write_snapshot(&job, &passphrase, &task);
        (task, job, r)
    })
    .await;
    match joined {
        Ok((task, job, Ok(done))) => {
            let result = publish(&job, done, &task).await;
            (Some(task), result)
        }
        Ok((task, _, Err(e))) => {
            let _ = tokio::fs::remove_file(&part).await;
            (Some(task), Err(AgentError::Io(e)))
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&part).await;
            (None, Err(AgentError::Other(anyhow::anyhow!("the export stopped unexpectedly: {e}"))))
        }
    }
}

/// Everything the blocking writer needs, gathered in async land.
struct Job {
    stem: String,
    part: PathBuf,
    final_path: PathBuf,
    staging: PathBuf,
    manifest: Manifest,
    system_files: Vec<(String, Vec<u8>)>,
    dumps: Vec<dumps::Staged>,
    sections: Vec<Section>,
    excludes: Excludes,
    pause: Vec<String>,
    est_total: u64,
    notes: Vec<String>,
}

struct Written {
    size: u64,
    sha256: String,
    index: Index,
}

async fn prepare(state: &AppState, req: &ExportReq, task: &TaskHandle, staging: &Path) -> AgentResult<Job> {
    ensure_private_dir(staging).await?;
    task.log("looking at what this server holds");
    let gathered = inventory::gather(state).await;
    let opts = PlanOptions { include_drive: req.include_drive };
    let sections = plan::plan_sections(&gathered.facts, &opts);
    let excludes = Excludes::default();

    let est = estimate_sections(&sections, &excludes).await;
    let db_bytes = database_bytes(state).await;
    let files_bytes: u64 = est.iter().map(|s| s.bytes).sum();
    let need = need_bytes(files_bytes, db_bytes);
    let free = crate::handlers::files::available_bytes(Path::new(format::SNAPSHOT_DIR));
    task.log(format!(
        "about {} of files and {} of databases; {} needed on the disk, {} free",
        human(files_bytes),
        human(db_bytes),
        human(need),
        free.map(human).unwrap_or_else(|| "unknown".into())
    ));
    if let Some(free) = free {
        if free < need {
            return Err(AgentError::Conflict(format!(
                "not enough free disk space: the snapshot needs about {} and {} is free{}",
                human(need),
                human(free),
                if req.include_drive { " (leaving Drive out makes it smaller)" } else { "" }
            )));
        }
    }

    let dumps = dumps::dump_all(state, staging, task).await?;

    let now = chrono::Utc::now();
    let stem = format::stem_for(&gathered.source.panel_host, now);
    let final_path = Path::new(format::SNAPSHOT_DIR).join(format::file_name(&stem));
    let part = final_path.with_file_name(format!("{}.part", format::file_name(&stem)));

    let mut inventory = gathered.inventory;
    inventory.mariadb_databases = dumps.mariadb_databases.clone();
    inventory.mariadb_users = dumps.mariadb_users;
    inventory.postgres_databases = dumps.postgres_databases.clone();

    let manifest = Manifest {
        kind: format::KIND.to_string(),
        format: format::FORMAT,
        created_at: now.to_rfc3339(),
        panel: gathered.panel,
        source: gathered.source,
        platform: gathered.platform,
        options: format::ExportOptions {
            include_drive: req.include_drive,
            paused_containers: if req.pause_docker { gathered.pause.clone() } else { vec![] },
        },
        crypto: format::CryptoInfo {
            scheme: "age-scrypt".into(),
            log_n: format::LOG_N,
            compression: "zstd-3".into(),
        },
        inventory,
        sections: sections
            .iter()
            .zip(&est)
            .map(|(s, e)| SectionInfo {
                id: s.id.to_string(),
                title: s.title.to_string(),
                paths: s.roots.iter().map(|r| r.path.to_string_lossy().into_owned()).collect(),
                est_bytes: e.bytes,
                est_files: e.files,
            })
            .collect(),
    };
    let dump_bytes: u64 = dumps
        .files
        .iter()
        .filter_map(|f| std::fs::metadata(&f.path).ok().map(|m| m.len()))
        .sum();
    Ok(Job {
        stem,
        part,
        final_path,
        staging: staging.to_path_buf(),
        manifest,
        system_files: gathered.system_files,
        dumps: dumps.files,
        sections,
        excludes,
        pause: if req.pause_docker { gathered.pause } else { vec![] },
        est_total: files_bytes + dump_bytes,
        notes: gathered.notes,
    })
}

/// The disk the snapshot needs: the files compress (0.9 is a careful
/// guess), the database dumps exist twice for a while (staged, then in the
/// file), and the floor stays free.
pub fn need_bytes(files: u64, databases: u64) -> u64 {
    files / 10 * 9 + databases / 10 * 6 + MIN_FREE_BYTES + 1024 * 1024 * 1024
}

fn write_snapshot(job: &Job, passphrase: &str, task: &TaskHandle) -> io::Result<Written> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&job.part)?;
    let threads = std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(1);
    let threads = threads.saturating_sub(1).clamp(1, 4);
    let mut w = SnapshotWriter::new(io::BufWriter::with_capacity(1024 * 1024, file), passphrase, format::LOG_N, threads)?;
    w.copy_sqlite_via(job.staging.clone());
    let written = w.written_counter();

    let mut index = Index::default();
    index.warnings.extend(job.notes.iter().cloned());

    w.append_bytes(format::MANIFEST, &serde_json::to_vec_pretty(&job.manifest).map_err(io::Error::other)?)?;
    w.append_bytes("README.txt", readme(&job.manifest).as_bytes())?;
    for (path, data) in &job.system_files {
        index
            .entries
            .insert(path.clone(), EntryHash { bytes: data.len() as u64, sha256: format::sha256_hex(data) });
        w.append_bytes(path, data)?;
    }
    let mut progress = Progress::new(job.est_total, task);
    for staged in &job.dumps {
        let hash = format::hash_file(&staged.path)?;
        let bytes = hash.bytes;
        index.entries.insert(staged.archive_path.clone(), hash);
        w.append_staged(&staged.archive_path, &staged.path)?;
        progress.add_done(bytes);
        progress.maybe_log("databases", 0, &written);
        // The dump is in the file: its staged copy can go now, which keeps
        // the disk the export needs closer to one copy.
        let _ = std::fs::remove_file(&staged.path);
    }

    let mut next_space_check = written.load(Ordering::Relaxed) + SPACE_CHECK_EVERY;
    for section in &job.sections {
        let mut pauser = if section.id == "docker" && !job.pause.is_empty() {
            Some(Pauser::pause(&job.pause, task))
        } else {
            None
        };
        let mut stats = TreeStats::default();
        let base = progress.done;
        for root in &section.roots {
            w.append_tree(root, &job.excludes, &mut stats, &mut |section_bytes| {
                progress.done = base + section_bytes;
                progress.maybe_log(section.title, section_bytes, &written);
                let now = written.load(Ordering::Relaxed);
                if now >= next_space_check {
                    next_space_check = now + SPACE_CHECK_EVERY;
                    if let Some(free) = crate::handlers::files::available_bytes(Path::new(format::SNAPSHOT_DIR)) {
                        if free < MIN_FREE_BYTES {
                            return Err(io::Error::other(format!(
                                "the disk is nearly full ({} free); the snapshot was stopped before it filled it",
                                human(free)
                            )));
                        }
                    }
                }
                Ok(())
            })?;
        }
        if let Some(p) = pauser.take() {
            p.unpause(task);
        }
        progress.done = base + stats.bytes;
        task.log(format!(
            "{}: {} in {} entries{}{}",
            section.title,
            human(stats.bytes),
            stats.files,
            if stats.skipped > 0 { format!(", {} skipped", stats.skipped) } else { String::new() },
            if stats.changed > 0 { format!(", {} changed while copied", stats.changed) } else { String::new() },
        ));
        for note in &stats.notes {
            task.log(format!("  {note}"));
        }
        index.warnings.extend(stats.notes.iter().map(|n| format!("{}: {n}", section.id)));
        index.sections.push(SectionDone {
            id: section.id.to_string(),
            bytes: stats.bytes,
            files: stats.files,
            skipped: stats.skipped,
            changed: stats.changed,
        });
    }

    index.completed = true;
    index.finished_at = chrono::Utc::now().to_rfc3339();
    w.append_bytes(format::INDEX, &serde_json::to_vec_pretty(&index).map_err(io::Error::other)?)?;
    let (buffered, size, sha256) = w.finish()?;
    let file = buffered.into_inner().map_err(|e| e.into_error())?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&job.part, &job.final_path)?;
    std::fs::File::open(format::SNAPSHOT_DIR)?.sync_all()?;
    Ok(Written { size, sha256, index })
}

async fn publish(job: &Job, done: Written, task: &TaskHandle) -> AgentResult<()> {
    let sidecar = Sidecar {
        name: job.stem.clone(),
        kind: format::KIND.to_string(),
        format: format::FORMAT,
        size: done.size,
        sha256: done.sha256.clone(),
        created_at: job.manifest.created_at.clone(),
        panel_commit: job.manifest.panel.commit.clone(),
        panel_dirty: job.manifest.panel.dirty,
        source_host: job.manifest.source.panel_host.clone(),
        include_drive: job.manifest.options.include_drive,
        sections: done.index.sections.clone(),
        origin: "local".into(),
    };
    let path = job.final_path.with_file_name(format!("{}.json", job.stem));
    crate::fs_atomic::write(&path, &serde_json::to_vec_pretty(&sidecar).unwrap_or_default(), 0o600).await?;
    task.log(format!(
        "snapshot {} written: {} (sha256 {})",
        format::file_name(&job.stem),
        human(done.size),
        done.sha256
    ));
    if !done.index.warnings.is_empty() {
        task.log(format!(
            "{} note(s) above: files that changed while copied or could not be read",
            done.index.warnings.len()
        ));
    }
    Ok(())
}

/// `progress 34% | 4.2 GiB of 12.3 GiB | Apps | 85 MB/s`, once per
/// PROGRESS_EVERY.
struct Progress<'a> {
    total: u64,
    done: u64,
    started: Instant,
    last: Instant,
    task: &'a TaskHandle,
}

impl<'a> Progress<'a> {
    fn new(total: u64, task: &'a TaskHandle) -> Self {
        let now = Instant::now();
        Progress { total, done: 0, started: now, last: now, task }
    }

    fn add_done(&mut self, n: u64) {
        self.done += n;
    }

    fn maybe_log(&mut self, what: &str, _section_bytes: u64, written: &std::sync::atomic::AtomicU64) {
        if self.last.elapsed() < PROGRESS_EVERY {
            return;
        }
        self.last = Instant::now();
        let pct = (self.done.min(self.total) * 100).checked_div(self.total).unwrap_or(0).min(99);
        let secs = self.started.elapsed().as_secs_f64().max(1.0);
        self.task.log(format!(
            "progress {pct}% | {} of {} | {what} | {}/s | file {}",
            human(self.done),
            human(self.total),
            human((self.done as f64 / secs) as u64),
            human(written.load(Ordering::Relaxed)),
        ));
    }
}

/// Freezes containers while their volumes are copied (a database inside
/// one would otherwise be copied mid-write), and always thaws them.
struct Pauser {
    names: Vec<String>,
    done: bool,
}

impl Pauser {
    fn pause(names: &[String], task: &TaskHandle) -> Pauser {
        let _ = std::fs::write(paused_marker(), serde_json::to_vec(names).unwrap_or_default());
        let mut paused = Vec::new();
        for name in names {
            match std::process::Command::new("docker").args(["pause", name]).output() {
                Ok(o) if o.status.success() => paused.push(name.clone()),
                _ => task.log(format!("container {name} could not be paused; its volumes are copied live")),
            }
        }
        if !paused.is_empty() {
            task.log(format!("paused while their volumes are copied: {}", paused.join(", ")));
        }
        Pauser { names: paused, done: false }
    }

    fn unpause(mut self, task: &TaskHandle) {
        self.thaw();
        if !self.names.is_empty() {
            task.log(format!("running again: {}", self.names.join(", ")));
        }
        self.done = true;
    }

    fn thaw(&mut self) {
        for name in &self.names {
            let _ = std::process::Command::new("docker").args(["unpause", name]).output();
        }
        let _ = std::fs::remove_file(paused_marker());
    }
}

impl Drop for Pauser {
    fn drop(&mut self) {
        if !self.done {
            self.thaw();
        }
    }
}

/// At agent start: half-written snapshots and staging folders of an export
/// that died with the agent go, and containers it froze run again. Pulled
/// files in `incoming/` stay: a pull resumes them.
pub async fn startup_cleanup() {
    let dir = Path::new(format::SNAPSHOT_DIR);
    if let Ok(text) = tokio::fs::read(paused_marker()).await {
        let names: Vec<String> = serde_json::from_slice(&text).unwrap_or_default();
        for name in &names {
            let _ = crate::exec::run("docker", ["unpause", name.as_str()]).await;
        }
        let _ = tokio::fs::remove_file(paused_marker()).await;
        if !names.is_empty() {
            tracing::warn!("unpaused containers an interrupted snapshot left paused: {}", names.join(", "));
        }
    }
    let Ok(mut rd) = tokio::fs::read_dir(dir).await else { return };
    while let Ok(Some(e)) = rd.next_entry().await {
        let name = e.file_name().to_string_lossy().into_owned();
        let path = e.path();
        if name.starts_with(".staging-") {
            let _ = tokio::fs::remove_dir_all(&path).await;
        } else if name.ends_with(".part") {
            let _ = tokio::fs::remove_file(&path).await;
        }
    }
}

async fn ensure_private_dir(path: &Path) -> AgentResult<()> {
    crate::fs_atomic::ensure_dir(path, 0o700).await?;
    crate::fs_atomic::chmod(path, 0o700).await?;
    crate::fs_atomic::chown_blocking(path, 0, 0)?;
    Ok(())
}

// ---------------------------------------------------------------- estimate

#[derive(Serialize, Clone, Debug)]
pub struct SectionEstimate {
    pub id: String,
    pub title: String,
    pub bytes: u64,
    pub files: u64,
}

#[derive(Serialize, Clone, Debug)]
pub struct Estimate {
    pub include_drive: bool,
    pub sections: Vec<SectionEstimate>,
    pub database_bytes: u64,
    pub total_bytes: u64,
    pub need_bytes: u64,
    pub free_bytes: Option<u64>,
    pub fits: bool,
    /// The Drive section's size even when it is left out, for the toggle.
    pub drive_bytes: u64,
}

async fn estimate_sections(sections: &[Section], excludes: &Excludes) -> Vec<SectionEstimate> {
    let sections = sections.to_vec();
    let excludes = excludes.clone();
    tokio::task::spawn_blocking(move || {
        sections
            .iter()
            .map(|s| {
                let (bytes, files) = plan::estimate(s, &excludes);
                SectionEstimate { id: s.id.to_string(), title: s.title.to_string(), bytes, files }
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

async fn database_bytes(state: &AppState) -> u64 {
    let mut total = 0u64;
    if let Ok(mut conn) = state.mariadb.pool().get_conn().await {
        total += conn
            .query_first::<u64, _>(
                "SELECT CAST(COALESCE(SUM(data_length + index_length), 0) AS UNSIGNED) FROM information_schema.tables \
                 WHERE table_schema NOT IN ('information_schema', 'performance_schema', 'mysql', 'sys')",
            )
            .await
            .ok()
            .flatten()
            .unwrap_or(0);
    }
    if crate::exec::run("systemctl", ["is-active", "--quiet", "postgresql"]).await.is_ok() {
        if let Ok(out) = crate::exec::run(
            "runuser",
            [
                "-u",
                "postgres",
                "--",
                "psql",
                "-XAt",
                "-c",
                "SELECT COALESCE(SUM(pg_database_size(datname)), 0) FROM pg_database WHERE datallowconn",
            ],
        )
        .await
        {
            total += out.stdout.trim().parse::<u64>().unwrap_or(0);
        }
    }
    total
}

type Cached = Option<(Instant, bool, Estimate)>;

fn estimate_cache() -> &'static std::sync::Mutex<Cached> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Cached>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(None))
}

/// Walks everything a snapshot would hold, so it is kept for 5 minutes.
pub async fn estimate(state: &AppState, include_drive: bool) -> Estimate {
    if let Some((at, drive, est)) = estimate_cache().lock().unwrap().clone() {
        if drive == include_drive && at.elapsed() < Duration::from_secs(300) {
            let mut est = est;
            est.free_bytes = crate::handlers::files::available_bytes(Path::new(format::SNAPSHOT_DIR))
                .or_else(|| crate::handlers::files::available_bytes(Path::new("/var/backups")));
            est.fits = est.free_bytes.is_none_or(|f| f >= est.need_bytes);
            return est;
        }
    }
    let gathered = inventory::gather(state).await;
    let all = plan::plan_sections(&gathered.facts, &PlanOptions { include_drive: true });
    let excludes = Excludes::default();
    let est_all = estimate_sections(&all, &excludes).await;
    let drive_bytes = est_all.iter().filter(|s| s.id == "drive").map(|s| s.bytes).sum();
    let sections: Vec<SectionEstimate> =
        est_all.into_iter().filter(|s| include_drive || s.id != "drive").collect();
    let database_bytes = database_bytes(state).await;
    let files: u64 = sections.iter().map(|s| s.bytes).sum();
    let need = need_bytes(files, database_bytes);
    let free_bytes = crate::handlers::files::available_bytes(Path::new(format::SNAPSHOT_DIR))
        .or_else(|| crate::handlers::files::available_bytes(Path::new("/var/backups")));
    let est = Estimate {
        include_drive,
        sections,
        database_bytes,
        total_bytes: files + database_bytes,
        need_bytes: need,
        free_bytes,
        fits: free_bytes.is_none_or(|f| f >= need),
        drive_bytes,
    };
    *estimate_cache().lock().unwrap() = Some((Instant::now(), include_drive, est.clone()));
    est
}

fn readme(m: &Manifest) -> String {
    format!(
        "vps-panel server snapshot\n\
         =========================\n\
         \n\
         Made {created} on {host} by panel commit {commit}.\n\
         \n\
         Normally a freshly installed panel imports this file (Backups > Server\n\
         snapshot). Without the panel, on a Linux machine:\n\
         \n\
         \x20 apt install age zstd\n\
         \x20 mkdir /root/snap && age -d FILE.vpssnap.tar.zst.age | zstd -dc | tar -xpf - -C /root/snap\n\
         \x20 (age asks for the passphrase)\n\
         \n\
         Inside:\n\
         \x20 manifest.json             what the server held\n\
         \x20 db/mariadb/users.sql      MariaDB accounts and grants: mariadb < users.sql\n\
         \x20 db/mariadb/NAME.sql.zst   one database each: zstd -dc NAME.sql.zst | mariadb\n\
         \x20 db/mariadb/pdns|mail.data.sql.zst   the panel's DNS zones and mailboxes (rows only)\n\
         \x20 db/postgres/roles.sql     PostgreSQL roles: runuser -u postgres -- psql -f roles.sql\n\
         \x20 db/postgres/NAME.dump     pg_restore --create -d postgres NAME.dump\n\
         \x20 panel/panel.db            the panel's own database (users, apps, sites, settings)\n\
         \x20 fs/...                    files under their original paths (fs/srv/apps, fs/var/vmail, ...)\n\
         \x20 system/*.json             accounts, Docker, Tailscale, firewall, swap\n\
         \x20 index.json                checksums of the dumps and what was copied\n\
         \n\
         The file holds every password and key of the server. Delete it when the\n\
         move is done.\n",
        created = m.created_at,
        host = m.source.panel_host,
        commit = m.panel.commit,
    )
}

pub fn human(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn need_counts_the_floor_and_both_copies_of_dumps() {
        let gib = 1024 * 1024 * 1024u64;
        assert_eq!(need_bytes(0, 0), 2 * gib);
        assert_eq!(need_bytes(10 * gib, 0), 9 * gib + 2 * gib);
        assert_eq!(need_bytes(0, 10 * gib), 6 * gib + 2 * gib);
    }

    #[test]
    fn human_sizes() {
        assert_eq!(human(512), "512 B");
        assert_eq!(human(1536), "1.5 KiB");
        assert_eq!(human(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }
}
