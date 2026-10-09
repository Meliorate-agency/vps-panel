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
        ["snapshots", "--json"],
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
            task.log(format!("init error: {e}"));
            task.finish(false);
            return;
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
        let paths: Vec<String> = req
            .paths
            .clone()
            .unwrap_or_else(|| DEFAULT_PATHS.iter().map(|s| s.to_string()).collect());
        let mut args: Vec<String> = vec!["backup".into(), "--json".into()];
        for exclude in BACKUP_EXCLUDES {
            args.push(format!("--exclude={exclude}"));
        }
        for p in &paths { args.push(p.clone()); }
        let env = repo_env_owned(&cfg);
        let env_refs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let code = exec::run_streaming_envs("restic", args.iter().map(|s| s.as_str()), env_refs, |line| {
            task.log(line.to_string());
        })
        .await
        .unwrap_or(-1);
        if code != 0 {
            task.log(format!("restic backup failed (exit {code})"));
            task.finish(false);
            return;
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

/// Snapshots kept after each backup (restic forget).
const RETENTION: &[&str] = &["--keep-daily", "7", "--keep-weekly", "4", "--keep-monthly", "6"];

/// Rebuildable or transient trees left out of every snapshot.
const BACKUP_EXCLUDES: &[&str] = &[
    "node_modules",
    "**/.next/cache",
    "/var/lib/vps-panel/build-snapshots",
];

/// Where database dumps are written before restic picks them up (inside
/// /var/lib/vps-panel, a backup root). Root-only: the parent is group
/// readable by the panel's group, and a dump holds every database.
const DUMP_ROOT: &str = "/var/lib/vps-panel/db-dumps";

async fn private_dump_dir(name: &str) -> AgentResult<std::path::PathBuf> {
    let root = std::path::Path::new(DUMP_ROOT);
    crate::fs_atomic::ensure_dir(root, 0o700).await?;
    crate::fs_atomic::chmod(root, 0o700).await?;
    let dir = root.join(name);
    crate::fs_atomic::ensure_dir(&dir, 0o700).await?;
    crate::fs_atomic::chmod(&dir, 0o700).await?;
    Ok(dir)
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
        task.finish(code == 0);
    });
    Ok(Json(TaskAccepted { task_id }))
}

async fn is_repo_initialized(state: &AppState) -> AgentResult<bool> {
    let env = repo_env(state);
    match exec::run_envs("restic", ["snapshots", "--last", "--json"], env).await {
        Ok(_) => Ok(true),
        Err(_) => Ok(false),
    }
}

async fn ensure_repo(cfg: &crate::config::Config) -> AgentResult<()> {
    let env = repo_env_from_cfg(cfg);
    match exec::run_envs("restic", ["snapshots", "--last"], env.clone()).await {
        Ok(_) => Ok(()),
        Err(_) => {
            let _ = tokio::fs::create_dir_all(&cfg.restic_repository).await;
            exec::run_envs("restic", ["init"], env).await.map(|_| ())
        }
    }
}

async fn dump_mariadb(task: &crate::ipc::tasks::TaskHandle) -> AgentResult<()> {
    let dump_dir = private_dump_dir("mariadb").await?;
    // The old location was group-readable; drop its copy.
    let _ = tokio::fs::remove_dir_all("/var/lib/vps-panel/mariadb-dump").await;
    let dump_path = dump_dir.join("all-databases.sql");
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
    use super::{validate_restore, RESTORE_ROOT};

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
}
