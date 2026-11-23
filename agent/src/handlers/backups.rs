use crate::error::{AgentError, AgentResult};
use crate::exec;
use crate::ipc::TaskAccepted;
use crate::state::AppState;
use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
pub struct Snapshot {
    pub id: String,
    pub time: String,
    pub paths: Vec<String>,
    pub hostname: String,
}

#[derive(Deserialize)]
pub struct RunReq {
    #[serde(default)]
    pub paths: Option<Vec<String>>,
}

#[derive(Deserialize)]
pub struct RestoreReq {
    pub snapshot: String,
    /// A new folder under RESTORE_ROOT; empty picks
    /// `<RESTORE_ROOT>/<snapshot>-<UTC time>`.
    #[serde(default)]
    pub target: String,
}

/// Restores only ever land here, in a folder of their own, where the Files
/// page can browse them. Restoring over live paths (any absolute path was
/// accepted, `/` included) overwrote running config with old copies in one
/// step and could not be undone.
const RESTORE_ROOT: &str = "/srv/drive/restores";

const DEFAULT_PATHS: &[&str] = &[
    "/etc/nginx/sites-available",
    "/etc/systemd/system",
    "/etc/vps-panel",
    "/etc/postfix",
    "/etc/dovecot",
    "/etc/rspamd",
    "/etc/powerdns",
    "/var/lib/vps-panel",
    "/var/vmail",
    "/srv/apps",
    // The operator's own files (/files → Drive). Nothing else on the box
    // holds a second copy of these, which makes them the content most worth
    // having in restic.
    crate::handlers::files::DRIVE_DIR,
];

/// Docker's named volumes: the quick-install containers keep their data here
/// (n8n's workflows and credentials, pgAdmin, Qdrant, Roundcube's settings and
/// address books), and none of it was in any snapshot. Only boxes with Docker
/// have it, and restic fails a backup on a missing path, so it joins the
/// default set only when it exists. Copied live, like /srv/apps.
const DOCKER_VOLUMES: &str = "/var/lib/docker/volumes";

/// What a backup without an explicit path list covers. The dump folder is
/// made by the dumps, which run first; like Docker's volumes it is only named
/// when it is there.
fn default_backup_paths() -> Vec<String> {
    let mut paths: Vec<String> = DEFAULT_PATHS.iter().map(|s| s.to_string()).collect();
    for optional in [DUMP_ROOT, DOCKER_VOLUMES] {
        if std::path::Path::new(optional).is_dir() {
            paths.push(optional.to_string());
        }
    }
    paths
}

/// SECURITY: a requested backup path must be one of the panel's known backup
/// roots, or a path inside one. Without this guard a caller could back up
/// /etc/shadow, /root/.ssh, /etc/letsencrypt (TLS private keys), etc. into the
/// restic repo and later exfiltrate them. The public MCP `backups.run` tool no
/// longer forwards `paths` at all; this protects the remaining panel/admin and
/// any direct agent callers.
fn validate_backup_path(p: &str) -> AgentResult<()> {
    if !p.starts_with('/')
        || p.split('/').any(|seg| seg == "..")
        || p.chars().any(|c| c.is_control())
    {
        return Err(AgentError::Invalid(format!("invalid backup path: {p}")));
    }
    let allowed = DEFAULT_PATHS
        .iter()
        .chain([&DUMP_ROOT, &DOCKER_VOLUMES])
        .any(|root| p == *root || p.starts_with(&format!("{root}/")));
    if !allowed {
        return Err(AgentError::Invalid(format!(
            "backup path '{p}' is not under an allowed backup root"
        )));
    }
    Ok(())
}

