//! Importing a server snapshot on a freshly installed panel: this server
//! takes over everything the old one hosted.
//!
//! The files go back where they were (site configs, app units, certificates
//! and app env files hold no IP address, so the same paths work here), the
//! databases are loaded, the DNS zones move to this server's addresses, and
//! then nginx, the apps, mail and the rest are started. Each step is written
//! to a journal, so a failed import is resumed where it stopped, or
//! discarded. A snapshot is trusted like a backup: importing one gives it
//! the server (it brings root's SSH keys, units and cron jobs), so only
//! import snapshots you made.

use super::archive;
use super::format::{self, Manifest, OpenError};
use super::merge;
use crate::error::{AgentError, AgentResult};
use crate::exec;
use crate::ipc::tasks::TaskHandle;
use crate::state::AppState;
use axum::extract::{Path as AxPath, State};
use axum::Json;
use mysql_async::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::io::{BufReader, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Root-only staging, outside the panel's own folder; on the same
/// filesystem as /srv and /var on a usual VPS, so files are renamed into
/// place rather than copied.
pub const IMPORT_DIR: &str = "/var/lib/vps-panel-import";

#[derive(Deserialize)]
pub struct PreflightReq {
    pub name: String,
    pub passphrase: String,
}

#[derive(Deserialize)]
pub struct ImportReq {
    pub name: String,
    pub passphrase: String,
    /// This server replaces the old one for good: it takes the old server's
    /// My Networks identity and networks too.
    #[serde(default)]
    pub replaces_old: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct Check {
    pub id: &'static str,
    /// "ok", "warn" or "block".
    pub level: &'static str,
    pub message: String,
}

#[derive(Serialize)]
pub struct Preflight {
    pub manifest: Manifest,
    pub checks: Vec<Check>,
    pub can_import: bool,
    /// An earlier import of this snapshot stopped part way: importing again
    /// continues it.
    pub resumable: bool,
    pub done_steps: Vec<String>,
}

#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)]
struct Journal {
    snapshot: String,
    started_at: String,
    done: Vec<String>,
    created_dbs: Vec<String>,
    created_pg_dbs: Vec<String>,
    created_users: Vec<String>,
    /// Paths this import put in place (for discard).
    placed: Vec<String>,
    /// uid / gid -> name on the old server (from the archive's headers).
    uid_names: BTreeMap<u32, String>,
    gid_names: BTreeMap<u32, String>,
    /// Zone ids (pdns) taken over, for the DNS moves.
    zone_ids: Vec<i64>,
    zones: Vec<String>,
    notes: Vec<String>,
    finished: bool,
}

fn staging(stem: &str) -> PathBuf {
    Path::new(IMPORT_DIR).join(stem)
}

fn journal_path(stem: &str) -> PathBuf {
    staging(stem).join("journal.json")
}

async fn load_journal(stem: &str) -> Option<Journal> {
    let bytes = tokio::fs::read(journal_path(stem)).await.ok()?;
    serde_json::from_slice(&bytes).ok()
}

async fn save_journal(j: &Journal) -> AgentResult<()> {
    crate::fs_atomic::write(&journal_path(&j.snapshot), &serde_json::to_vec_pretty(j).unwrap_or_default(), 0o600).await
}

/// The snapshot file, made here or pulled.
async fn locate(stem: &str) -> AgentResult<PathBuf> {
    if !format::is_valid_stem(stem) {
        return Err(AgentError::Invalid("not a snapshot name".into()));
    }
    for dir in [format::SNAPSHOT_DIR, format::INCOMING_DIR] {
        let p = Path::new(dir).join(format::file_name(stem));
        if tokio::fs::symlink_metadata(&p).await.is_ok_and(|m| m.is_file()) {
            return Ok(p);
        }
    }
    Err(AgentError::NotFound)
}

fn open_error(e: OpenError) -> AgentError {
    match e {
        OpenError::Io(e) => AgentError::Io(e),
        other => AgentError::Invalid(other.to_string()),
    }
}

async fn read_manifest(path: &Path, passphrase: &str) -> AgentResult<Manifest> {
    let p = path.to_path_buf();
    let pass = passphrase.to_string();
    tokio::task::spawn_blocking(move || archive::read_manifest(&p, &pass))
        .await
        .map_err(|e| AgentError::Other(anyhow::anyhow!("manifest: {e}")))?
        .map_err(open_error)
}

// --------------------------------------------------------------- preflight

pub async fn preflight(State(state): State<AppState>, Json(req): Json<PreflightReq>) -> AgentResult<Json<Preflight>> {
    let path = locate(&req.name).await?;
    let manifest = read_manifest(&path, &req.passphrase).await?;
    let journal = load_journal(&req.name).await.filter(|j| !j.finished);
    let size = tokio::fs::metadata(&path).await.map(|m| m.len()).unwrap_or(0);
    let checks = checks(&state, &manifest, journal.is_some(), size).await;
    let can_import = !checks.iter().any(|c| c.level == "block");
    Ok(Json(Preflight {
        manifest,
        can_import,
        resumable: journal.is_some(),
        done_steps: journal.map(|j| j.done).unwrap_or_default(),
        checks,
    }))
}

async fn checks(state: &AppState, m: &Manifest, resuming: bool, file_size: u64) -> Vec<Check> {
    let mut out = Vec::new();
    let mut push = |id: &'static str, level: &'static str, message: String| out.push(Check { id, level, message });

    if m.format > format::FORMAT {
        push("format", "block", format!("The snapshot is in format {}, newer than this panel reads; update this panel first.", m.format));
    } else {
        push("format", "ok", format!("Format {} (this panel reads up to {}).", m.format, format::FORMAT));
    }
    if m.panel.commit.is_empty() {
        push("version", "warn", "The snapshot does not say which panel version made it.".into());
    } else if super::pull::knows_commit(&m.panel.commit).await {
        push("version", "ok", "This panel is the same version as the one that made the snapshot, or newer.".into());
    } else {
        push(
            "version",
            "block",
            format!(
                "This panel is older than the one that made the snapshot (it does not have commit {}). Update this panel first (the Update button), then import.",
                &m.panel.commit[..m.panel.commit.len().min(12)]
            ),
        );
    }
    if m.panel.dirty {
        push("version-local", "warn", "The old panel had local changes to its code; the import may not know all of them.".into());
    }
    let here = super::inventory::gather_platform().await;
    if !m.platform.arch.is_empty() && m.platform.arch != here.arch {
        push(
            "arch",
            "warn",
            format!("The old server is {} and this one {}: apps with compiled parts need a redeploy after the import.", m.platform.arch, here.arch),
        );
    }
    if m.platform.os_version != here.os_version && !m.platform.os_version.is_empty() {
        push("os", "warn", format!("The old server ran {} {}, this one {} {}.", m.platform.os, m.platform.os_version, here.os, here.os_version));
    }

    if resuming {
        push("empty", "ok", "An earlier import of this snapshot stopped part way; importing continues it.".into());
    } else {
        let busy = occupied(state).await;
        if busy.is_empty() {
            push("empty", "ok", "This panel hosts nothing yet.".into());
        } else {
            push(
                "empty",
                "block",
                format!(
                    "The import replaces what a server hosts, so it only runs on a freshly installed panel. This one already has: {}.",
                    busy.join(", ")
                ),
            );
        }
    }

    let files: u64 = m.sections.iter().map(|s| s.est_bytes).sum();
    let need = files + 2 * file_size + 2 * 1024 * 1024 * 1024;
    let free = crate::handlers::files::available_bytes(Path::new("/var/lib"));
    match free {
        Some(f) if f < need => push(
            "disk",
            "block",
            format!(
                "Not enough free disk space: about {} is needed and {} is free.",
                super::export::human(need),
                super::export::human(f)
            ),
        ),
        Some(f) => push("disk", "ok", format!("{} free, about {} needed.", super::export::human(f), super::export::human(need))),
        None => push("disk", "warn", "Free disk space could not be read.".into()),
    }
    if !m.inventory.containers.is_empty() || !m.inventory.docker_volumes.is_empty() {
        push("docker", "ok", "Docker is installed if it is not yet, and its containers and volumes are brought back.".into());
    }
    if m.inventory.tailscale {
        push("tailscale", "warn", "Tailscale is installed, but this server has to log in to it once (Apps > Tailscale).".into());
    }
    out
}

/// What makes this panel not fresh: anything an import would collide with.
async fn occupied(state: &AppState) -> Vec<String> {
    let mut busy = Vec::new();
    let counts = state.panel_db.with(|c| {
        let n = |t: &str| c.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get::<_, i64>(0)).unwrap_or(0);
        Ok((n("apps"), n("sites"), n("redirects")))
    });
    if let Ok((apps, sites, redirects)) = counts {
        if apps > 0 {
            busy.push(format!("{apps} app(s)"));
        }
        if sites > 0 {
            busy.push(format!("{sites} site(s)"));
        }
        if redirects > 0 {
            busy.push(format!("{redirects} redirect(s)"));
        }
    }
    if let Ok(mut rd) = tokio::fs::read_dir("/srv/apps").await {
        while let Ok(Some(e)) = rd.next_entry().await {
            if !e.file_name().to_string_lossy().starts_with('.') {
                busy.push("files in /srv/apps".into());
                break;
            }
        }
    }
    if let Ok(mut conn) = state.mariadb.pool().get_conn().await {
        let mail: u64 = conn.query_first("SELECT COUNT(*) FROM mail.virtual_domains").await.ok().flatten().unwrap_or(0);
        if mail > 0 {
            busy.push(format!("{mail} mail domain(s)"));
        }
        let dbs: Vec<String> = conn.query("SHOW DATABASES").await.unwrap_or_default();
        let own: Vec<String> = dbs
            .into_iter()
            .filter(|d| !crate::handlers::databases::is_reserved_db(d) && !d.starts_with("vpsnap_"))
            .collect();
        if !own.is_empty() {
            busy.push(format!("database(s) {}", own.join(", ")));
        }
        let panel_host = state.config.panel_host.to_lowercase();
        let zones: Vec<String> = conn.query("SELECT name FROM pdns.domains").await.unwrap_or_default();
        // The zone of this panel's own name (the setup wizard can make it).
        let foreign: Vec<String> = zones
            .into_iter()
            .filter(|z| {
                let z = z.to_lowercase();
                !(panel_host == z || panel_host.ends_with(&format!(".{z}")))
            })
            .collect();
        if !foreign.is_empty() {
            busy.push(format!("DNS zone(s) {}", foreign.join(", ")));
        }
    }
    busy
}

// ----------------------------------------------------------------- import

pub async fn start(State(state): State<AppState>, Json(req): Json<ImportReq>) -> AgentResult<Json<crate::ipc::TaskAccepted>> {
    super::export::refuse_overlap(&state)?;
    let path = locate(&req.name).await?;
    let manifest = read_manifest(&path, &req.passphrase).await?;
    let journal = load_journal(&req.name).await.filter(|j| !j.finished);
    let size = tokio::fs::metadata(&path).await.map(|m| m.len()).unwrap_or(0);
    let blocks: Vec<String> = checks(&state, &manifest, journal.is_some(), size)
        .await
        .into_iter()
        .filter(|c| c.level == "block")
        .map(|c| c.message)
        .collect();
    if !blocks.is_empty() {
        return Err(AgentError::Conflict(blocks.join(" ")));
    }
    crate::fs_atomic::ensure_dir(Path::new(IMPORT_DIR), 0o700).await?;
    crate::fs_atomic::chmod(Path::new(IMPORT_DIR), 0o700).await?;
    crate::fs_atomic::chown_blocking(Path::new(IMPORT_DIR), 0, 0)?;
    crate::fs_atomic::ensure_dir(&staging(&req.name), 0o700).await?;
    let task = state.tasks.create("snapshots.import");
    let task_id = task.id.clone();
    let mut journal = journal.unwrap_or_else(|| Journal {
        snapshot: req.name.clone(),
        started_at: chrono::Utc::now().to_rfc3339(),
        ..Default::default()
    });
    tokio::spawn(async move {
        task.start();
        let outcome = run(&state, &req, &manifest, &path, &mut journal, &task).await;
        match outcome {
            Ok(()) => {
                task.log("the import is done; the panel restarts in a few seconds and asks you to sign in with the old server's login");
                task.finish(true);
                // Every startup check (privacy, mail routing, webmail) runs
                // against what was imported; this task has ended by then.
                let _ = exec::run(
                    "systemd-run",
                    [
                        "--on-active=5",
                        "--collect",
                        "--unit=vps-panel-after-import",
                        "systemctl",
                        "restart",
                        "vps-panel-agent",
                        "vps-panel",
                    ],
                )
                .await;
            }
            Err(e) => {
                let _ = save_journal(&journal).await;
                task.log(format!(
                    "the import stopped: {e}. Import the snapshot again to continue from here, or discard what was imported."
                ));
                task.finish(false);
            }
        }
    });
    Ok(Json(crate::ipc::TaskAccepted { task_id }))
}

const STEPS: &[&str] = &[
    "unpack", "packages", "accounts", "files", "panel-a", "mariadb", "dns-mail", "postgres", "docker", "services",
    "settings", "panel-b",
];

async fn run(
    state: &AppState,
    req: &ImportReq,
    m: &Manifest,
    file: &Path,
    j: &mut Journal,
    task: &TaskHandle,
) -> AgentResult<()> {
    let stage = staging(&req.name);
    task.log(format!(
        "importing {} made on {} ({})",
        req.name, m.source.panel_host, m.created_at
    ));
    for step in STEPS {
        if j.done.iter().any(|d| d == step) {
            task.log(format!("[{step}] done earlier"));
            continue;
        }
        task.log(format!("[{step}]"));
        match *step {
            "unpack" => step_unpack(file, &req.passphrase, &stage, j, task).await?,
            "packages" => step_packages(state, &stage, task).await?,
            "accounts" => step_accounts(&stage, j, task).await?,
            "files" => step_files(state, &stage, j, task).await?,
            "panel-a" => {
                let db = stage.join("panel/panel.db");
                let done = state.panel_db.with(|c| merge::merge_panel_part_a(c, &db))?;
                task.log(format!("panel records: {}", done.join(", ")));
            }
            "mariadb" => step_mariadb(m, &stage, j, task).await?,
            "dns-mail" => step_dns_mail(state, m, &stage, j, task).await?,
            "postgres" => step_postgres(m, &stage, j, task).await?,
            "docker" => step_docker(state, &stage, j, task).await?,
            "services" => step_services(state, j, task).await?,
            "settings" => step_settings(state, &stage, j, task).await?,
            "panel-b" => step_panel_b(state, &stage, req.replaces_old, task).await?,
            _ => {}
        }
        j.done.push(step.to_string());
        save_journal(j).await?;
    }
    j.finished = true;
    save_journal(j).await?;
    summary(m, j, task);
    // The staging held a decrypted copy of everything: it goes now. The
    // journal goes with it.
    let _ = tokio::fs::remove_dir_all(&stage).await;
    Ok(())
}

fn summary(m: &Manifest, j: &Journal, task: &TaskHandle) {
    task.log("--- still to do by hand ---");
    task.log(format!(
        "1. Point DNS at this server: at the domain registrar, the name servers' glue records ({}) and any records hosted elsewhere get this server's IP addresses.",
        if m.source.ipv4.is_some() { "they named the old server's address" } else { "check them" }
    ));
    task.log("2. Reverse DNS (PTR) for this server's IPs, at this server's provider, to the panel's host name (mail delivery needs it).");
    task.log(format!(
        "3. When DNS points here, take over the old panel host name {} under Settings > Panel hostname.",
        m.source.panel_host
    ));
    task.log("4. Mail and app data written on the old server after the snapshot was made is not here: stop the old server's apps and mail first, or make a new snapshot.");
    task.log("5. SSH host keys are this server's own: clients that knew the old server warn once (ssh-keygen -R <name>).");
    task.log("6. Delete the snapshot file on both servers when you are done: it holds every password and key.");
    for n in &j.notes {
        task.log(format!("note: {n}"));
    }
}

// ------------------------------------------------------------------ steps

async fn step_unpack(file: &Path, passphrase: &str, stage: &Path, j: &mut Journal, task: &TaskHandle) -> AgentResult<()> {
    // A half unpack from an earlier attempt starts over.
    for sub in ["fs", "db", "panel", "system"] {
        let _ = tokio::fs::remove_dir_all(stage.join(sub)).await;
    }
    let file = file.to_path_buf();
    let stage2 = stage.to_path_buf();
    let pass = passphrase.to_string();
    let (users, groups, warnings) = tokio::task::spawn_blocking(move || unpack(&file, &pass, &stage2))
        .await
        .map_err(|e| AgentError::Other(anyhow::anyhow!("unpack: {e}")))??;
    j.uid_names = users;
    j.gid_names = groups;
    for w in warnings.iter().take(20) {
        task.log(format!("  {w}"));
    }
    task.log("unpacked and checked");
    Ok(())
}

type Unpacked = (BTreeMap<u32, String>, BTreeMap<u32, String>, Vec<String>);

fn unpack(file: &Path, passphrase: &str, stage: &Path) -> AgentResult<Unpacked> {
    let f = std::fs::File::open(file)?;
    let mut ar = archive::open(BufReader::with_capacity(256 * 1024, f), passphrase).map_err(open_error)?;
    ar.set_preserve_permissions(true);
    ar.set_preserve_ownerships(true);
    ar.set_unpack_xattrs(false);
    ar.set_overwrite(true);
    let mut users = BTreeMap::new();
    let mut groups = BTreeMap::new();
    let mut index: Option<format::Index> = None;
    let mut first = true;
    for entry in ar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let name = path.to_string_lossy().into_owned();
        if first {
            first = false;
            if name != format::MANIFEST {
                return Err(AgentError::Invalid("not a server snapshot (no manifest first)".into()));
            }
        }
        if name == format::INDEX {
            let mut text = String::new();
            entry.read_to_string(&mut text)?;
            index = serde_json::from_str(&text).ok();
            continue;
        }
        if name.starts_with("fs/") {
            let h = entry.header();
            if let (Ok(uid), Ok(Some(u))) = (h.uid(), h.username()) {
                users.entry(uid as u32).or_insert_with(|| u.to_string());
            }
            if let (Ok(gid), Ok(Some(g))) = (h.gid(), h.groupname()) {
                groups.entry(gid as u32).or_insert_with(|| g.to_string());
            }
        }
        // unpack_in refuses `..` and absolute paths: nothing lands outside
        // the staging folder.
        if !entry.unpack_in(stage)? {
            return Err(AgentError::Invalid(format!("the snapshot holds a path outside its folders: {name}")));
        }
    }
    let index = index.filter(|i| i.completed).ok_or_else(|| {
        AgentError::Invalid("the snapshot is incomplete (no index at its end): the export did not finish".into())
    })?;
    for (entry, want) in &index.entries {
        let got = format::hash_file(&stage.join(entry))?;
        if got.sha256 != want.sha256 {
            return Err(AgentError::Invalid(format!("{entry} does not match its checksum: the snapshot is damaged")));
        }
    }
    Ok((users, groups, index.warnings))
}