/// SECURITY: restic `restore <snapshot> --target <target>` runs as root and
/// writes the archived tree under `target`. An unvalidated target='/' would
/// overwrite /etc, /srv/apps, systemd units, etc.; a snapshot beginning with
/// '-' would be parsed by restic as a flag (argument injection). Restrict the
/// snapshot to a restic id / "latest" and the target to a clean absolute path.
/// Returns the folder to restore into.
fn validate_restore(snapshot: &str, target: &str) -> AgentResult<String> {
    let snap_ok = snapshot == "latest"
        || (!snapshot.is_empty() && snapshot.chars().all(|c| c.is_ascii_hexdigit()));
    if !snap_ok {
        return Err(AgentError::Invalid(
            "invalid snapshot id (expected a restic id or 'latest')".into(),
        ));
    }
    if target.trim().is_empty() {
        let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
        return Ok(format!("{RESTORE_ROOT}/{snapshot}-{stamp}"));
    }
    // Only after the empty check: "/" trims to "" and must be refused, not
    // read as "no target".
    let target = target.trim().trim_end_matches('/');
    let Some(name) = target.strip_prefix(&format!("{RESTORE_ROOT}/")) else {
        return Err(AgentError::Invalid(format!(
            "restores go into a new folder under {RESTORE_ROOT}/, not over live paths"
        )));
    };
    if name.is_empty()
        || name.split('/').any(|seg| seg.is_empty() || seg == "." || seg == "..")
        || name.chars().any(|c| c.is_control())
    {
        return Err(AgentError::Invalid("invalid restore folder name".into()));
    }
    if std::path::Path::new(target).exists() {
        return Err(AgentError::Conflict(format!(
            "{target} already exists - restore into a new folder"
        )));
    }
    Ok(target.to_string())
}

pub async fn list(State(state): State<AppState>) -> AgentResult<Json<Vec<Snapshot>>> {
    if !is_repo_initialized(&state).await? {
        return Ok(Json(Vec::new()));
    }
    let env = repo_env(&state);
    let out = exec::run_envs(
        "restic",
        ["snapshots", "--no-lock", "--json"],
        env,
    )
    .await?;
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&out.stdout).unwrap_or_default();
    let mut snapshots = Vec::with_capacity(parsed.len());
    for s in parsed {
        snapshots.push(Snapshot {
            id: s.get("short_id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            time: s.get("time").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            paths: s
                .get("paths")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default(),
            hostname: s.get("hostname").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        });
    }
    Ok(Json(snapshots))
}

pub async fn run(
    State(state): State<AppState>,
    Json(req): Json<RunReq>,
) -> AgentResult<Json<TaskAccepted>> {
    if let Some(paths) = &req.paths {
        for p in paths {
            validate_backup_path(p)?;
        }
    }
    if state.tasks.any_live_with_prefix("backups.") {
        return Err(AgentError::Conflict("a backup or restore is already running".into()));
    }
    let task = state.tasks.create("backups.run");
    let task_id = task.id.clone();
    let cfg = state.config.clone();
    let panel_db = state.panel_db.clone();
    tokio::spawn(async move {
        task.start();
        if let Err(e) = ensure_repo(&cfg).await {
            task.log(format!("cannot open the backup repository: {e}"));
            task.finish(false);
            return;
        }
        if adopt_old_dump_root(std::path::Path::new(OLD_DUMP_ROOT), std::path::Path::new(DUMP_ROOT)).await {
            task.log(format!("moved {OLD_DUMP_ROOT} to {DUMP_ROOT}: database dumps are written there now"));
        }
        // A failed dump fails the task (it used to be a warning under a
        // "succeeded" task), but the files are still backed up.
        let mut problems: Vec<String> = Vec::new();
        if let Err(e) = dump_mariadb(&task).await {
            problems.push(format!("MariaDB dump failed: {e}"));
        }
        if let Err(e) = dump_postgres(&task).await {
            problems.push(format!("PostgreSQL dump failed: {e}"));
        }
        if let Err(e) = dump_panel_db(&panel_db, &task).await {
            problems.push(format!("panel.db copy failed: {e}"));
        }
        for p in &problems {
            task.log(p.clone());
        }
        let dumps_ok = problems.is_empty();
        let paths: Vec<String> = req.paths.clone().unwrap_or_else(default_backup_paths);
        let mut args: Vec<String> = vec!["backup".into(), "--json".into()];
        for exclude in BACKUP_EXCLUDES {
            args.push(format!("--exclude={exclude}"));
        }
        for p in &paths { args.push(p.clone()); }
        let env = repo_env_owned(&cfg);
        let env_refs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let mut status = StatusThrottle::default();
        let code = exec::run_streaming_envs("restic", args.iter().map(|s| s.as_str()), env_refs, |line| {
            if status.keep(line, std::time::Instant::now()) {
                task.log(line.to_string());
            }
        })
        .await
        .unwrap_or(-1);
        if code == 3 {
            // Exit 3: the snapshot was made, but some files could not be read
            // (/srv/apps and Docker volumes are copied live, and files there
            // change or vanish meanwhile). It still counts as a problem, but
            // ending here skipped forget/prune: on a server where this
            // happens every run, no snapshot was ever removed.
            problems.push(
                "restic could not read some files, so this snapshot is incomplete (see its error lines above)"
                    .to_string(),
            );
        } else if code != 0 {
            task.log(format!("restic backup failed (exit {code})"));
            task.finish(false);
            return;
        }
        // The dumps of this run are in a snapshot from their new folder: what
        // an earlier version left in the old one, where it could not be
        // moved, can go.
        if dumps_ok && paths.iter().any(|p| p == DUMP_ROOT) && remove_old_dump_root().await {
            task.log(format!(
                "removed {OLD_DUMP_ROOT}: database dumps are written to {DUMP_ROOT} now"
            ));
        }
        // The repository lives on the same disk by default: without pruning
        // every snapshot was kept forever until the disk filled.
        task.log(format!("restic forget {} --prune", RETENTION.join(" ")));
        let mut forget: Vec<&str> = vec!["forget"];
        forget.extend_from_slice(RETENTION);
        forget.push("--prune");
        let env_refs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let prune_code = exec::run_streaming_envs("restic", forget, env_refs, |line| {
            task.log(line.to_string());
        })
        .await
        .unwrap_or(-1);
        if prune_code != 0 {
            problems.push(format!("restic forget/prune failed (exit {prune_code}); old snapshots were kept"));
        }
        if problems.is_empty() {
            task.finish(true);
        } else {
            task.log(format!("backup finished with problems: {}", problems.join("; ")));
            task.finish(false);
        }
    });
    Ok(Json(TaskAccepted { task_id }))
}