fn read_json(path: &Path) -> Value {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null)
}

async fn has_cmd(cmd: &str) -> bool {
    exec::run("sh", ["-c", &format!("command -v {cmd}")]).await.is_ok()
}

async fn container_exists(name: &str) -> bool {
    exec::run("docker", ["inspect", "--type=container", name]).await.is_ok()
}

/// A's quick-install containers (named like their recipe).
const CONTAINER_RECIPES: &[&str] = &["qdrant", "pgadmin", "phpmyadmin", "roundcube", "n8n"];

fn old_containers(stage: &Path) -> Vec<String> {
    read_json(&stage.join("system/docker.json"))
        .get("containers")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|c| c.get("Name").and_then(Value::as_str).map(|n| n.trim_start_matches('/').to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// Runs a quick-install recipe and waits for its task.
async fn install(state: &AppState, slug: &str, task: &TaskHandle) -> AgentResult<()> {
    task.log(format!("installing {slug}"));
    let accepted = crate::handlers::quick_install::run(State(state.clone()), AxPath(slug.to_string())).await?;
    let id = accepted.0.task_id;
    loop {
        tokio::time::sleep(Duration::from_secs(3)).await;
        match state.tasks.get(&id) {
            Some(t) if t.status.as_str() == "succeeded" => return Ok(()),
            Some(t) if t.status.as_str() == "failed" => {
                return Err(AgentError::Conflict(format!(
                    "installing {slug} failed (see its task on Tasks); fix it and import again to continue"
                )))
            }
            Some(_) => {}
            None => return Err(AgentError::Conflict(format!("the {slug} install task went missing"))),
        }
    }
}

async fn step_packages(state: &AppState, stage: &Path, task: &TaskHandle) -> AgentResult<()> {
    let docker = read_json(&stage.join("system/docker.json"));
    let containers = old_containers(stage);
    let has_volumes = stage.join("fs/var/lib/docker/volumes").exists();
    let docker_apps = std::fs::read_dir(stage.join("fs/etc/vps-panel/apps"))
        .map(|rd| rd.filter_map(|e| e.ok()).any(|e| e.file_name().to_string_lossy().ends_with(".docker.json")))
        .unwrap_or(false);
    let want_docker = docker.get("installed").and_then(Value::as_bool).unwrap_or(false)
        && (!containers.is_empty() || has_volumes || docker_apps);
    if want_docker && !has_cmd("docker").await {
        install(state, "docker", task).await?;
    }
    if stage.join("db/postgres").exists() && !has_cmd("psql").await {
        install(state, "postgresql", task).await?;
    }
    if stage.join("fs/var/lib/redis").exists() && !has_cmd("redis-server").await {
        install(state, "redis", task).await?;
    }
    let ts = read_json(&stage.join("system/tailscale.json"));
    if ts.get("installed").and_then(Value::as_bool).unwrap_or(false) && !has_cmd("tailscale").await {
        install(state, "tailscale", task).await?;
    }
    for slug in CONTAINER_RECIPES {
        if containers.iter().any(|c| c == slug) && !container_exists(slug).await {
            install(state, slug, task).await?;
        }
    }
    Ok(())
}

async fn user_exists(name: &str) -> bool {
    exec::run("getent", ["passwd", name]).await.is_ok()
}

async fn group_exists(name: &str) -> bool {
    exec::run("getent", ["group", name]).await.is_ok()
}

/// Accounts made on the old server's /users page, with their passwords.
async fn step_accounts(stage: &Path, j: &mut Journal, task: &TaskHandle) -> AgentResult<()> {
    let users = read_json(&stage.join("system/users.json"));
    for u in users.as_array().cloned().unwrap_or_default() {
        let name = u.get("name").and_then(Value::as_str).unwrap_or("");
        if !crate::handlers::system::is_safe_unix_name(name)
            || crate::handlers::system::RESERVED_SYSTEM_USERS.contains(&name)
        {
            j.notes.push(format!("account {name:?} was not made again (not an allowed name)"));
            continue;
        }
        if user_exists(name).await {
            task.log(format!("account {name} is here already"));
            continue;
        }
        let home = u.get("home").and_then(Value::as_str).filter(|h| h.starts_with("/home/")).unwrap_or("");
        let home = if home.is_empty() { format!("/home/{name}") } else { home.to_string() };
        let shell = u.get("shell").and_then(Value::as_str).unwrap_or("/bin/bash");
        let shell = if crate::handlers::system::ALLOWED_SHELLS.contains(&shell) { shell } else { "/bin/bash" };
        exec::run("useradd", ["-M", "-d", home.as_str(), "-s", shell, name]).await?;
        j.created_users.push(name.to_string());
        if let Some(hash) = u.get("password_hash").and_then(Value::as_str) {
            if !hash.contains(':') && !hash.contains('\n') && !hash.is_empty() {
                exec::run("usermod", ["-p", hash, name]).await?;
            }
        }
        for g in u.get("groups").and_then(Value::as_array).cloned().unwrap_or_default() {
            let Some(g) = g.as_str() else { continue };
            if g != name && group_exists(g).await {
                let _ = exec::run("usermod", ["-aG", g, name]).await;
            }
        }
        task.log(format!("account {name} made"));
    }
    Ok(())
}

/// Where each part of fs/ goes, and how.
enum Place {
    /// Into place; the snapshot's copy wins over one already here.
    Put,
    /// Kept only for reference (this server's own configuration stays).
    Skip,
}

/// What the import does with `fs/<path>`; checked from the most specific
/// rule down.
fn placement(path: &str, own_panel_host: &str) -> Place {
    const SKIP: &[&str] = &[
        "/etc/postfix",
        "/etc/dovecot",
        "/etc/rspamd",
        "/etc/powerdns",
        "/etc/ufw",
        "/etc/ssh",
        "/etc/sysctl.d",
        "/root/.ssh",
        "/var/lib/docker",
        "/var/lib/redis",
        "/etc/nginx/sites-available/vps-panel.conf",
        "/etc/nginx/sites-enabled/vps-panel.conf",
        "/etc/nginx/vps-panel-access.conf",
    ];
    if SKIP.iter().any(|s| path == *s || path.starts_with(&format!("{s}/"))) {
        return Place::Skip;
    }
    if path.starts_with("/etc/fail2ban/jail.d/") {
        // The operator's ban policy; the rest is this install's own.
        return if path.ends_with("zzz-vps-panel-policy.conf") { Place::Put } else { Place::Skip };
    }
    if !own_panel_host.is_empty() && path == format!("/etc/vps-panel/certs/{own_panel_host}") {
        return Place::Skip;
    }
    Place::Put
}

/// Moves `from` (a staged tree) onto `to`, merging folders.
fn place_tree(from: &Path, to: &Path, placed: &mut Vec<String>, own_panel_host: &str) -> std::io::Result<()> {
    let rel = to.to_string_lossy().into_owned();
    if let Place::Skip = placement(&rel, own_panel_host) {
        return Ok(());
    }
    let from_meta = std::fs::symlink_metadata(from)?;
    let to_meta = std::fs::symlink_metadata(to).ok();
    match to_meta {
        None => {
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            move_path(from, to)?;
            placed.push(rel);
        }
        Some(t) if from_meta.is_dir() && t.is_dir() && !t.file_type().is_symlink() => {
            for e in std::fs::read_dir(from)? {
                let e = e?;
                place_tree(&e.path(), &to.join(e.file_name()), placed, own_panel_host)?;
            }
            // The folder's own mode and owner from the snapshot.
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(to, std::fs::Permissions::from_mode(from_meta.mode() & 0o7777));
            }
            let _ = std::os::unix::fs::chown(to, Some(from_meta.uid()), Some(from_meta.gid()));
        }
        Some(t) => {
            // A file (or a folder of another kind) here already: the
            // snapshot's copy replaces it.
            if t.is_dir() && !t.file_type().is_symlink() {
                std::fs::remove_dir_all(to)?;
            } else {
                std::fs::remove_file(to)?;
            }
            move_path(from, to)?;
            placed.push(rel);
        }
    }
    Ok(())
}

/// rename(2), or a copy when the staging is on another filesystem.
fn move_path(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(nix::libc::EXDEV) => {
            let status = std::process::Command::new("cp").arg("-a").arg(from).arg(to).status()?;
            if !status.success() {
                return Err(std::io::Error::other(format!("cp -a {} failed", from.display())));
            }
            let _ = if from.is_dir() { std::fs::remove_dir_all(from) } else { std::fs::remove_file(from) };
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// The old server's uid / gid -> this server's, by name. Numbers that mean
/// the same here are left out.
fn owner_maps(j: &Journal) -> (HashMap<u32, u32>, HashMap<u32, u32>, Vec<String>) {
    let mut uids = HashMap::new();
    let mut gids = HashMap::new();
    let mut unknown = Vec::new();
    for (uid, name) in &j.uid_names {
        match nix::unistd::User::from_name(name).ok().flatten() {
            Some(u) if u.uid.as_raw() != *uid => {
                uids.insert(*uid, u.uid.as_raw());
            }
            Some(_) => {}
            None => {
                unknown.push(format!("user {name}"));
                uids.insert(*uid, 0);
            }
        }
    }
    for (gid, name) in &j.gid_names {
        match nix::unistd::Group::from_name(name).ok().flatten() {
            Some(g) if g.gid.as_raw() != *gid => {
                gids.insert(*gid, g.gid.as_raw());
            }
            Some(_) => {}
            None => {
                unknown.push(format!("group {name}"));
                gids.insert(*gid, 0);
            }
        }
    }
    (uids, gids, unknown)
}

/// Gives every file under `root` this server's owner of the same name.
fn remap_owners(root: &Path, uids: &HashMap<u32, u32>, gids: &HashMap<u32, u32>) -> u64 {
    if uids.is_empty() && gids.is_empty() {
        return 0;
    }
    let mut changed = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(p) = stack.pop() {
        let Ok(m) = std::fs::symlink_metadata(&p) else { continue };
        let nu = uids.get(&m.uid()).copied();
        let ng = gids.get(&m.gid()).copied();
        if nu.is_some() || ng.is_some() {
            let _ = std::os::unix::fs::lchown(&p, nu, ng);
            changed += 1;
        }
        if m.is_dir() && !m.file_type().is_symlink() {
            if let Ok(rd) = std::fs::read_dir(&p) {
                stack.extend(rd.filter_map(|e| e.ok()).map(|e| e.path()));
            }
        }
    }
    changed
}

async fn step_files(state: &AppState, stage: &Path, j: &mut Journal, task: &TaskHandle) -> AgentResult<()> {
    let fs_root = stage.join("fs");
    let own_host = crate::server_ip::panel_host_now(&state.config.panel_host).await.to_lowercase();
    let mut placed: Vec<String> = Vec::new();
    let fs2 = fs_root.clone();
    let host2 = own_host.clone();
    let placed_result = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<String>> {
        let mut placed = Vec::new();
        if fs2.exists() {
            for e in std::fs::read_dir(&fs2)? {
                let e = e?;
                place_tree(&e.path(), &Path::new("/").join(e.file_name()), &mut placed, &host2)?;
            }
        }
        Ok(placed)
    })
    .await
    .map_err(|e| AgentError::Other(anyhow::anyhow!("place files: {e}")))??;
    placed.extend(placed_result);
    task.log(format!("{} paths put in place", placed.len()));

    let (uids, gids, unknown) = owner_maps(j);
    if !unknown.is_empty() {
        j.notes.push(format!(
            "owners unknown on this server, given to root: {}",
            unknown.join(", ")
        ));
    }
    let roots: Vec<PathBuf> = placed.iter().map(PathBuf::from).collect();
    let n = tokio::task::spawn_blocking(move || roots.iter().map(|r| remap_owners(r, &uids, &gids)).sum::<u64>())
        .await
        .unwrap_or(0);
    if n > 0 {
        task.log(format!("{n} files given this server's owner of the same name"));
    }
    merge_root_ssh(stage, task).await;
    j.placed.extend(placed);
    Ok(())
}

/// root's SSH: the old server's keys are ADDED (never replacing the key
/// that reaches this server), its deploy keys and Host aliases come over.
async fn merge_root_ssh(stage: &Path, task: &TaskHandle) {
    let from = stage.join("fs/root/.ssh");
    let Ok(mut rd) = tokio::fs::read_dir(&from).await else { return };
    let to = Path::new("/root/.ssh");
    let _ = crate::fs_atomic::ensure_dir(to, 0o700).await;
    while let Ok(Some(e)) = rd.next_entry().await {
        let name = e.file_name().to_string_lossy().into_owned();
        let target = to.join(&name);
        match name.as_str() {
            "authorized_keys" | "known_hosts" | "config" => {
                let theirs = tokio::fs::read_to_string(e.path()).await.unwrap_or_default();
                let mut ours = tokio::fs::read_to_string(&target).await.unwrap_or_default();
                let mut added = 0;
                if name == "config" {
                    // Whole Host blocks the config does not have yet.
                    for block in host_blocks(&theirs) {
                        if !ours.contains(block.lines().next().unwrap_or("")) {
                            if !ours.is_empty() && !ours.ends_with('\n') {
                                ours.push('\n');
                            }
                            ours.push_str(&block);
                            added += 1;
                        }
                    }
                } else {
                    for line in theirs.lines().filter(|l| !l.trim().is_empty()) {
                        if !ours.lines().any(|o| o == line) {
                            if !ours.is_empty() && !ours.ends_with('\n') {
                                ours.push('\n');
                            }
                            ours.push_str(line);
                            ours.push('\n');
                            added += 1;
                        }
                    }
                }
                if added > 0 {
                    let _ = crate::fs_atomic::write(&target, ours.as_bytes(), 0o600).await;
                    task.log(format!("/root/.ssh/{name}: {added} added"));
                }
            }
            _ => {
                let dest = if tokio::fs::metadata(&target).await.is_ok() {
                    to.join(format!("{name}.from-snapshot"))
                } else {
                    target
                };
                if tokio::fs::copy(e.path(), &dest).await.is_ok() {
                    let _ = crate::fs_atomic::chmod(&dest, 0o600).await;
                    let _ = crate::fs_atomic::chown_blocking(&dest, 0, 0);
                }
            }
        }
    }
}

/// `Host ...` blocks of an ssh config, each with its lines.
fn host_blocks(config: &str) -> Vec<String> {
    let mut blocks: Vec<String> = Vec::new();
    for line in config.lines() {
        if line.trim_start().to_ascii_lowercase().starts_with("host ") {
            blocks.push(String::new());
        }
        if let Some(b) = blocks.last_mut() {
            b.push_str(line);
            b.push('\n');
        }
    }
    blocks
}

async fn mariadb_file(path: &Path, db: Option<&str>) -> AgentResult<()> {
    let p = path.to_string_lossy().into_owned();
    let script = if p.ends_with(".zst") {
        r#"set -o pipefail; zstd -dc "$1" | mariadb --local-infile=0 ${2:+"$2"}"#
    } else {
        r#"mariadb --local-infile=0 ${2:+"$2"} < "$1""#
    };
    exec::run("bash", ["-c", script, "import", p.as_str(), db.unwrap_or("")]).await.map(|_| ())
}

async fn step_mariadb(m: &Manifest, stage: &Path, j: &mut Journal, task: &TaskHandle) -> AgentResult<()> {
    let dir = stage.join("db/mariadb");
    if !dir.exists() {
        return Ok(());
    }
    // Databases first (the grants name them), accounts after.
    j.created_dbs = m.inventory.mariadb_databases.clone();
    save_journal(j).await?;
    let mut rd = tokio::fs::read_dir(&dir).await?;
    let mut dumps = Vec::new();
    while let Ok(Some(e)) = rd.next_entry().await {
        let n = e.file_name().to_string_lossy().into_owned();
        if n.ends_with(".sql.zst") && !n.ends_with(".data.sql.zst") {
            dumps.push(e.path());
        }
    }
    dumps.sort();
    for d in dumps {
        task.log(format!("loading {}", d.file_name().unwrap_or_default().to_string_lossy()));
        mariadb_file(&d, None).await?;
    }
    let users = dir.join("users.sql");
    if users.exists() {
        mariadb_file(&users, None).await?;
        task.log(format!("{} MariaDB account(s) with their grants", m.inventory.mariadb_users));
    }
    Ok(())
}

async fn step_dns_mail(state: &AppState, m: &Manifest, stage: &Path, j: &mut Journal, task: &TaskHandle) -> AgentResult<()> {
    let pool = state.mariadb.pool();
    for (schema, live, scratch, sql, parent, children) in [
        (
            "pdns",
            "pdns",
            "vpsnap_pdns",
            crate::handlers::dns::PDNS_SCHEMA,
            "domains",
            &["records", "domainmetadata", "cryptokeys", "comments"][..],
        ),
        ("mail", "mail", "vpsnap_mail", crate::handlers::mail::MAIL_SCHEMA, "virtual_domains", &["virtual_users", "virtual_aliases"][..]),
    ] {
        let data = stage.join(format!("db/mariadb/{schema}.data.sql.zst"));
        if !data.exists() {
            continue;
        }
        let mut conn = pool.get_conn().await?;
        conn.query_drop(format!("DROP DATABASE IF EXISTS `{scratch}`")).await?;
        conn.query_drop(format!("CREATE DATABASE `{scratch}`")).await?;
        drop(conn);
        exec::run_with_input("mariadb", [scratch], sql).await?;
        mariadb_file(&data, Some(scratch)).await?;
        let merged = merge::merge_schema(pool, scratch, live, parent, "name", children).await?;
        let mut conn = pool.get_conn().await?;
        conn.query_drop(format!("DROP DATABASE IF EXISTS `{scratch}`")).await?;
        task.log(format!(
            "{schema}: {} taken{}",
            merged.taken.len(),
            if merged.skipped.is_empty() { String::new() } else { format!(", {} here already: {}", merged.skipped.len(), merged.skipped.join(", ")) }
        ));
        if !merged.skipped.is_empty() {
            j.notes.push(format!("{schema}: kept this server's own {}", merged.skipped.join(", ")));
        }
        if schema == "pdns" {
            j.zone_ids = merged.ids.values().copied().collect();
            j.zones = merged.taken.clone();
        }
    }
    save_journal(j).await?;

    // The zones move to this server's addresses.
    let own = crate::handlers::dns::own_addrs(state).await;
    let old_v4: Vec<String> = m.source.ipv4.iter().cloned().collect();
    match &own.v4 {
        Some(v4) if !j.zone_ids.is_empty() => {
            let n = merge::move_zones_ipv4(pool, &j.zone_ids, &old_v4, v4, chrono::Utc::now()).await?;
            task.log(format!("DNS: {n} record(s) moved to {v4}"));
        }
        Some(_) => {}
        None => j.notes.push("this server's IPv4 address is unknown: the DNS records still name the old server's".into()),
    }
    if let Some(old6) = &m.source.ipv6 {
        let mut own2 = own.clone();
        if !own2.own_v6.contains(old6) {
            own2.own_v6.push(old6.clone());
        }
        own2.own_v4.extend(old_v4.iter().cloned());
        let mut conn = crate::handlers::dns::pdns_conn(state).await?;
        let mut on: Vec<String> = Vec::new();
        for (id, zone) in j.zone_ids.iter().zip(&j.zones) {
            if let Ok((true, _)) = crate::handlers::dns::zone_ipv6_on(&mut conn, *id as u32, zone, &own2.own_v6).await {
                on.push(zone.clone());
            }
        }
        drop(conn);
        if own.v6.is_some() {
            let n = crate::handlers::dns::move_zones_ipv6(state, &on, &own2, || Ok(()), || {}).await?;
            task.log(format!("DNS: IPv6 moved in {n} zone(s)"));
        } else if !on.is_empty() {
            for zone in &on {
                crate::handlers::dns::set_zone_ipv6(state, zone, false, &own2).await?;
            }
            let unpinned = merge::unpin_ipv6_mail(pool).await.unwrap_or(0);
            j.notes.push(format!(
                "this server has no IPv6 address: IPv6 was switched off in {} (Domains > a domain > IPv6 switches it back on once it has one){}",
                on.join(", "),
                if unpinned > 0 { format!("; {unpinned} mail domain(s) pinned to IPv6 send over IPv4 now") } else { String::new() }
            ));
        }
    }
    // DNSSEC zones are signed again for their moved records.
    for zone in &j.zones {
        let _ = exec::run("pdnsutil", ["rectify-zone", zone.as_str()]).await;
    }
    let _ = exec::run("pdns_control", ["purge"]).await;
    Ok(())
}

async fn step_postgres(m: &Manifest, stage: &Path, j: &mut Journal, task: &TaskHandle) -> AgentResult<()> {
    let dir = stage.join("db/postgres");
    if !dir.exists() {
        return Ok(());
    }
    let _ = exec::run("systemctl", ["start", "postgresql"]).await;
    let roles = dir.join("roles.sql");
    if roles.exists() {
        // Errors such as an existing role are expected; psql goes on.
        let p = roles.to_string_lossy().into_owned();
        exec::run("bash", ["-c", r#"runuser -u postgres -- psql -X -q < "$1""#, "roles", p.as_str()]).await?;
    }
    j.created_pg_dbs = m.inventory.postgres_databases.clone();
    save_journal(j).await?;
    let mut rd = tokio::fs::read_dir(&dir).await?;
    while let Ok(Some(e)) = rd.next_entry().await {
        let n = e.file_name().to_string_lossy().into_owned();
        if !n.ends_with(".dump") {
            continue;
        }
        let p = e.path().to_string_lossy().into_owned();
        task.log(format!("restoring PostgreSQL {n}"));
        // From stdin: the staging is root's, the restore runs as postgres.
        if let Err(e) = exec::run(
            "bash",
            ["-c", r#"runuser -u postgres -- pg_restore --create --exit-on-error -d postgres < "$1""#, "restore", p.as_str()],
        )
        .await
        {
            j.notes.push(format!("PostgreSQL {n}: {e}"));
        }
    }
    Ok(())
}

async fn step_docker(state: &AppState, stage: &Path, j: &mut Journal, task: &TaskHandle) -> AgentResult<()> {
    let vol_root = stage.join("fs/var/lib/docker/volumes");
    let docker = read_json(&stage.join("system/docker.json"));
    if has_cmd("docker").await && vol_root.exists() {
        let containers = old_containers(stage);
        let ours: Vec<&str> = CONTAINER_RECIPES.iter().copied().filter(|c| containers.iter().any(|x| x == c)).collect();
        for c in &ours {
            let _ = exec::run("docker", ["stop", c]).await;
        }
        let labels: HashMap<String, Vec<String>> = docker
            .get("volumes")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| {
                        let name = v.get("Name")?.as_str()?.to_string();
                        let labels = v
                            .get("Labels")
                            .and_then(Value::as_object)
                            .map(|o| o.iter().map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or(""))).collect())
                            .unwrap_or_default();
                        Some((name, labels))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut rd = tokio::fs::read_dir(&vol_root).await?;
        while let Ok(Some(e)) = rd.next_entry().await {
            let name = e.file_name().to_string_lossy().into_owned();
            let data = e.path().join("_data");
            if !data.exists() {
                continue;
            }
            let mut args: Vec<String> = vec!["volume".into(), "create".into()];
            for l in labels.get(&name).cloned().unwrap_or_default() {
                args.push("--label".into());
                args.push(l);
            }
            args.push(name.clone());
            exec::run("docker", args.iter().map(String::as_str)).await?;
            let mount = exec::run("docker", ["volume", "inspect", "-f", "{{.Mountpoint}}", name.as_str()]).await?.stdout;
            let mount = PathBuf::from(mount.trim());
            let data2 = data.clone();
            let mount2 = mount.clone();
            tokio::task::spawn_blocking(move || -> std::io::Result<()> {
                // The volume's own content (a fresh recipe's) makes way.
                if let Ok(rd) = std::fs::read_dir(&mount2) {
                    for e in rd.flatten() {
                        let p = e.path();
                        let _ = if p.is_dir() && !p.is_symlink() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
                    }
                }
                for e in std::fs::read_dir(&data2)? {
                    let e = e?;
                    move_path(&e.path(), &mount2.join(e.file_name()))?;
                }
                // Numeric owners stay: they are the container's users.
                if let Ok(m) = std::fs::symlink_metadata(&data2) {
                    let _ = std::os::unix::fs::chown(&mount2, Some(m.uid()), Some(m.gid()));
                }
                Ok(())
            })
            .await
            .map_err(|e| AgentError::Other(anyhow::anyhow!("volume {name}: {e}")))??;
            j.placed.push(format!("docker-volume:{name}"));
            task.log(format!("volume {name} filled"));
        }
        for c in &ours {
            let _ = exec::run("docker", ["start", c]).await;
        }
        if !ours.is_empty() {
            j.notes.push(format!(
                "{} run with their data from the old server; check their settings under Apps > Quick install (host names, URLs)",
                ours.join(", ")
            ));
        }
    }
    let redis = stage.join("fs/var/lib/redis");
    if redis.exists() && has_cmd("redis-server").await {
        let _ = exec::run("systemctl", ["stop", "redis-server"]).await;
        let mut placed = Vec::new();
        let r2 = redis.clone();
        let _ = tokio::task::spawn_blocking(move || place_tree(&r2, Path::new("/var/lib/redis"), &mut placed, "")).await;
        let _ = exec::run("chown", ["-R", "redis:redis", "/var/lib/redis"]).await;
        let _ = exec::run("systemctl", ["start", "redis-server"]).await;
        task.log("Redis data put back");
    }
    // Docker apps: their image pulled or built again, from the restored
    // settings and source.
    let apps: Vec<(String, String, u16, String)> = state
        .panel_db
        .with(|c| {
            let mut s = c.prepare("SELECT name, source_dir, port, subdomain FROM apps")?;
            let rows = s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? as u16, r.get(3)?)))?;
            rows.collect()
        })
        .unwrap_or_default();
    for (name, source_dir, port, subdomain) in apps {
        if !Path::new(&format!("/etc/vps-panel/apps/{name}.docker.json")).exists() {
            continue;
        }
        task.log(format!("Docker app {name}: getting its image"));
        let req: crate::handlers::apps::CreateApp = match serde_json::from_value(serde_json::json!({
            "name": name, "source_dir": source_dir, "port": port, "subdomain": subdomain,
            "issue_ssl": false, "skip_dns_check": true, "step": "all", "runtime": "docker",
        })) {
            Ok(r) => r,
            Err(e) => {
                j.notes.push(format!("Docker app {name}: {e}"));
                continue;
            }
        };
        let acme = state.config.acme_email.clone();
        if let Err(e) = Box::pin(crate::handlers::docker_apps::run_deploy(&state.config, &acme, &req, task)).await {
            j.notes.push(format!("Docker app {name} did not start ({e}); deploy it again from Apps"));
        }
    }
    Ok(())
}

async fn step_services(state: &AppState, j: &mut Journal, task: &TaskHandle) -> AgentResult<()> {
    let _ = exec::run("systemctl", ["daemon-reload"]).await;
    // A site whose config nginx refuses is set aside, not the import.
    let mut quarantined = Vec::new();
    for _ in 0..50 {
        match exec::run("nginx", ["-t"]).await {
            Ok(_) => break,
            Err(AgentError::Subprocess { stderr, .. }) => {
                let bad = stderr
                    .lines()
                    .find_map(|l| l.split(" in /etc/nginx/sites-enabled/").nth(1))
                    .and_then(|rest| rest.split(':').next())
                    .map(str::to_string);
                let Some(bad) = bad.filter(|b| b != "vps-panel.conf") else {
                    j.notes.push(format!("nginx refuses its configuration: {}", stderr.lines().last().unwrap_or("")));
                    break;
                };
                let _ = tokio::fs::remove_file(format!("/etc/nginx/sites-enabled/{bad}")).await;
                quarantined.push(bad);
            }
            Err(e) => return Err(e),
        }
    }
    if !quarantined.is_empty() {
        j.notes.push(format!(
            "nginx refused these sites, which are switched off (their files stay in sites-available): {}",
            quarantined.join(", ")
        ));
    }
    let _ = exec::run("systemctl", ["reload", "nginx"]).await;

    let apps: Vec<String> = state
        .panel_db
        .with(|c| {
            let mut s = c.prepare("SELECT name FROM apps")?;
            let rows = s.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect()
        })
        .unwrap_or_default();
    for name in apps {
        if Path::new(&format!("/etc/vps-panel/apps/{name}.docker.json")).exists() {
            continue;
        }
        let unit = format!("vps-app-{name}");
        match exec::run("systemctl", ["enable", "--now", unit.as_str()]).await {
            Ok(_) => task.log(format!("app {name} started")),
            Err(e) => j.notes.push(format!("app {name} did not start: {e}")),
        }
    }
    if let Err(e) = crate::handlers::ssl::regenerate_mail_sni(state).await {
        j.notes.push(format!("mail certificates map: {e}"));
    }
    for (verb, unit) in [("reload", "postfix"), ("reload", "dovecot"), ("restart", "rspamd"), ("reload", "fail2ban"), ("reload", "pdns")] {
        let _ = exec::run("systemctl", [verb, unit]).await;
    }
    crate::handlers::mail_ip::ensure_at_startup(state.clone()).await;
    Ok(())
}

/// A rule line from `ufw show added` (`ufw allow 8080/tcp comment 'x'`) as
/// arguments, quotes respected.
pub(crate) fn ufw_args(line: &str) -> Option<Vec<String>> {
    let rest = line.trim().strip_prefix("ufw ")?;
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in rest.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => quote = Some(c),
            (None, c) if c.is_whitespace() => {
                if !cur.is_empty() {
                    args.push(std::mem::take(&mut cur));
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if !cur.is_empty() {
        args.push(cur);
    }
    (!args.is_empty()).then_some(args)
}

async fn step_settings(state: &AppState, stage: &Path, j: &mut Journal, task: &TaskHandle) -> AgentResult<()> {
    // Firewall: the old server's extra allow rules.
    let fw = read_json(&stage.join("system/firewall.json"));
    let ours = exec::run("ufw", ["show", "added"]).await.map(|o| o.stdout).unwrap_or_default();
    let mut added = 0;
    for line in fw.get("ufw_added").and_then(Value::as_str).unwrap_or("").lines() {
        let line = line.trim();
        if !line.starts_with("ufw allow") || ours.lines().any(|l| l.trim() == line) {
            continue;
        }
        if let Some(args) = ufw_args(line) {
            if exec::run("ufw", args.iter().map(String::as_str)).await.is_ok() {
                added += 1;
            }
        }
    }
    if added > 0 {
        task.log(format!("firewall: {added} allow rule(s) of the old server added"));
    }

    // SSH server: port, idle timeout, password and root login, through the
    // panel's own checks (they refuse a change nobody could log in after).
    if let Ok(text) = tokio::fs::read_to_string(stage.join("fs/etc/ssh/sshd_config.d/00-vps-panel.conf")).await {
        let mut port = None;
        let mut idle = None;
        let mut password = None;
        let mut root = None;
        for line in text.lines() {
            let mut parts = line.split_whitespace();
            let (Some(k), Some(v)) = (parts.next(), parts.next()) else { continue };
            match k.to_ascii_lowercase().as_str() {
                "port" => port = v.parse::<u16>().ok(),
                "channeltimeout" => {
                    idle = v.strip_prefix("session=").and_then(|m| m.strip_suffix('m')).and_then(|m| m.parse::<u32>().ok())
                }
                "passwordauthentication" => password = Some(v == "yes"),
                "permitrootlogin" => root = Some(v.to_string()),
                _ => {}
            }
        }
        if port.is_some() || idle.is_some() || password.is_some() || root.is_some() {
            let req = crate::handlers::ssh::SshServerConfigUpdate {
                port,
                idle_timeout_minutes: idle,
                password_auth: password,
                permit_root_login: root,
                force: false,
            };
            match crate::handlers::ssh::config_set(Json(req)).await {
                Ok(_) => task.log("SSH server settings of the old server applied"),
                Err(e) => j.notes.push(format!("SSH server settings were not applied ({e}); set them under Settings > SSH server")),
            }
        }
    }

    let settings = read_json(&stage.join("system/settings.json"));
    // Swap, when this server has none.
    let swap_here = tokio::fs::read_to_string("/proc/swaps").await.unwrap_or_default().lines().count() > 1;
    let swap_kib: u64 = settings
        .get("swaps")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .and_then(|l| l.split_whitespace().nth(2))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if !swap_here && swap_kib > 0 {
        let req = crate::handlers::swap::SetSwapReq {
            size_mb: swap_kib / 1024,
            swappiness: settings.get("swappiness").and_then(Value::as_u64).unwrap_or(60) as u32,
        };
        match crate::handlers::swap::set(State(state.clone()), Json(req)).await {
            Ok(a) => wait_task(state, &a.0.task_id).await,
            Err(e) => j.notes.push(format!("swap was not made ({e})")),
        }
    }
    // The MCP token (AI agents), if the old server had one in its .env.
    if let Some(token) = settings.pointer("/env/MCP_TOKEN").and_then(Value::as_str) {
        if let Err(e) = set_env_key(state, "MCP_TOKEN", token).await {
            j.notes.push(format!("MCP_TOKEN was not set ({e})"));
        }
    }
    j.notes.push("Settings > Panel access is this server's own: enter the old server's allowed addresses again if you used them".into());
    Ok(())
}

async fn wait_task(state: &AppState, id: &str) {
    for _ in 0..200 {
        tokio::time::sleep(Duration::from_secs(2)).await;
        match state.tasks.get(id) {
            Some(t) if t.status.as_str() == "succeeded" || t.status.as_str() == "failed" => return,
            None => return,
            _ => {}
        }
    }
}

/// Sets one `KEY=value` line of /etc/vps-panel/.env, keeping the rest.
async fn set_env_key(state: &AppState, key: &str, value: &str) -> AgentResult<()> {
    if value.contains('\n') {
        return Err(AgentError::Invalid("value with a newline".into()));
    }
    let path = Path::new("/etc/vps-panel/.env");
    let text = tokio::fs::read_to_string(path).await?;
    let mut found = false;
    let mut out: Vec<String> = text
        .lines()
        .map(|l| {
            if l.split_once('=').is_some_and(|(k, _)| k.trim() == key) {
                found = true;
                format!("{key}={value}")
            } else {
                l.to_string()
            }
        })
        .collect();
    if !found {
        out.push(format!("{key}={value}"));
    }
    let mut body = out.join("\n");
    body.push('\n');
    crate::fs_atomic::write(path, body.as_bytes(), 0o640).await?;
    crate::fs_atomic::chown_blocking(path, 0, state.config.vps_panel_gid)?;
    Ok(())
}

async fn step_panel_b(state: &AppState, stage: &Path, replaces_old: bool, task: &TaskHandle) -> AgentResult<()> {
    let db = stage.join("panel/panel.db");
    state.panel_db.with(|c| merge::merge_panel_part_b(c, &db, replaces_old))?;
    if replaces_old {
        let key = stage.join("panel/node-identity.key");
        if key.exists() {
            let dest = state.config.panel_db_path.with_file_name("node-identity.key");
            tokio::fs::copy(&key, &dest).await?;
            crate::fs_atomic::chmod(&dest, 0o600).await?;
            if let Some(uid) = state.config.vps_panel_uid {
                crate::fs_atomic::chown_blocking(&dest, uid, state.config.vps_panel_gid)?;
            }
            task.log("My Networks: this server takes the old server's place (shut the old one down)");
        }
    }
    task.log("panel users, settings and history taken over; everyone signs in again");
    Ok(())
}

// ----------------------------------------------------------------- discard

#[derive(Deserialize)]
pub struct DiscardReq {
    pub name: String,
}

/// Undoes what an unfinished import did, from its journal: its databases,
/// accounts and placed files go, and its staging. Packages it installed
/// stay. Reinstalling the server is the sure way back.
pub async fn discard(State(state): State<AppState>, Json(req): Json<DiscardReq>) -> AgentResult<Json<crate::ipc::OkResponse>> {
    if state.tasks.any_live_with_prefix("snapshots.") {
        return Err(AgentError::Conflict("a snapshot task is running".into()));
    }
    if !format::is_valid_stem(&req.name) {
        return Err(AgentError::Invalid("not a snapshot name".into()));
    }
    let Some(j) = load_journal(&req.name).await else {
        let _ = tokio::fs::remove_dir_all(staging(&req.name)).await;
        return Ok(Json(crate::ipc::OkResponse::default()));
    };
    if j.finished {
        return Err(AgentError::Conflict("that import finished; there is nothing to discard".into()));
    }
    if let Ok(mut conn) = state.mariadb.pool().get_conn().await {
        for db in &j.created_dbs {
            if !crate::handlers::databases::is_reserved_db(db) {
                let _ = conn.query_drop(format!("DROP DATABASE IF EXISTS `{}`", db.replace('`', "``"))).await;
            }
        }
        for id in &j.zone_ids {
            for t in ["records", "domainmetadata", "cryptokeys", "comments"] {
                let _ = conn.exec_drop(format!("DELETE FROM pdns.{t} WHERE domain_id = ?"), (id,)).await;
            }
            let _ = conn.exec_drop("DELETE FROM pdns.domains WHERE id = ?", (id,)).await;
        }
    }
    for db in &j.created_pg_dbs {
        if !crate::handlers::postgres::is_reserved_db(db) {
            let _ = exec::run("runuser", ["-u", "postgres", "--", "dropdb", "--if-exists", db.as_str()]).await;
        }
    }
    for u in &j.created_users {
        let _ = exec::run("userdel", [u.as_str()]).await;
    }
    for p in j.placed.iter().rev() {
        if let Some(vol) = p.strip_prefix("docker-volume:") {
            let _ = exec::run("docker", ["volume", "rm", vol]).await;
            continue;
        }
        let path = Path::new(p);
        // Only what the import put there, never a top-level system folder.
        if path.components().count() < 3 {
            continue;
        }
        let _ = if path.is_dir() && !path.is_symlink() {
            tokio::fs::remove_dir_all(path).await
        } else {
            tokio::fs::remove_file(path).await
        };
    }
    let _ = tokio::fs::remove_dir_all(staging(&req.name)).await;
    Ok(Json(crate::ipc::OkResponse::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_keeps_this_servers_own_configuration() {
        for p in [
            "/etc/postfix/main.cf",
            "/etc/ssh/sshd_config.d/00-vps-panel.conf",
            "/etc/nginx/sites-enabled/vps-panel.conf",
            "/root/.ssh/authorized_keys",
            "/etc/fail2ban/jail.d/zz-vps-panel.conf",
            "/var/lib/docker/volumes/x/_data",
            "/etc/vps-panel/certs/panel.example.net",
        ] {
            assert!(matches!(placement(p, "panel.example.net"), Place::Skip), "{p}");
        }
        for p in [
            "/srv/apps/shop",
            "/etc/nginx/sites-available/site-shop.example.com",
            "/etc/systemd/system/vps-app-shop.service",
            "/etc/vps-panel/certs/shop.example.com",
            "/etc/fail2ban/jail.d/zzz-vps-panel-policy.conf",
            "/var/vmail/example.com",
            "/var/lib/rspamd/dkim/example.com.default.key",
        ] {
            assert!(matches!(placement(p, "panel.example.net"), Place::Put), "{p}");
        }
    }

    #[test]
    fn ufw_rule_lines() {
        assert_eq!(ufw_args("ufw allow 8080/tcp").unwrap(), vec!["allow", "8080/tcp"]);
        assert_eq!(
            ufw_args("ufw allow 5432/tcp comment 'Postgres for the app'").unwrap(),
            vec!["allow", "5432/tcp", "comment", "Postgres for the app"]
        );
        assert_eq!(ufw_args("something else"), None);
    }

    #[test]
    fn ssh_host_blocks() {
        let cfg = "# top\nHost github-shop\n  HostName github.com\n  IdentityFile ~/.ssh/github_shop\nHost other\n  User x\n";
        let b = host_blocks(cfg);
        assert_eq!(b.len(), 2);
        assert!(b[0].starts_with("Host github-shop\n") && b[0].contains("IdentityFile"));
    }

    #[test]
    fn files_are_placed_merged_and_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("from/srv");
        let to = tmp.path().join("to/srv");
        std::fs::create_dir_all(from.join("apps/shop")).unwrap();
        std::fs::write(from.join("apps/shop/app.js"), b"new").unwrap();
        std::fs::create_dir_all(to.join("apps/shop")).unwrap();
        std::fs::write(to.join("apps/shop/app.js"), b"old").unwrap();
        std::fs::write(to.join("apps/keep.txt"), b"here").unwrap();
        let mut placed = Vec::new();
        place_tree(&from, &to, &mut placed, "").unwrap();
        assert_eq!(std::fs::read(to.join("apps/shop/app.js")).unwrap(), b"new");
        assert_eq!(std::fs::read(to.join("apps/keep.txt")).unwrap(), b"here");
        assert_eq!(placed, vec![to.join("apps/shop/app.js").to_string_lossy().into_owned()]);
    }
}