/// How often one of restic's progress lines goes into the task log.
const STATUS_EVERY: std::time::Duration = std::time::Duration::from_secs(10);

/// `restic backup --json` prints a `status` line many times a second for as
/// long as it runs. Each one went into the task log, and the whole log is
/// rewritten to panel.db every two seconds: an hour-long first backup left
/// hundreds of thousands of lines there and in the browser. One status line
/// per STATUS_EVERY is kept; every other line (errors, the summary) always.
#[derive(Default)]
struct StatusThrottle {
    last: Option<std::time::Instant>,
}

impl StatusThrottle {
    fn keep(&mut self, line: &str, now: std::time::Instant) -> bool {
        if !line.starts_with(r#"{"message_type":"status""#) {
            return true;
        }
        if self.last.is_some_and(|t| now.duration_since(t) < STATUS_EVERY) {
            return false;
        }
        self.last = Some(now);
        true
    }
}

/// Snapshots kept after each backup (restic forget). Counted per host, not
/// per host and list of paths, which is restic's default: the snapshots made
/// before a path joined the list (the dump folder, Docker's volumes) were a
/// group of their own that no later backup added to, so its last seven days
/// and six months were kept for ever.
const RETENTION: &[&str] = &[
    "--group-by", "host", "--keep-daily", "7", "--keep-weekly", "4", "--keep-monthly", "6",
];

/// Rebuildable or transient trees left out of every snapshot.
const BACKUP_EXCLUDES: &[&str] = &[
    "node_modules",
    "**/.next/cache",
    "/var/lib/vps-panel/build-snapshots",
];

/// Where database dumps are written before restic picks them up; a backup
/// root of its own. Root-only, and under a folder only root can write. Not
/// inside /var/backups/vps-panel next to it: that is the restic repository,
/// which operators copy to other machines, and a dump is every database in
/// plain text.
const DUMP_ROOT: &str = "/var/backups/vps-panel-db-dumps";

/// Where the dumps were until they moved. /var/lib/vps-panel belongs to the
/// panel's user, who could not read the root-only folder but could rename it
/// away and put one of its own, or a link, in its place just before a backup
/// wrote the next dump there. Snapshots from that time still hold the dumps
/// under this path, and a restore puts them back under it.
const OLD_DUMP_ROOT: &str = "/var/lib/vps-panel/db-dumps";

/// Moves the dump folder of an earlier version to where it is now, with the
/// dumps in it: written next to them, the new dumps needed their size a
/// second time, and on a full disk every backup from then on would have
/// failed at the dump. Only a real folder of the agent's own user (root) is
/// moved, since the panel's user can put anything else in that place, and
/// only when nothing is at the new place yet. Across filesystems the rename
/// fails and the old folder stays until a backup has succeeded. True when it
/// was moved.
async fn adopt_old_dump_root(old: &std::path::Path, new: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let ours = |m: &std::fs::Metadata| m.is_dir() && m.uid() == nix::unistd::getuid().as_raw();
    if tokio::fs::symlink_metadata(new).await.is_ok() {
        return false;
    }
    if !tokio::fs::symlink_metadata(old).await.is_ok_and(|m| ours(&m)) {
        return false;
    }
    if tokio::fs::rename(old, new).await.is_err() {
        return false;
    }
    // The old place can be changed between the look and the move: what
    // arrived is looked at again, and anything else is thrown out.
    if tokio::fs::symlink_metadata(new).await.is_ok_and(|m| ours(&m)) {
        return true;
    }
    if tokio::fs::remove_dir_all(new).await.is_err() {
        let _ = tokio::fs::remove_file(new).await;
    }
    false
}

/// Deletes the old dump folder; true when there was one. A link put in its
/// place is removed itself, never followed.
async fn remove_old_dump_root() -> bool {
    if tokio::fs::symlink_metadata(OLD_DUMP_ROOT).await.is_err() {
        return false;
    }
    match tokio::fs::remove_dir_all(OLD_DUMP_ROOT).await {
        Ok(()) => true,
        // Not a directory: a file somebody put there.
        Err(_) => tokio::fs::remove_file(OLD_DUMP_ROOT).await.is_ok(),
    }
}

/// Where a restored snapshot has its database dumps: new location, then the
/// one snapshots from before the move have.
fn restored_dump_dirs(target: &str) -> [String; 2] {
    [format!("{target}{DUMP_ROOT}"), format!("{target}{OLD_DUMP_ROOT}")]
}

/// Both levels are made root's as well as 0700, whatever they were before: a
/// folder another user owns stays readable to it whatever its mode.
async fn private_dump_dir(name: &str) -> AgentResult<std::path::PathBuf> {
    let root = std::path::Path::new(DUMP_ROOT);
    crate::fs_atomic::ensure_dir(root, 0o700).await?;
    crate::fs_atomic::chmod(root, 0o700).await?;
    crate::fs_atomic::chown_blocking(root, 0, 0)?;
    let dir = root.join(name);
    crate::fs_atomic::ensure_dir(&dir, 0o700).await?;
    crate::fs_atomic::chmod(&dir, 0o700).await?;
    crate::fs_atomic::chown_blocking(&dir, 0, 0)?;
    Ok(dir)
}

/// Deletes the previous dump so the new one is a new file of root's. Written
/// over in place it kept its owner (the panel user after that chown -R) and
/// any hard link made to it while the folder was open.
async fn remove_old_dump(path: &std::path::Path) {
    let _ = tokio::fs::remove_file(path).await;
}

/// pg_dumpall as the postgres user, when PostgreSQL is installed. Databases
/// created on /pg were never backed up.
async fn dump_postgres(task: &crate::ipc::tasks::TaskHandle) -> AgentResult<()> {
    if exec::run("systemctl", ["is-active", "--quiet", "postgresql"]).await.is_err() {
        task.log("PostgreSQL is not running here - skipping its dump");
        return Ok(());
    }
    let dir = private_dump_dir("postgres").await?;
    let path = dir.join("all-databases.sql");
    remove_old_dump(&path).await;
    task.log(format!("pg_dumpall -> {}", path.display()));
    let target = path.to_string_lossy().into_owned();
    // runuser drops to postgres for the dump; root's shell writes the file
    // into the root-only directory.
    exec::run(
        "sh",
        [
            "-c",
            r#"umask 077 && runuser -u postgres -- pg_dumpall > "$1""#,
            "pg-dump",
            target.as_str(),
        ],
    )
    .await
    .map(|_| ())
}

/// A consistent copy of panel.db (VACUUM INTO). Copying the live file next
/// to its WAL could capture a state that does not restore cleanly.
async fn dump_panel_db(
    panel_db: &crate::db::PanelDb,
    task: &crate::ipc::tasks::TaskHandle,
) -> AgentResult<()> {
    let dir = private_dump_dir("panel").await?;
    let path = dir.join("panel.db");
    let _ = tokio::fs::remove_file(&path).await;
    task.log(format!("panel.db -> {}", path.display()));
    let target = path.to_string_lossy().into_owned();
    panel_db.with(|conn| conn.execute("VACUUM INTO ?1", rusqlite::params![target]).map(|_| ()))
}

pub async fn restore(
    State(state): State<AppState>,
    Json(req): Json<RestoreReq>,
) -> AgentResult<Json<TaskAccepted>> {
    let target = validate_restore(&req.snapshot, &req.target)?;
    if state.tasks.any_live_with_prefix("backups.") {
        return Err(AgentError::Conflict("a backup or restore is already running".into()));
    }
    // Restored files keep their original owners and modes; the root-only
    // parent keeps copies of secrets away from other local users.
    crate::fs_atomic::ensure_dir(std::path::Path::new(RESTORE_ROOT), 0o700).await?;
    let task = state.tasks.create(&format!("backups.restore:{}", req.snapshot));
    let task_id = task.id.clone();
    let cfg = state.config.clone();
    tokio::spawn(async move {
        task.start();
        remove_stale_locks(&cfg).await;
        let env = repo_env_owned(&cfg);
        let env_refs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        task.log(format!("restoring snapshot {} into {target}", req.snapshot));
        let code = exec::run_streaming_envs(
            "restic",
            ["restore", &req.snapshot, "--target", &target],
            env_refs,
            |line| task.log(line.to_string()),
        )
        .await
        .unwrap_or(-1);
        // Older snapshots have the dumps in another place than newer ones.
        for dir in restored_dump_dirs(&target) {
            if std::path::Path::new(&dir).is_dir() {
                task.log(format!("database dumps of this snapshot: {dir}"));
            }
        }
        task.finish(code == 0);
    });
    Ok(Json(TaskAccepted { task_id }))
}

async fn is_repo_initialized(state: &AppState) -> AgentResult<bool> {
    let env = repo_env(state);
    // --no-lock: reading the list needs none, and with one the page showed
    // "No snapshots yet" for as long as a prune held its exclusive lock.
    match exec::run_envs("restic", ["snapshots", "--no-lock", "--last", "--json"], env).await {
        Ok(_) => Ok(true),
        Err(_) => Ok(false),
    }
}

/// Removes the locks a killed restic left in the repository (the OOM killer
/// during a prune, a power cut, an agent stopped mid-run). `restic unlock`
/// only removes stale ones: of a process that is gone, or not refreshed for
/// 30 minutes. With such a lock every later `forget --prune` failed with
/// "repository is already locked", and after a killed prune so did the
/// backup itself (as a misleading "init error") and every restore.
async fn remove_stale_locks(cfg: &crate::config::Config) {
    // Fails when there is no repository yet; ensure_repo creates it.
    let _ = exec::run_envs("restic", ["unlock"], repo_env_from_cfg(cfg)).await;
}

async fn ensure_repo(cfg: &crate::config::Config) -> AgentResult<()> {
    remove_stale_locks(cfg).await;
    ensure_repo_at(&cfg.restic_repository, repo_env_from_cfg(cfg)).await
}

/// Creates the repository when there is none. Every failure of `restic
/// snapshots` used to lead to `restic init`: with a wrong RESTIC_PASSWORD, a
/// repository that could not be reached or one that was locked, the backup
/// then ended with init's "config file already exists" and the reason was
/// never shown.
async fn ensure_repo_at(repository: &str, env: Vec<(&'static str, String)>) -> AgentResult<()> {
    let failed = match exec::run_envs("restic", ["snapshots", "--latest", "1"], env.clone()).await {
        Ok(_) => return Ok(()),
        Err(e) => e,
    };
    if !repo_is_missing(&failed, repository) {
        return Err(failed);
    }
    let _ = tokio::fs::create_dir_all(repository).await;
    match exec::run_envs("restic", ["init"], env).await {
        Ok(_) => Ok(()),
        Err(init) => Err(AgentError::Other(anyhow::anyhow!(
            "{init} (restic init, tried because of: {failed})"
        ))),
    }
}

/// Repository locations that are not a folder on this machine (restic's
/// backend prefixes).
const REMOTE_BACKENDS: &[&str] = &["sftp:", "s3:", "b2:", "azure:", "gs:", "swift:", "rest:", "rclone:"];

/// True when a failed `restic snapshots` means there is no repository yet.
/// restic 0.17 and later exit with 10 for exactly that. 0.16, which Ubuntu
/// 24.04 has, exits with 1 for everything and says "unable to open config
/// file ... Is there a repository at the following location?" when it cannot
/// read the repository's `config`, also when that is the network's fault. So
/// for a local repository the folder itself is asked: no `config` and no keys
/// in it. A remote one is taken at restic's word; `init` refuses a repository
/// that exists.
fn repo_is_missing(failed: &AgentError, repository: &str) -> bool {
    let AgentError::Subprocess { code, stderr, .. } = failed else {
        return false;
    };
    if *code != 10 && !stderr.contains("unable to open config file") {
        return false;
    }
    if REMOTE_BACKENDS.iter().any(|b| repository.starts_with(b)) {
        return true;
    }
    let dir = std::path::Path::new(repository.strip_prefix("local:").unwrap_or(repository));
    let no_config = matches!(
        std::fs::symlink_metadata(dir.join("config")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
    );
    // Keys without a config are what is left of a damaged repository; a new
    // one made on top of it would hide that.
    let no_keys = std::fs::read_dir(dir.join("keys")).map_or(true, |mut d| d.next().is_none());
    no_config && no_keys
}

async fn dump_mariadb(task: &crate::ipc::tasks::TaskHandle) -> AgentResult<()> {
    let dump_dir = private_dump_dir("mariadb").await?;
    // The old location was group-readable; drop its copy.
    let _ = tokio::fs::remove_dir_all("/var/lib/vps-panel/mariadb-dump").await;
    let dump_path = dump_dir.join("all-databases.sql");
    remove_old_dump(&dump_path).await;
    task.log(format!("mariadb-dump -> {}", dump_path.display()));
    exec::run(
        "mariadb-dump",
        [
            "--all-databases",
            "--single-transaction",
            "--quick",
            "--lock-tables=0",
            "--result-file",
            dump_path.to_str().unwrap(),
        ],
    )
    .await
    .map(|_| ())
}

fn repo_env(state: &AppState) -> Vec<(&'static str, String)> {
    repo_env_from_cfg(&state.config)
}

fn repo_env_owned(cfg: &crate::config::Config) -> Vec<(String, String)> {
    repo_env_from_cfg(cfg)
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
}

fn repo_env_from_cfg(cfg: &crate::config::Config) -> Vec<(&'static str, String)> {
    vec![
        ("RESTIC_PASSWORD", cfg.restic_password.clone()),
        ("RESTIC_REPOSITORY", cfg.restic_repository.clone()),
    ]
}

#[cfg(test)]
mod tests {
    use super::{
        adopt_old_dump_root, ensure_repo_at, repo_is_missing, restored_dump_dirs, validate_backup_path,
        validate_restore, StatusThrottle, DUMP_ROOT, OLD_DUMP_ROOT, RESTORE_ROOT, RETENTION, STATUS_EVERY,
    };
    use crate::error::AgentError;

    #[test]
    fn restores_only_into_a_new_folder_under_the_restore_root() {
        assert!(validate_restore("latest", "/").is_err());
        assert!(validate_restore("latest", "/etc").is_err());
        assert!(validate_restore("latest", RESTORE_ROOT).is_err());
        assert!(validate_restore("latest", "/srv/drive/restores/../etc").is_err());
        assert!(validate_restore("latest", "/srv/drive/restores//x").is_err());
        assert_eq!(
            validate_restore("abc123", "/srv/drive/restores/before-upgrade/").unwrap(),
            "/srv/drive/restores/before-upgrade"
        );
        assert!(validate_restore("abc123", "")
            .unwrap()
            .starts_with("/srv/drive/restores/abc123-"));
        assert!(validate_restore("not-hex!", "").is_err());
    }

    #[test]
    fn restic_progress_lines_are_thinned_out_and_nothing_else() {
        let status = r#"{"message_type":"status","percent_done":0.5,"total_files":10,"files_done":5}"#;
        let error = r#"{"message_type":"error","error":{},"during":"archival","item":"/srv/apps/x"}"#;
        let summary = r#"{"message_type":"summary","files_new":3,"snapshot_id":"abc123"}"#;
        let start = std::time::Instant::now();
        let mut t = StatusThrottle::default();
        assert!(t.keep(status, start));
        assert!(!t.keep(status, start + STATUS_EVERY / 2));
        assert!(t.keep(error, start + STATUS_EVERY / 2));
        assert!(t.keep(summary, start + STATUS_EVERY / 2));
        assert!(t.keep("plain text from restic", start + STATUS_EVERY / 2));
        assert!(t.keep(status, start + STATUS_EVERY));
        assert!(!t.keep(status, start + STATUS_EVERY + STATUS_EVERY / 2));
    }

    #[test]
    fn docker_volumes_are_a_backup_root_but_not_the_rest_of_docker() {
        assert!(validate_backup_path("/var/lib/docker/volumes").is_ok());
        assert!(validate_backup_path("/var/lib/docker/volumes/n8n_data/_data").is_ok());
        assert!(validate_backup_path("/srv/apps/shop").is_ok());
        for bad in ["/var/lib/docker", "/var/lib/docker/image", "/var/lib/docker/volumes/../containers", "/etc/shadow"] {
            assert!(validate_backup_path(bad).is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn dumps_have_a_backup_root_of_their_own_outside_the_repository() {
        assert_eq!(DUMP_ROOT, "/var/backups/vps-panel-db-dumps");
        assert!(validate_backup_path(DUMP_ROOT).is_ok());
        assert!(validate_backup_path("/var/backups/vps-panel-db-dumps/mariadb").is_ok());
        // The repository itself and the rest of /var/backups are not.
        for bad in ["/var/backups", "/var/backups/vps-panel", "/var/backups/vps-panel-db-dumps-old"] {
            assert!(validate_backup_path(bad).is_err(), "{bad} should be refused");
        }
        // The old place is still inside a backup root; snapshots made before
        // the move restore their dumps under it.
        assert!(validate_backup_path(OLD_DUMP_ROOT).is_ok());
        assert_eq!(
            restored_dump_dirs("/srv/drive/restores/abc123-20260101-000000"),
            [
                "/srv/drive/restores/abc123-20260101-000000/var/backups/vps-panel-db-dumps",
                "/srv/drive/restores/abc123-20260101-000000/var/lib/vps-panel/db-dumps",
            ]
        );
        // One retention for all of this host's snapshots, whatever paths they
        // were made with.
        assert_eq!(RETENTION[..2], ["--group-by", "host"]);
    }

    #[tokio::test]
    async fn the_old_dump_folder_moves_to_the_new_place_once() {
        let dir = tempfile::tempdir().unwrap();
        let (old, new) = (dir.path().join("db-dumps"), dir.path().join("vps-panel-db-dumps"));
        // Nothing to move.
        assert!(!adopt_old_dump_root(&old, &new).await);
        std::fs::create_dir_all(old.join("mariadb")).unwrap();
        std::fs::write(old.join("mariadb/all-databases.sql"), "-- dump").unwrap();
        assert!(adopt_old_dump_root(&old, &new).await);
        assert!(!old.exists());
        assert_eq!(std::fs::read_to_string(new.join("mariadb/all-databases.sql")).unwrap(), "-- dump");

        // Never over what is at the new place, and never a link.
        std::fs::create_dir(&old).unwrap();
        assert!(!adopt_old_dump_root(&old, &new).await);
        assert!(old.is_dir());
        std::fs::remove_dir_all(&new).unwrap();
        std::fs::remove_dir(&old).unwrap();
        std::os::unix::fs::symlink(dir.path(), &old).unwrap();
        assert!(!adopt_old_dump_root(&old, &new).await);
        assert!(!new.exists());
        assert!(old.is_symlink());
    }

    fn restic_failed(code: i32, stderr: &str) -> AgentError {
        AgentError::Subprocess { cmd: "restic".into(), code, stderr: stderr.into() }
    }

    #[test]
    fn a_repository_is_only_created_where_there_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let repo_str = repo.to_str().unwrap();
        // restic 0.16.4, word for word.
        let no_repo = restic_failed(
            1,
            &format!("Fatal: unable to open config file: stat {repo_str}/config: no such file or directory\nIs there a repository at the following location?\n{repo_str}\n"),
        );
        let wrong_password = restic_failed(1, "Fatal: wrong password or no key found\n");
        let locked = restic_failed(
            1,
            "unable to create lock in backend: repository is already locked by PID 4242 on host by root (UID 0, GID 0)\n",
        );

        // No folder, then an empty one (what the installer leaves).
        assert!(repo_is_missing(&no_repo, repo_str));
        std::fs::create_dir(&repo).unwrap();
        assert!(repo_is_missing(&no_repo, repo_str));
        assert!(repo_is_missing(&no_repo, &format!("local:{repo_str}")));
        // restic 0.17+ has an exit code for it.
        assert!(repo_is_missing(&restic_failed(10, "Fatal: repository does not exist\n"), repo_str));

        assert!(!repo_is_missing(&wrong_password, repo_str));
        assert!(!repo_is_missing(&restic_failed(12, "Fatal: wrong password or no key found\n"), repo_str));
        assert!(!repo_is_missing(&locked, repo_str));
        assert!(!repo_is_missing(&restic_failed(11, "repository is already locked\n"), repo_str));
        // restic itself is not installed.
        let not_installed = AgentError::Io(std::io::Error::from(std::io::ErrorKind::NotFound));
        assert!(!repo_is_missing(&not_installed, repo_str));

        // A repository that lost its config still has its keys: not empty.
        std::fs::create_dir(repo.join("keys")).unwrap();
        assert!(repo_is_missing(&no_repo, repo_str));
        std::fs::write(repo.join("keys").join("0a1b2c"), "{}").unwrap();
        assert!(!repo_is_missing(&no_repo, repo_str));
        // And one whose config is there was not missing, whatever restic said.
        std::fs::remove_dir_all(repo.join("keys")).unwrap();
        std::fs::write(repo.join("config"), "x").unwrap();
        assert!(!repo_is_missing(&no_repo, repo_str));

        // A remote repository cannot be looked at: restic's word counts.
        assert!(repo_is_missing(&no_repo, "sftp:backup@host:/srv/restic"));
        assert!(!repo_is_missing(&wrong_password, "sftp:backup@host:/srv/restic"));
    }

    /// The same against the real restic (0.16.4 is what Ubuntu 24.04 has).
    #[tokio::test]
    #[ignore = "needs restic (cargo test -- --ignored)"]
    async fn a_wrong_password_is_reported_and_a_missing_repository_created() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let repo_str = repo.to_str().unwrap();
        let env = |password: &str| {
            vec![("RESTIC_PASSWORD", password.to_string()), ("RESTIC_REPOSITORY", repo_str.to_string())]
        };
        ensure_repo_at(repo_str, env("right")).await.unwrap();
        assert!(repo.join("config").is_file());
        // A second run finds it.
        ensure_repo_at(repo_str, env("right")).await.unwrap();

        let wrong = ensure_repo_at(repo_str, env("wrong")).await.unwrap_err().to_string();
        assert!(wrong.contains("wrong password"), "{wrong}");
        assert!(!wrong.contains("already exists"), "{wrong}");

        // Damaged: the config is gone, the keys are not. Left alone.
        std::fs::remove_file(repo.join("config")).unwrap();
        let damaged = ensure_repo_at(repo_str, env("right")).await.unwrap_err().to_string();
        assert!(damaged.contains("unable to open config file"), "{damaged}");
        assert!(!repo.join("config").exists());
    }
}
