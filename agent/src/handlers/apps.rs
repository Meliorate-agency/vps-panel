use crate::error::{AgentError, AgentResult};
use crate::exec;
use crate::fs_atomic;
use crate::handlers::app_git;
use crate::handlers::docker_apps;
use crate::handlers::nginx as nginx_handler;
use crate::ipc::{OkResponse, TaskAccepted};
use crate::nginx_template::AppVhostTemplate;
use crate::state::AppState;
use crate::systemd_template::AppServiceTemplate;
use crate::validate::{is_safe_subdomain, validate_name};
use askama::Template;
use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::time::{sleep, Duration};

#[cfg(unix)]
fn symlink(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

#[cfg(not(unix))]
fn symlink(_src: &std::path::Path, _dst: &std::path::Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "symlink only supported on unix",
    ))
}

#[derive(Deserialize)]
pub struct CreateApp {
    pub name: String,
    pub source_dir: String,
    pub port: u16,
    pub subdomain: String,
    /// None keeps the existing unit's ExecStart on a re-deploy (and means
    /// `/usr/bin/npm start` for a new app). The panel form always sends it;
    /// an MCP re-run that omits it no longer resets a custom entrypoint.
    #[serde(default)]
    pub exec_start: Option<String>,
    /// None keeps the existing EnvironmentFile on a re-deploy (empty for a new
    /// app). Some(..) replaces the file. An MCP re-run that omitted env used
    /// to wipe every variable.
    #[serde(default)]
    pub env: Option<Vec<EnvKv>>,
    pub issue_ssl: bool,
    #[serde(default)]
    pub skip_dns_check: bool,
    /// Optional git repo to clone into `source_dir` before the deploy steps
    /// run. When present, the task pulls the code first (and `npm ci` if a
    /// lockfile is present), then continues with the normal vhost/unit/SSL
    /// flow. Lets operators deploy straight from a GitHub URL instead of
    /// rsync-ing code onto the box by hand.
    #[serde(default)]
    pub repo_url: Option<String>,
    /// Which subset of the deploy pipeline to run for this request. Values:
    ///   "pull"    → git clone / git pull only
    ///   "install" → npm install / npm ci only
    ///   "build"   → npm run build only (skipped if no build script)
    ///   "deploy"  → write unit + vhost + start + SSL only
    ///   "all" / None → backwards-compatible full pipeline
    /// Lets the panel surface 4 buttons (Pull / Install / Build / Deploy)
    /// each as a separate task so the operator can iterate per stage.
    #[serde(default)]
    pub step: Option<String>,
    /// "node" or "docker". None = what the app already is (docker when
    /// /etc/vps-panel/apps/<name>.docker.json exists), else node. An app
    /// never switches runtime on a re-deploy: delete it first.
    #[serde(default)]
    pub runtime: Option<String>,
    /// Docker apps: image, container port, kept folders, limits. None on a
    /// re-deploy keeps the saved settings (docker_apps.rs).
    #[serde(default)]
    pub docker: Option<docker_apps::DockerSpec>,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct EnvKv {
    pub key: String,
    pub value: String,
}

#[derive(Serialize)]
pub struct AppRow {
    pub name: String,
    pub subdomain: String,
    pub port: u16,
    pub source_dir: String,
    pub active: String,
    pub sub: String,
    // "panel" = managed by this panel (vps-app-*.service unit exists and
    // optionally a row in `apps`). "external" = a folder under /srv/apps that
    // the panel didn't create (a manual deploy produces these).
    pub kind: String,
    /// "missing" — source_dir doesn't exist on disk
    /// "empty"   — source_dir exists but contains no entries
    /// "populated" — source_dir exists and has at least one entry
    /// Lets /apps surface a "folder empty" warning so the operator can spot
    /// an app whose code was wiped (e.g. via the delete-with-wipe flow) but
    /// whose panel-side row is still around.
    /// "none" — a Docker app, which has no source folder.
    pub source_dir_state: String,
    /// "node" | "docker".
    pub runtime: String,
    /// Docker apps: the image reference they run, as entered.
    pub image: Option<String>,
}

const APPS_BASE_DIR: &str = "/srv/apps";

fn looks_like_app_name(s: &str) -> bool {
    if s.is_empty() || s.starts_with('.') {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[derive(Deserialize)]
pub struct PatchEnv {
    pub env: Vec<EnvKv>,
}

/// One `.env*` file discovered inside an app's source directory.
#[derive(Serialize)]
pub struct EnvFileEntry {
    /// Path relative to the source dir, forward-slashed (e.g. `.env.local`
    /// or `apps/web/.env`).
    pub path: String,
    /// Absolute on-disk path, for display.
    pub abs_path: String,
    pub size: u64,
    /// File body. Empty when `too_large` is set.
    pub content: String,
    /// Set when the file exceeds the inline cap; the UI shows it read-only.
    pub too_large: bool,
}

#[derive(Serialize)]
pub struct EnvFilesResp {
    pub source_dir: String,
    pub files: Vec<EnvFileEntry>,
    /// True when the scan hit the result cap and stopped early.
    pub truncated: bool,
}

#[derive(Deserialize)]
pub struct SaveEnvFile {
    /// Relative path under the source dir. The final segment (the file name)
    /// must start with `.env`.
    pub path: String,
    pub content: String,
    #[serde(default)]
    pub restart: bool,
}

#[derive(Serialize)]
pub struct SaveEnvFileResp {
    pub ok: bool,
    /// The systemd unit that was restarted, if any (None when `restart` was
    /// false or no unit exists for the app yet, e.g. pre-deploy).
    pub restarted: Option<String>,
}

#[derive(Deserialize)]
pub struct DeleteEnvFile {
    /// Relative `.env*` path under the source dir - validated identically to
    /// save so it can't escape the folder.
    pub path: String,
    #[serde(default)]
    pub restart: bool,
}

#[derive(Deserialize)]
pub struct SetEntrypoint {
    pub exec_start: String,
}

#[derive(Serialize)]
pub struct SetEntrypointResp {
    pub ok: bool,
    pub restarted: bool,
}

pub(crate) const VPS_APP_PREFIX: &str = "vps-app-";

// Project `.env*` scanning limits. Recurse the source dir but never descend
// into dependency/build output (which can hold thousands of fixture .env
// files), cap depth + total results, and only inline reasonably-sized files.
const ENV_SCAN_IGNORE_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    ".next",
    ".turbo",
    ".cache",
    ".svelte-kit",
    ".nuxt",
    ".vercel",
    ".output",
    "dist",
    "build",
    "out",
    "coverage",
    "vendor",
    "target",
];
const ENV_SCAN_MAX_DEPTH: usize = 4;
const ENV_SCAN_MAX_RESULTS: usize = 50;
const ENV_FILE_MAX_INLINE: u64 = 256 * 1024;
const ENV_FILE_MAX_WRITE: usize = 1024 * 1024;

pub async fn list(State(state): State<AppState>) -> AgentResult<Json<Vec<AppRow>>> {
    let out = exec::run(
        "systemctl",
        ["list-units", "--type=service", "--all", "--no-pager", "--no-legend", "--plain", "vps-app-*.service"],
    )
    .await?;
    let mut rows = Vec::new();
    let mut seen_source_dirs: std::collections::HashSet<String> = std::collections::HashSet::new();
    for line in out.stdout.lines() {
        let mut parts = line.split_whitespace();
        let unit = parts.next().unwrap_or("");
        let _load = parts.next();
        let active = parts.next().unwrap_or("").to_string();
        let sub = parts.next().unwrap_or("").to_string();
        if !unit.starts_with(VPS_APP_PREFIX) {
            continue;
        }
        let name = unit
            .strip_prefix(VPS_APP_PREFIX)
            .and_then(|s| s.strip_suffix(".service"))
            .unwrap_or("")
            .to_string();
        if name.is_empty() {
            continue;
        }
        let (subdomain, port, source_dir) = lookup_app(&state, &name).await.unwrap_or_default();
        if !source_dir.is_empty() {
            seen_source_dirs.insert(source_dir.clone());
        }
        let docker = docker_apps::read_config(&name).await;
        let source_dir_state = source_dir_state_for(&source_dir, docker.is_some()).await;
        rows.push(AppRow {
            name,
            subdomain,
            port,
            source_dir,
            active,
            sub,
            kind: "panel".into(),
            source_dir_state,
            runtime: runtime_label(docker.is_some()),
            image: docker.map(|d| d.image),
        });
    }

    // Drift detection: surface every folder under /srv/apps/ that isn't already
    // represented by a panel-managed unit above. Matches the manual deploy
    // procedure (each folder = one app) so admins can see hand-rolled apps
    // alongside panel-deployed ones.
    if let Ok(mut entries) = tokio::fs::read_dir(APPS_BASE_DIR).await {
        let panel_names: std::collections::HashSet<String> =
            rows.iter().map(|r| r.name.clone()).collect();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let Ok(ft) = entry.file_type().await else {
                continue;
            };
            if !ft.is_dir() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(|s| s.to_string()) else {
                continue;
            };
            if !looks_like_app_name(&name) {
                continue;
            }
            let source_dir = format!("{APPS_BASE_DIR}/{name}");
            if panel_names.contains(&name) || seen_source_dirs.contains(&source_dir) {
                continue;
            }
            let (active, sub) = external_service_status(&name).await;
            let source_dir_state = classify_dir(&source_dir).await;
            rows.push(AppRow {
                name,
                subdomain: String::new(),
                port: 0,
                source_dir,
                active,
                sub,
                kind: "external".into(),
                source_dir_state,
                runtime: runtime_label(false),
                image: None,
            });
        }
    }

    rows.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Json(rows))
}

fn runtime_label(docker: bool) -> String {
    if docker { "docker" } else { "node" }.to_string()
}

/// A Docker app keeps its data in named volumes and has no source folder.
async fn source_dir_state_for(source_dir: &str, docker: bool) -> String {
    if docker {
        "none".to_string()
    } else if source_dir.is_empty() {
        "missing".to_string()
    } else {
        classify_dir(source_dir).await
    }
}

// Best-effort lookup of an external app's systemd state. The manual deploy
// procedure names the unit "<app>.service" (no vps-app- prefix). If no matching unit
// exists, returns empty strings so the UI can render a muted dash.
async fn external_service_status(name: &str) -> (String, String) {
    let unit = format!("{name}.service");
    let out = match exec::run("systemctl", ["show", &unit, "--no-pager"]).await {
        Ok(o) => o,
        Err(_) => return (String::new(), String::new()),
    };
    let active = field(&out.stdout, "ActiveState=").unwrap_or_default();
    let sub = field(&out.stdout, "SubState=").unwrap_or_default();
    // `systemctl show` on a non-existent unit returns LoadState=not-found
    // with ActiveState=inactive. Treat that as "no unit" rather than "inactive".
    if field(&out.stdout, "LoadState=").as_deref() == Some("not-found") {
        return (String::new(), String::new());
    }
    (active, sub)
}

#[derive(Serialize)]
pub struct GitStatus {
    pub has_repo: bool,
    /// Number of commits the local checkout is behind its upstream tracking
    /// branch (i.e. how many `git pull` would apply). 0 = up to date.
    pub behind: u32,
    /// Number of commits ahead of upstream (local-only commits). Surfaced
    /// so the UI can warn that a plain `git pull --ff-only` won't work.
    pub ahead: u32,
    /// Short SHA of HEAD, for display.
    pub current_sha: Option<String>,
    /// Short SHA of the upstream tip, for display.
    pub upstream_sha: Option<String>,
    /// Set when the fetch / compare failed (no network, no upstream tracking
    /// branch, ssh-key not added to deploy keys, etc.). The UI shows a muted
    /// "couldn't check" instead of pretending everything's fine.
    pub error: Option<String>,
}

/// Fast `git fetch` + `HEAD..@{u}` comparison for the panel's /apps page.
/// Per-app `git fetch` is run in parallel by the panel, so this handler
/// should return quickly; we cap the fetch at 15s to avoid hanging the
/// whole page on a dead network.
pub async fn git_status(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> AgentResult<Json<GitStatus>> {
    validate_name(&name)?;
    let (_, _, source_dir) = lookup_app(&state, &name).await.unwrap_or_default();
    let git_dir = if source_dir.is_empty() {
        PathBuf::from(format!("{APPS_BASE_DIR}/{name}/.git"))
    } else {
        PathBuf::from(&source_dir).join(".git")
    };
    if !git_dir.is_dir() {
        return Ok(Json(GitStatus {
            has_repo: false,
            behind: 0,
            ahead: 0,
            current_sha: None,
            upstream_sha: None,
            error: None,
        }));
    }
    let dir = git_dir
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    // Network fetch as root into the app's mirror (root's deploy keys), then
    // everything in the checkout as its owner: see app_git.rs. Capped at 15s
    // so a dead remote cannot hang the /apps page.
    normalize_checkout_owner(&state.config, &dir).await;
    let owner = app_git::owner_of(std::path::Path::new(&dir))?;
    let fetch_err = match fetch_url(&state, &name, owner.as_ref(), &dir).await {
        None => Some(
            "no Repository URL: set it on the app page so the panel can check for updates".to_string(),
        ),
        Some(url) => {
            let gid = owner.as_ref().map(|o| o.gid);
            match tokio::time::timeout(
                std::time::Duration::from_secs(15),
                app_git::update_mirror(&name, &url, gid, None),
            )
            .await
            {
                Err(_) => Some("git fetch timed out after 15s".to_string()),
                Ok(Err(e)) => Some(format!("git fetch failed: {e}")),
                Ok(Ok(mirror)) => {
                    let synced = async {
                        app_git::point_origin_at_mirror(owner.as_ref(), &dir, &mirror).await?;
                        app_git::owner_git(owner.as_ref(), Some(&dir), Some(&mirror), &["fetch", "--quiet", "origin"])
                            .await
                    }
                    .await;
                    synced.err().map(|e| format!("git fetch failed: {e}"))
                }
            }
        }
    };

    let current_sha = app_git::owner_git(owner.as_ref(), Some(&dir), None, &["rev-parse", "--short", "HEAD"])
        .await
        .ok()
        .map(|o| o.stdout.trim().to_string());
    let upstream_sha = app_git::owner_git(owner.as_ref(), Some(&dir), None, &["rev-parse", "--short", "@{u}"])
        .await
        .ok()
        .map(|o| o.stdout.trim().to_string());

    if upstream_sha.is_none() {
        return Ok(Json(GitStatus {
            has_repo: true,
            behind: 0,
            ahead: 0,
            current_sha,
            upstream_sha: None,
            error: Some(
                "no upstream tracking branch (HEAD doesn't track a remote)".into(),
            ),
        }));
    }

    let count_out = app_git::owner_git(
        owner.as_ref(),
        Some(&dir),
        None,
        &["rev-list", "--left-right", "--count", "HEAD...@{u}"],
    )
    .await;
    let (ahead, behind) = match count_out {
        Ok(o) => {
            // Output is "<ahead>\t<behind>" with HEAD on the left,
            // upstream on the right per the `...` syntax.
            let mut parts = o.stdout.split_whitespace();
            let a = parts.next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
            let b = parts.next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
            (a, b)
        }
        Err(_) => (0, 0),
    };

    Ok(Json(GitStatus {
        has_repo: true,
        behind,
        ahead,
        current_sha,
        upstream_sha,
        error: fetch_err,
    }))
}

pub async fn get(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> AgentResult<Json<AppRow>> {
    validate_name(&name)?;

    // 1. Try the panel-managed path: row in apps table OR a vps-app-*.service
    //    unit file on disk. lookup_app() covers both via its DB lookup + unit
    //    file fallback.
    let panel_data = lookup_app(&state, &name).await;
    if let Some((subdomain, port, source_dir)) = panel_data {
        let unit = format!("{VPS_APP_PREFIX}{name}.service");
        let out = exec::run("systemctl", ["show", &unit, "--no-pager"]).await?;
        let active = field(&out.stdout, "ActiveState=").unwrap_or_default();
        let sub = field(&out.stdout, "SubState=").unwrap_or_default();
        let docker = docker_apps::read_config(&name).await;
        let source_dir_state = source_dir_state_for(&source_dir, docker.is_some()).await;
        return Ok(Json(AppRow {
            name,
            subdomain,
            port,
            source_dir,
            active,
            sub,
            kind: "panel".into(),
            source_dir_state,
            runtime: runtime_label(docker.is_some()),
            image: docker.map(|d| d.image),
        }));
    }

    // 2. Not panel-managed — does /srv/apps/<name>/ exist? If so it was
    //    detected by list()'s drift scan as an "external" app and the
    //    operator now wants to open its detail page (e.g. to recover it
    //    via the staged deploy buttons).
    let source_dir = format!("{APPS_BASE_DIR}/{name}");
    let meta = tokio::fs::metadata(&source_dir).await.ok();
    if meta.as_ref().is_some_and(|m| m.is_dir()) {
        let (active, sub) = external_service_status(&name).await;
        let source_dir_state = classify_dir(&source_dir).await;
        return Ok(Json(AppRow {
            name,
            subdomain: String::new(),
            port: 0,
            source_dir,
            active,
            sub,
            kind: "external".into(),
            source_dir_state,
            runtime: runtime_label(false),
            image: None,
        }));
    }

    // 3. Neither panel-managed nor on disk — genuinely doesn't exist.
    Err(AgentError::NotFound)
}

pub async fn create(
    State(state): State<AppState>,
    Json(req): Json<CreateApp>,
) -> AgentResult<Json<TaskAccepted>> {
    validate_name(&req.name)?;
    if !is_safe_subdomain(&req.subdomain) {
        return Err(AgentError::Invalid("invalid subdomain".into()));
    }
    if !(1024..65535).contains(&req.port) {
        return Err(AgentError::Invalid("port must be in 1024..65535".into()));
    }
    // SECURITY: exec_start is rendered verbatim into the unit's `ExecStart=` line
    // (AppServiceTemplate uses askama escape="none"). A newline/CR in the value
    // would let a caller append extra [Service] directives — e.g. inject
    // `User=root` (systemd applies last-wins, overriding the template's
    // User=apps) plus an `ExecStartPre=` payload — turning a deploy into RCE as
    // root the moment the unit is enabled. Reject any control character before
    // the value can reach the template. Reachable via the public MCP apps.create
    // tool and any panel session, so the agent is the enforcement point.
    if let Some(exec_start) = &req.exec_start {
        if exec_start.chars().any(|c| c.is_control()) {
            return Err(AgentError::Invalid(
                "exec_start must be a single line with no control characters".into(),
            ));
        }
        if exec_start.trim().is_empty() {
            return Err(AgentError::Invalid("exec_start must not be empty".into()));
        }
    }
    // Keys become lines of a systemd EnvironmentFile: a newline or `=` in a
    // key would inject extra variables.
    if let Some(env) = &req.env {
        validate_env_keys(env)?;
    }
    let mut req = req;
    if resolve_runtime(&req).await? == "docker" {
        req.runtime = Some("docker".into());
        match req.docker.take() {
            Some(spec) => {
                let spec = docker_apps::normalize_spec(spec);
                docker_apps::validate_spec(&spec)?;
                req.docker = Some(spec);
            }
            None if docker_apps::is_docker_app(&req.name) => {}
            None => {
                return Err(AgentError::Invalid(
                    "a new Docker app needs its image and container port".into(),
                ))
            }
        }
    }
    refuse_if_app_task_running(&state, &req.name)?;

    let task = state.tasks.create(&format!("apps.create:{}", req.name));
    let task_id = task.id.clone();
    let cfg = state.config.clone();
    let panel_db = state.panel_db.clone();

    tokio::spawn(async move {
        task.start();
        let acme_email = crate::handlers::ssl::resolve_acme_email(&panel_db, &cfg);
        let result = run_deploy(&cfg, &acme_email, &req, &task).await;
        match result {
            Ok(()) => {
                task.log("done");
                task.finish(true);
            }
            Err(e) => {
                task.log(format!("error: {e}"));
                task.finish(false);
            }
        }
    });

    Ok(Json(TaskAccepted { task_id }))
}

/// "node" or "docker" for a deploy request. An app keeps the runtime it was
/// deployed with: the other runtime's unit would replace it but leave its
/// files (a build, volumes) behind, so switching means delete + deploy.
async fn resolve_runtime(req: &CreateApp) -> AgentResult<&'static str> {
    let is_docker = docker_apps::is_docker_app(&req.name);
    let has_unit = PathBuf::from(format!("/etc/systemd/system/{VPS_APP_PREFIX}{}.service", req.name)).exists();
    let wanted = match req.runtime.as_deref() {
        None if is_docker => "docker",
        None | Some("node") => "node",
        Some("docker") => "docker",
        Some(other) => {
            return Err(AgentError::Invalid(format!(
                "unknown runtime {other:?} - expected node or docker"
            )))
        }
    };
    if wanted == "node" && is_docker {
        return Err(AgentError::Conflict(format!(
            "{} is a Docker app - delete it first to deploy a Node app under that name",
            req.name
        )));
    }
    if wanted == "docker" && has_unit && !is_docker {
        return Err(AgentError::Conflict(format!(
            "{} is a Node app - delete it first to deploy a Docker app under that name",
            req.name
        )));
    }
    Ok(wanted)
}

async fn run_deploy(
    cfg: &crate::config::Config,
    acme_email: &str,
    req: &CreateApp,
    task: &crate::ipc::tasks::TaskHandle,
) -> AgentResult<()> {
    if req.runtime.as_deref() == Some("docker") {
        return docker_apps::run_deploy(cfg, acme_email, req, task).await;
    }
    let unit_name = format!("{VPS_APP_PREFIX}{}.service", req.name);
    let unit_path = PathBuf::from(format!("/etc/systemd/system/{unit_name}"));
    let env_path = PathBuf::from(format!("/etc/vps-panel/apps/{}.env", req.name));

    // Which stages this request asked for. None / "all" = everything,
    // matching the pre-staged behaviour. Individual stages can be invoked
    // independently from the panel's four-button UI.
    let step = req.step.as_deref().unwrap_or("all");
    let do_pull = matches!(step, "pull" | "all");
    let do_install = matches!(step, "install" | "all");
    let do_build = matches!(step, "build" | "all");
    let do_deploy = matches!(step, "deploy" | "all");
    if !do_pull && !do_install && !do_build && !do_deploy {
        return Err(AgentError::Invalid(format!(
            "unknown step '{step}' — expected pull / install / build / deploy / all"
        )));
    }
    // Fail before pull/install/build touch the tree, not after them.
    if do_deploy && port_held_by_other(req.port, &unit_name).await {
        return Err(AgentError::Conflict(format!(
            "port {} is in use by something other than {unit_name}",
            req.port
        )));
    }

    // Stage 1: PULL — git clone or pull, then chown to apps user.
    if do_pull {
        if let Some(repo_url) = req.repo_url.as_deref() {
            let repo_url = repo_url.trim();
            if !is_safe_git_url(repo_url) {
                return Err(AgentError::Invalid(format!(
                    "refusing repo URL {repo_url:?} - must be https://… or git@…:…"
                )));
            }
            // Friendly check for the common mistake: writing the SSH config
            // Host alias as the *user* (`<alias>@github.com:…`) instead of
            // as the *host* (`git@<alias>:…`). github/gitlab/bitbucket all
            // require the SSH user to be "git" — anything else fails with
            // a confusing "Permission denied (publickey)" because the
            // username never gets to the public-key auth phase.
            if let Some(misuse_hint) = misused_alias_hint(repo_url) {
                return Err(AgentError::Invalid(misuse_hint));
            }
            let src = PathBuf::from(&req.source_dir);
            let is_existing_clone = src.join(".git").is_dir();
            // Root fetches into the app's mirror; the checkout is only ever
            // touched by its owner (the apps user for a new clone). See
            // app_git.rs.
            let owner = if is_existing_clone {
                normalize_checkout_owner(cfg, &req.source_dir).await;
                app_git::owner_of(&src)?
            } else {
                app_git::owner_for_uid(cfg.apps_uid)?
            };
            let mirror = app_git::update_mirror(&req.name, repo_url, owner.as_ref().map(|o| o.gid), Some(task))
                .await?;
            if is_existing_clone {
                app_git::point_origin_at_mirror(owner.as_ref(), &req.source_dir, &mirror).await?;
                task.log(format!(
                    "pulling latest from {repo_url} into {}",
                    src.display()
                ));
                app_git::owner_git_logged(
                    owner.as_ref(),
                    Some(&req.source_dir),
                    Some(&mirror),
                    &["pull", "--ff-only"],
                    task,
                )
                .await?;
            } else {
                if src.exists() {
                    let mut rd = tokio::fs::read_dir(&src).await.map_err(AgentError::Io)?;
                    if rd.next_entry().await.map_err(AgentError::Io)?.is_some() {
                        return Err(AgentError::Conflict(format!(
                            "source_dir {} is non-empty and not a git checkout - refusing to clone over it",
                            src.display()
                        )));
                    }
                } else {
                    tokio::fs::create_dir_all(&src).await.map_err(AgentError::Io)?;
                }
                // The owner clones into it, so it must own the empty folder.
                if cfg.apps_uid != 0 {
                    fs_atomic::chown_blocking(&src, cfg.apps_uid, cfg.apps_gid)?;
                }
                app_git::trust_mirror(owner.as_ref(), &mirror).await?;
                task.log(format!("cloning {repo_url} into {}", src.display()));
                app_git::owner_git_logged(
                    owner.as_ref(),
                    None,
                    Some(&mirror),
                    &["clone", "--no-hardlinks", &mirror, &req.source_dir],
                    task,
                )
                .await?;
            }
            if cfg.apps_uid != 0 {
                let _ = exec::run("chown", ["-R", "apps:apps", &req.source_dir]).await;
            }
        } else if step == "pull" {
            return Err(AgentError::Invalid(
                "pull step requires a repo_url".into(),
            ));
        } else {
            task.log("no repo_url - skipping git pull step");
        }
    }

    // Install and build replace the node_modules and build output the running
    // app starts from. Save them first and put them back if either stage fails,
    // so a registry blip or a broken commit never leaves the app unstartable.
    let snapshot = if (do_install || do_build) && PathBuf::from(&req.source_dir).is_dir() {
        Some(ArtifactSnapshot::take(&req.name, &req.source_dir, task).await)
    } else {
        None
    };
    let staged: AgentResult<()> = async {
        // Stage 2: INSTALL — npm ci / npm install.
        if do_install {
            let src = PathBuf::from(&req.source_dir);
            if !src.is_dir() {
                return Err(AgentError::Invalid(format!(
                    "source_dir does not exist: {} - run Pull first",
                    req.source_dir
                )));
            }
            // Make the tree apps-owned BEFORE npm runs (run_npm drops to the apps
            // user) so install lifecycle scripts can still write node_modules. The
            // post-stage chown below is now a backstop.
            if cfg.apps_uid != 0 {
                let _ = exec::run("chown", ["-R", "apps:apps", &req.source_dir]).await;
            }
            if src.join("package-lock.json").is_file() {
                task.log("installing deps (npm ci)");
                run_npm(
                    cfg,
                    &["--prefix", &req.source_dir, "ci", "--no-audit", "--no-fund"],
                    &[],
                    task,
                )
                .await?;
            } else if src.join("package.json").is_file() {
                task.log("installing deps (npm install)");
                run_npm(
                    cfg,
                    &[
                        "--prefix",
                        &req.source_dir,
                        "install",
                        "--no-audit",
                        "--no-fund",
                    ],
                    &[],
                    task,
                )
                .await?;
            } else {
                task.log("no package.json - skipping npm install");
            }
            if cfg.apps_uid != 0 {
                let _ = exec::run("chown", ["-R", "apps:apps", &req.source_dir]).await;
            }
        }

        // Stage 3: BUILD — npm run build (only if a build script is defined).
        if do_build {
            let src = PathBuf::from(&req.source_dir);
            if !src.is_dir() {
                return Err(AgentError::Invalid(format!(
                    "source_dir does not exist: {} - run Pull first",
                    req.source_dir
                )));
            }
            let pkg_path = src.join("package.json");
            let mut has_build = false;
            if let Ok(body) = tokio::fs::read_to_string(&pkg_path).await {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
                    has_build = json
                        .get("scripts")
                        .and_then(|s| s.get("build"))
                        .and_then(|v| v.as_str())
                        .is_some();
                }
            }
            if has_build {
                task.log("running npm run build");
                if cfg.apps_uid != 0 {
                    let _ = exec::run("chown", ["-R", "apps:apps", &req.source_dir]).await;
                }
                // The app's variables reach the build: NEXT_PUBLIC_* and anything
                // next.config reads are baked in at build time. The request's env
                // when it carries one, else the env file already on disk.
                let build_env: Vec<(String, String)> = match &req.env {
                    Some(env) => env.iter().map(|kv| (kv.key.clone(), kv.value.clone())).collect(),
                    None => build_env_for(&req.name).await,
                };
                run_npm(cfg, &["--prefix", &req.source_dir, "run", "build"], &build_env, task).await?;
                if cfg.apps_uid != 0 {
                    let _ = exec::run("chown", ["-R", "apps:apps", &req.source_dir]).await;
                }
            } else {
                task.log("no `build` script in package.json - skipping build step");
            }
        }
        Ok(())
    }
    .await;
    if let Some(snapshot) = &snapshot {
        match &staged {
            Ok(()) => snapshot.discard().await,
            Err(_) => {
                snapshot.restore(task).await;
            }
        }
    }
    staged?;

    // Anything past here is the DEPLOY stage. Bail early for the lighter
    // stages so we don't try to validate the systemd unit or vhost when
    // the operator just wanted Pull / Install / Build.
    if !do_deploy {
        return Ok(());
    }

    task.log(format!("validating source dir {}", req.source_dir));
    if !PathBuf::from(&req.source_dir).is_dir() {
        return Err(AgentError::Invalid(format!(
            "source_dir does not exist: {}",
            req.source_dir
        )));
    }

    if port_held_by_other(req.port, &unit_name).await {
        return Err(AgentError::Conflict(format!(
            "port {} is in use by something other than {unit_name}",
            req.port
        )));
    }

    if !req.skip_dns_check {
        task.log(format!("checking DNS for {}", req.subdomain));
        if let Err(e) = dns_resolves_locally(&req.subdomain).await {
            task.log(format!("dns check warning: {e} (continuing)"));
        }
    }

    write_deploy_env(cfg, req.env.as_deref(), &env_path, task).await?;
    let exec_start = match &req.exec_start {
        Some(s) => s.clone(),
        None => existing_exec_start(&unit_path)
            .await
            .unwrap_or_else(|| "/usr/bin/npm start".to_string()),
    };

    // A failure below removes the unit only if this deploy created it: on a
    // re-deploy it is the existing app's unit.
    let unit_existed = unit_path.exists();
    task.log(format!("writing systemd unit {}", unit_path.display()));
    let unit_body = AppServiceTemplate {
        name: &req.name,
        working_dir: &req.source_dir,
        exec_start: &exec_start,
        user: "apps",
        env_file: env_path.to_str().unwrap(),
        port: req.port,
    }
    .render()
    .map_err(|e| AgentError::Other(anyhow::anyhow!("askama: {e}")))?;
    fs_atomic::write(&unit_path, unit_body.as_bytes(), 0o644).await?;

    task.log("systemctl daemon-reload");
    exec::run("systemctl", ["daemon-reload"]).await?;
    // restart, not start: on a re-deploy the unit is already running and
    // must pick up the new unit file and build.
    task.log(format!("systemctl enable + restart {unit_name}"));
    let _ = exec::run("systemctl", ["enable", &unit_name]).await;
    if let Err(e) = app_systemctl(&["restart"], &unit_name).await {
        if !unit_existed {
            rollback_unit(&unit_path).await;
        }
        return Err(e);
    }

    // Give the app up to 45s to bind (TypeScript compile-on-start, first-run
    // DB migrations, `npm start` overhead), then require it to stay up without
    // systemd restarting it. On failure we DO NOT roll back the unit: an
    // installed-but-stopped unit is what an operator needs to investigate,
    // not a vanished one. The journal tail goes into the task log so the
    // reason (crash trace / missing env / port mismatch) is visible in /tasks
    // without shell access.
    const PORT_WAIT_SECS: u64 = 45;
    task.log(format!(
        "waiting up to {PORT_WAIT_SECS}s for the app to bind 127.0.0.1:{}, then {APP_STABLE_SECS}s to confirm it stays up",
        req.port
    ));
    if let Err(reason) = verify_app_up(&unit_name, req.port, Duration::from_secs(PORT_WAIT_SECS)).await {
        task.log(format!("=== {reason} ==="));
        log_journal_tail(&unit_name, 80, task).await;
        // Stop AND disable, so it neither keeps crashing nor comes back on
        // reboot. It stays installed for `systemctl status` / a fixed re-run.
        let _ = exec::run("systemctl", ["disable", "--now", &unit_name]).await;
        return Err(AgentError::Conflict(format!(
            "{reason}. See the journal lines above for why. The systemd unit is left installed at {} \
             (stopped and disabled) so you can `systemctl status {unit_name}` or re-run Deploy after fixing.",
            unit_path.display(),
        )));
    }

    publish_vhost(cfg, acme_email, req, &unit_name, &unit_path, unit_existed, task).await
}

/// The deploy's EnvironmentFile: the request's variables replace the file,
/// none keeps it (an empty one for a new app). root:apps 0640.
pub(crate) async fn write_deploy_env(
    cfg: &crate::config::Config,
    env: Option<&[EnvKv]>,
    env_path: &std::path::Path,
    task: &crate::ipc::tasks::TaskHandle,
) -> AgentResult<()> {
    match env {
        Some(env) => {
            task.log(format!("writing env file {}", env_path.display()));
            fs_atomic::ensure_dir(env_path.parent().unwrap(), 0o750).await?;
            fs_atomic::write(env_path, render_env_file(env).as_bytes(), 0o640).await?;
        }
        None if env_path.exists() => {
            task.log(format!("keeping the existing env file {}", env_path.display()));
        }
        None => {
            task.log(format!("writing an empty env file {}", env_path.display()));
            fs_atomic::ensure_dir(env_path.parent().unwrap(), 0o750).await?;
            fs_atomic::write(env_path, b"", 0o640).await?;
        }
    }
    if cfg.apps_uid != 0 {
        let _ = fs_atomic::chown_blocking(env_path, 0, cfg.apps_gid);
    }
    Ok(())
}

/// The last deploy stage, once the unit is up: the app's own vhost (unless a
/// /domains site already routes the name) and its certificate.
pub(crate) async fn publish_vhost(
    cfg: &crate::config::Config,
    acme_email: &str,
    req: &CreateApp,
    unit_name: &str,
    unit_path: &std::path::Path,
    unit_existed: bool,
    task: &crate::ipc::tasks::TaskHandle,
) -> AgentResult<()> {
    let vhost_path = PathBuf::from(format!("/etc/nginx/sites-available/{}", req.subdomain));
    let vhost_link = PathBuf::from(format!("/etc/nginx/sites-enabled/{}", req.subdomain));

    // If the operator has already provisioned a port-mode site under
    // /domains for this subdomain (file `/etc/nginx/sites-available/site-
    // <domain>`), the HTTP routing slot exists — proxy_pass, SSL block,
    // ACME helper are all already wired up. Skip creating a duplicate
    // app vhost; the systemd unit binding `127.0.0.1:<port>` is enough for
    // traffic to start flowing.
    let site_vhost = PathBuf::from(format!(
        "/etc/nginx/sites-available/site-{}",
        req.subdomain
    ));
    let site_owns_slot = site_vhost.exists();
    let cert_path = format!("/etc/vps-panel/certs/{}/fullchain.pem", req.subdomain);
    let key_path = format!("/etc/vps-panel/certs/{}/privkey.pem", req.subdomain);
    let acme_webroot = crate::nginx_template::ACME_WEBROOT;
    let cert_exists = std::path::Path::new(&cert_path).exists();

    if site_owns_slot {
        task.log(format!(
            "skipping app vhost — site already serves {} ({})",
            req.subdomain,
            site_vhost.display()
        ));
    } else {
        task.log(format!("writing nginx vhost {}", vhost_path.display()));
        let vhost_body = AppVhostTemplate {
            name: &req.name,
            server_name: &req.subdomain,
            upstream_port: req.port,
            ssl_enabled: cert_exists,
            cert_path: &cert_path,
            key_path: &key_path,
            acme_webroot,
        }
        .render()
        .map_err(|e| AgentError::Other(anyhow::anyhow!("askama: {e}")))?;
        fs_atomic::write(&vhost_path, vhost_body.as_bytes(), 0o644).await?;

        if !vhost_link.exists() {
            symlink(&vhost_path, &vhost_link).map_err(AgentError::Io)?;
        }

        task.log("nginx -t");
        if let Err(e) = nginx_handler::test_config().await {
            let _ = std::fs::remove_file(&vhost_link);
            let _ = std::fs::remove_file(&vhost_path);
            let _ = exec::run("systemctl", ["stop", unit_name]).await;
            let _ = exec::run("systemctl", ["disable", unit_name]).await;
            if !unit_existed {
                rollback_unit(unit_path).await;
            }
            return Err(e);
        }
        task.log("systemctl reload nginx");
        exec::run("systemctl", ["reload", "nginx"]).await?;
    }

    // SSL is owned by the site under /domains when the operator picked an
    // existing slot — skip even if the API caller asked for it, otherwise
    // we'd race the site for the same cert path. The /domains UI is the
    // single place to manage SSL for the subdomain.
    if req.issue_ssl && !cert_exists && !site_owns_slot {
        task.log("issuing SSL via instant-acme");
        if let Err(e) = crate::handlers::ssl::issue_for(cfg, acme_email, &req.subdomain, task).await {
            task.log(format!("ssl issuance failed: {e} (vhost remains on HTTP)"));
        } else {
            task.log("rewriting vhost with SSL enabled");
            let vhost_body = AppVhostTemplate {
                name: &req.name,
                server_name: &req.subdomain,
                upstream_port: req.port,
                ssl_enabled: true,
                cert_path: &cert_path,
                key_path: &key_path,
                acme_webroot,
            }
            .render()
            .map_err(|e| AgentError::Other(anyhow::anyhow!("askama: {e}")))?;
            fs_atomic::write(&vhost_path, vhost_body.as_bytes(), 0o644).await?;
            nginx_handler::test_config().await?;
            exec::run("systemctl", ["reload", "nginx"]).await?;
        }
    }

    Ok(())
}

/// Detect a repo URL of the form `<word>@<host>:<path>` where `<host>` is
/// a well-known git provider (github / gitlab / bitbucket) and `<word>` is
/// anything other than `git`. Returns a clear actionable error string.
/// Returns None for any URL the agent should just hand to git as-is.
fn misused_alias_hint(repo_url: &str) -> Option<String> {
    let providers = ["github.com", "gitlab.com", "bitbucket.org"];
    let at_pos = repo_url.find('@')?;
    let after_at = &repo_url[at_pos + 1..];
    let colon_pos = after_at.find(':')?;
    let host_part = &after_at[..colon_pos];
    if !providers.iter().any(|p| host_part.eq_ignore_ascii_case(p)) {
        return None;
    }
    let user_part = &repo_url[..at_pos];
    if user_part == "git" {
        return None;
    }
    let path_part = &after_at[colon_pos + 1..];
    Some(format!(
        "git URL '{repo_url}' uses '{user_part}@' as the SSH user, but {host_part} requires the user to be 'git'. \
        If '{user_part}' is a Host alias from /root/.ssh/config, the correct form is 'git@{user_part}:{path_part}' \
        (alias in the *host* position, not the user)."
    ))
}

/// Refuse anything that doesn't look like a normal git remote URL. We
/// allow `https://`, `http://` (lab use only), and the `git@host:path`
/// SSH shorthand. Refuse `file://`, raw paths, and anything with a shell
/// metacharacter so the URL can't escape the argv.
fn is_safe_git_url(url: &str) -> bool {
    if url.is_empty() || url.len() > 1024 {
        return false;
    }
    if url
        .chars()
        .any(|c| c == ' ' || c == ';' || c == '|' || c == '&' || c == '$' || c == '`' || c == '\n')
    {
        return false;
    }
    url.starts_with("https://")
        || url.starts_with("http://")
        || (url.contains('@') && url.contains(':') && !url.starts_with('/'))
}

/// Run a subprocess and stream every line of stdout/stderr to the task
/// log. Returns Err if the exit code is non-zero — caller's `?` aborts
/// the deploy flow and `TaskHandle::drop` marks the task as failed.
async fn stream_subprocess<S, I>(
    cmd: &str,
    args: I,
    task: &crate::ipc::tasks::TaskHandle,
) -> AgentResult<()>
where
    S: AsRef<std::ffi::OsStr>,
    I: IntoIterator<Item = S>,
{
    let code = exec::run_streaming(cmd, args, |line| {
        task.log(line.to_string());
    })
    .await?;
    if code != 0 {
        return Err(AgentError::Subprocess {
            cmd: cmd.to_string(),
            code,
            stderr: format!("{cmd} exited with code {code} (see task log above)"),
        });
    }
    Ok(())
}

/// POSIX single-quote shell escape. Safe for paths the agent constructs
/// (validated app names, repo URLs already passed `is_safe_git_url`).
fn shell_quote(s: &str) -> String {
    let escaped = s.replace('\'', "'\\''");
    format!("'{escaped}'")
}

/// Run an `npm` subcommand (install / build) for a deploy or update as the
/// UNPRIVILEGED `apps` user instead of root.
///
/// SECURITY (C7): `npm ci`/`install` execute the repo's preinstall/postinstall/
/// prepare lifecycle scripts, and `npm run build` executes the repo's build
/// script — i.e. arbitrary code from the cloned repository. Previously these
/// ran as the agent's identity (root), so any repo (deployable via the panel OR
/// the public MCP `apps.create`/`apps.update` tools) was root RCE. We drop to
/// the `apps` user via `sudo -u apps -H` (runs without a password since we are
/// root, and `-H` sets HOME to apps's home so npm's cache is writable). This
/// mirrors the existing `sudo -u postgres` pattern. The caller chowns the
/// source tree to apps:apps FIRST so the unprivileged npm can still write
/// node_modules / build output. `umask 022` keeps node_modules/.bin world-exec
/// so the app's systemd unit (User=apps) can run them.
///
/// Falls back to the previous root behaviour ONLY when no `apps` user exists
/// (apps_uid == 0), so minimal boxes without the apps account still deploy.
///
/// `envs` are exported for this npm run only (sudo resets the environment,
/// so they cannot simply be inherited). Pass the app's variables for
/// `npm run build`, nothing for install: a `NODE_ENV=production` in the app's
/// env would make `npm ci` skip devDependencies the build needs.
async fn run_npm(
    cfg: &crate::config::Config,
    args: &[&str],
    envs: &[(String, String)],
    task: &crate::ipc::tasks::TaskHandle,
) -> AgentResult<()> {
    let mut script = String::from("umask 022");
    for (key, value) in envs {
        // Keys are re-checked because they are spliced into the script;
        // values are single-quoted.
        if is_valid_env_key(key) {
            script.push_str(&format!(" && export {key}={}", shell_quote(value)));
        }
    }
    script.push_str(" && exec npm");
    for arg in args {
        script.push(' ');
        script.push_str(&shell_quote(arg));
    }
    if cfg.apps_uid == 0 {
        return stream_subprocess("sh", ["-c", script.as_str()], task).await;
    }
    stream_subprocess(
        "sudo",
        ["-u", "apps", "-H", "sh", "-c", script.as_str()],
        task,
    )
    .await
}

pub(crate) async fn rollback_unit(unit_path: &std::path::Path) {
    let _ = fs_atomic::remove_if_exists(unit_path).await;
    let _ = exec::run("systemctl", ["daemon-reload"]).await;
}

const APP_UNIT_DROPIN_DIR: &str = "/etc/systemd/system/vps-app-.service.d";
const APP_UNIT_DROPIN: &str = "/etc/systemd/system/vps-app-.service.d/10-vps-panel-start-limit.conf";
const APP_UNIT_DROPIN_BODY: &str = "\
# Written by vps-panel-agent (apps.rs::ensure_app_unit_defaults). systemd applies
# a `vps-app-.service.d` drop-in to every vps-app-*.service.
#
# The unit restarts on failure after 2s, too slowly for systemd's default start
# limit (5 starts in 10s) to ever trip, so an app that crashed on boot restarted
# forever. After 5 starts in 5 minutes the unit now stays failed. Panel actions
# run `systemctl reset-failed` first, so Start/Restart/Update always go through.
[Unit]
StartLimitIntervalSec=300
StartLimitBurst=5
";

/// Install the drop-in above. Runs at every agent start, so existing boxes pick
/// it up on their next update without any unit being re-rendered. No-op when
/// the file is already current.
pub async fn ensure_app_unit_defaults() {
    let current = tokio::fs::read_to_string(APP_UNIT_DROPIN).await.ok();
    if current.as_deref() == Some(APP_UNIT_DROPIN_BODY) {
        return;
    }
    let dir = std::path::Path::new(APP_UNIT_DROPIN_DIR);
    if let Err(e) = fs_atomic::ensure_dir(dir, 0o755).await {
        tracing::warn!(error = %e, "could not create {APP_UNIT_DROPIN_DIR}");
        return;
    }
    let path = std::path::Path::new(APP_UNIT_DROPIN);
    if let Err(e) = fs_atomic::write(path, APP_UNIT_DROPIN_BODY.as_bytes(), 0o644).await {
        tracing::warn!(error = %e, "could not write {APP_UNIT_DROPIN}");
        return;
    }
    let _ = exec::run("systemctl", ["daemon-reload"]).await;
    tracing::info!("installed app unit start limit drop-in");
}

/// `systemctl <args> <unit>` for an operator action on an app unit (Start,
/// Restart, Update, env and entrypoint saves). Clears the unit's failed state
/// and start-rate counter first, so the crash-loop limit above never refuses
/// an action the operator asked for.
pub(crate) async fn app_systemctl(args: &[&str], unit: &str) -> AgentResult<()> {
    let _ = exec::run("systemctl", ["reset-failed", unit]).await;
    let mut full: Vec<&str> = args.to_vec();
    full.push(unit);
    exec::run("systemctl", full).await.map(|_| ())
}

pub(crate) async fn unit_prop(unit: &str, prop: &str) -> String {
    exec::run("systemctl", ["show", "-p", prop, "--value", unit])
        .await
        .map(|o| o.stdout.trim().to_string())
        .unwrap_or_default()
}

async fn unit_restarts(unit: &str) -> u64 {
    unit_prop(unit, "NRestarts").await.parse().unwrap_or(0)
}

/// Seconds a restarted app must stay up, with no automatic restart, before a
/// deploy or update counts as done.
const APP_STABLE_SECS: u64 = 10;

/// Did the unit really come up? `systemctl restart` returns as soon as a
/// Type=simple process is spawned, so an app that dies 100 ms later still
/// "restarted", and one TCP connect can reach a process about to crash
/// (`next start` listens before it looks for its build). Healthy means: the
/// port answers within `bind_wait`, then the unit stays active for
/// APP_STABLE_SECS without systemd restarting it. Call right after the start
/// (an explicit start resets NRestarts). Err carries a one-line reason.
async fn verify_app_up(unit: &str, port: u16, bind_wait: Duration) -> Result<(), String> {
    verify_unit_up(unit, port, bind_wait, Readiness::Listening).await
}

/// When an app counts as started.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Readiness {
    /// Something accepts connections on the port (a Node app binds it itself).
    Listening,
    /// The port answers an HTTP request with at least one byte. Docker's
    /// proxy accepts on a published port as soon as the container exists and
    /// drops the connection while the app inside is still starting.
    HttpAnswer,
}

pub(crate) async fn verify_unit_up(
    unit: &str,
    port: u16,
    bind_wait: Duration,
    readiness: Readiness,
) -> Result<(), String> {
    use tokio::net::TcpStream;
    let baseline = unit_restarts(unit).await;
    let deadline = tokio::time::Instant::now() + bind_wait;
    loop {
        if unit_restarts(unit).await > baseline {
            return Err(format!("{unit} crashed while starting and systemd restarted it"));
        }
        let state = unit_prop(unit, "ActiveState").await;
        if state == "failed" || state == "inactive" {
            return Err(format!("{unit} exited while starting (state: {state})"));
        }
        let ready = match readiness {
            Readiness::Listening => TcpStream::connect(("127.0.0.1", port)).await.is_ok(),
            Readiness::HttpAnswer => http_answers(port).await,
        };
        if ready {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(match readiness {
                Readiness::Listening => format!("{unit} did not bind port {port} within {}s", bind_wait.as_secs()),
                Readiness::HttpAnswer => format!(
                    "{unit} did not answer HTTP on 127.0.0.1:{port} within {}s (is the container port right?)",
                    bind_wait.as_secs()
                ),
            });
        }
        sleep(Duration::from_millis(500)).await;
    }
    for _ in 0..APP_STABLE_SECS {
        sleep(Duration::from_secs(1)).await;
        if unit_restarts(unit).await > baseline || unit_prop(unit, "ActiveState").await != "active" {
            return Err(format!(
                "{unit} bound port {port} but crashed within {APP_STABLE_SECS}s"
            ));
        }
    }
    Ok(())
}

/// Any reply at all (an error page or a redirect included) to a plain GET.
async fn http_answers(port: u16) -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let probe = async {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.ok()?;
        stream
            .write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\nUser-Agent: vps-panel-agent\r\n\r\n")
            .await
            .ok()?;
        let mut byte = [0u8; 1];
        (stream.read(&mut byte).await.ok()? > 0).then_some(())
    };
    matches!(tokio::time::timeout(Duration::from_secs(3), probe).await, Ok(Some(())))
}

/// Append the unit's last journal lines to the task log.
pub(crate) async fn log_journal_tail(unit: &str, lines: u32, task: &crate::ipc::tasks::TaskHandle) {
    let n = lines.to_string();
    match exec::run(
        "journalctl",
        ["-u", unit, "-n", n.as_str(), "--no-pager", "--output=short-iso"],
    )
    .await
    {
        Ok(out) => {
            task.log(format!("--- last {lines} journal lines of {unit} ---"));
            for line in out.stdout.lines() {
                task.log(line.to_string());
            }
        }
        Err(_) => task.log(format!("(could not read the journal; run journalctl -u {unit})")),
    }
}

/// What a running Node app needs from install and build. `npm ci` deletes
/// node_modules up front and build tools (next build, vite, CRA, nuxt) empty
/// their output directory before writing, so a failed install or build used to
/// leave the live app with nothing to start from on its next restart.
const RUNTIME_ARTIFACT_DIRS: &[&str] = &["node_modules", ".next", ".output", "dist", "build", "out"];

/// Outside every source tree on purpose: a copy inside the project (say
/// `node_modules.prev`) would be crawled by tsc, eslint and tailwind during the
/// very build it protects.
const BUILD_SNAPSHOT_BASE: &str = "/var/lib/vps-panel/build-snapshots";

/// The app's runtime artifacts as they were before an install/build, so a
/// failure can put them back.
struct ArtifactSnapshot {
    source_dir: PathBuf,
    dir: PathBuf,
    names: Vec<&'static str>,
}

impl ArtifactSnapshot {
    /// Hard-link copies (`cp -al`): fast and free on disk. Safe because npm and
    /// the build tools delete and recreate files instead of rewriting them in
    /// place. A directory that cannot be linked (different filesystem) is
    /// logged and left unprotected.
    async fn take(app: &str, source_dir: &str, task: &crate::ipc::tasks::TaskHandle) -> Self {
        let dir = PathBuf::from(format!("{BUILD_SNAPSHOT_BASE}/{app}"));
        let mut snap = ArtifactSnapshot {
            source_dir: PathBuf::from(source_dir),
            dir: dir.clone(),
            names: Vec::new(),
        };
        // Left over from an update the agent restart interrupted.
        let _ = tokio::fs::remove_dir_all(&dir).await;
        if let Err(e) = fs_atomic::ensure_dir(&dir, 0o700).await {
            task.log(format!("could not save the current build ({e}); a failed build cannot be undone"));
            return snap;
        }
        for name in RUNTIME_ARTIFACT_DIRS {
            let live = snap.source_dir.join(name);
            let is_dir = tokio::fs::symlink_metadata(&live)
                .await
                .map(|m| m.is_dir())
                .unwrap_or(false);
            if !is_dir {
                continue;
            }
            match exec::run("cp", [std::ffi::OsStr::new("-al"), live.as_os_str(), dir.join(name).as_os_str()]).await {
                Ok(_) => snap.names.push(name),
                Err(e) => task.log(format!(
                    "could not save {} ({e}); a failed build cannot restore it",
                    live.display()
                )),
            }
        }
        if !snap.names.is_empty() {
            task.log(format!(
                "saved the current {} so a failed install or build can be undone",
                snap.names.join(", ")
            ));
        }
        snap
    }

    /// Put the saved artifacts back in place of whatever the failed run left.
    async fn restore(&self, task: &crate::ipc::tasks::TaskHandle) -> bool {
        let mut ok = true;
        for name in &self.names {
            let live = self.source_dir.join(name);
            if tokio::fs::symlink_metadata(&live).await.is_ok() {
                if let Err(e) = tokio::fs::remove_dir_all(&live).await {
                    task.log(format!("could not remove the failed {}: {e}", live.display()));
                    ok = false;
                    continue;
                }
            }
            if let Err(e) = tokio::fs::rename(self.dir.join(name), &live).await {
                task.log(format!("could not restore {}: {e}", live.display()));
                ok = false;
            }
        }
        let _ = tokio::fs::remove_dir_all(&self.dir).await;
        if !self.names.is_empty() {
            task.log(if ok {
                format!("restored the previous {}", self.names.join(", "))
            } else {
                "restoring the previous build was incomplete, see above".to_string()
            });
        }
        ok
    }

    async fn discard(&self) {
        let _ = tokio::fs::remove_dir_all(&self.dir).await;
    }
}

/// Classify a directory as "missing" / "empty" / "populated". Cheap: one
/// stat + (if exists) at most one ReadDir poll.
async fn classify_dir(path: &str) -> String {
    match tokio::fs::metadata(path).await {
        Err(_) => "missing".to_string(),
        Ok(meta) if !meta.is_dir() => "missing".to_string(),
        Ok(_) => match tokio::fs::read_dir(path).await {
            Err(_) => "missing".to_string(),
            Ok(mut rd) => match rd.next_entry().await {
                Ok(Some(_)) => "populated".to_string(),
                _ => "empty".to_string(),
            },
        },
    }
}

#[derive(Serialize)]
pub struct SourceStateResp {
    /// "missing" | "empty" | "populated", as classified by `classify_dir`.
    pub state: String,
}

/// Lightweight "is there code at /srv/apps/<name>/ yet?" probe used by the
/// /apps/new form to gate the Deploy button. Returns the classification
/// without requiring the app to be registered in panel.db — Deploy would
/// otherwise blindly write a systemd unit pointing at an empty folder and
/// fail to start. Validates `name` so this can't be abused as a generic
/// filesystem-probe endpoint.
pub async fn source_state(
    Path(name): Path<String>,
) -> AgentResult<Json<SourceStateResp>> {
    validate_name(&name)?;
    let path = format!("{APPS_BASE_DIR}/{name}");
    Ok(Json(SourceStateResp {
        state: classify_dir(&path).await,
    }))
}

#[derive(Deserialize, Default)]
pub struct DeleteQuery {
    /// When true, also `rm -rf` the source directory after stopping the
    /// unit. Guarded server-side: only paths under `/srv/apps/` are wiped.
    #[serde(default)]
    pub wipe: bool,
}

pub async fn delete(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<DeleteQuery>,
) -> AgentResult<Json<OkResponse>> {
    validate_name(&name)?;
    // Read before anything is removed: it names the container, image tags and
    // volumes to clean up.
    let docker = docker_apps::read_config(&name).await;
    let unit = format!("{VPS_APP_PREFIX}{name}.service");
    let _ = exec::run("systemctl", ["stop", &unit]).await;
    let _ = exec::run("systemctl", ["disable", &unit]).await;
    let unit_path = PathBuf::from(format!("/etc/systemd/system/{unit}"));
    let _ = fs_atomic::remove_if_exists(&unit_path).await;
    let env_path = PathBuf::from(format!("/etc/vps-panel/apps/{name}.env"));
    let _ = fs_atomic::remove_if_exists(&env_path).await;

    let (subdomain, _, source_dir) = lookup_app(&state, &name).await.unwrap_or_default();
    // Only the app's own `# vps-app:<name>` vhost: a file of that name that
    // the app did not write (a hand-made vhost) used to be deleted too.
    if !subdomain.is_empty() {
        remove_app_vhost(&name, &subdomain).await;
    }
    let _ = exec::run("systemctl", ["daemon-reload"]).await;

    // A Docker app has no checkout: `wipe` means its data volumes.
    if let Some(spec) = &docker {
        docker_apps::remove_app(&name, spec, q.wipe).await;
        return Ok(Json(OkResponse::default()));
    }

    // The fetch cache is no use without the app.
    app_git::remove_mirror(&name).await;

    // Optional wipe of /srv/apps/<name>. Hard-checked: refuse anything not
    // anchored at APPS_BASE_DIR/ to prevent a corrupt panel.db from making
    // the agent rm -rf an arbitrary path. Best-effort cleanup — the rest
    // of the delete already succeeded by this point.
    if q.wipe {
        let dir = if !source_dir.is_empty() {
            source_dir
        } else {
            format!("{APPS_BASE_DIR}/{name}")
        };
        if dir.starts_with(&format!("{APPS_BASE_DIR}/")) && PathBuf::from(&dir).is_dir() {
            let _ = tokio::fs::remove_dir_all(&dir).await;
        }
    }

    Ok(Json(OkResponse::default()))
}

/// Start = enable + start: the app comes back after a reboot until the
/// operator stops it.
pub async fn start(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> AgentResult<Json<OkResponse>> {
    validate_name(&name)?;
    ensure_has_slot(&state, &name).await?;
    app_systemctl(&["enable", "--now"], &format!("{VPS_APP_PREFIX}{name}.service")).await?;
    Ok(Json(OkResponse::default()))
}

/// Stop = stop + disable. A plain `systemctl stop` left the unit enabled, so a
/// "stopped" app started again on the next reboot (and a broken one crash
/// looped from then on).
pub async fn stop(Path(name): Path<String>) -> AgentResult<Json<OkResponse>> {
    validate_name(&name)?;
    exec::run("systemctl", ["disable", "--now", &format!("{VPS_APP_PREFIX}{name}.service")]).await?;
    Ok(Json(OkResponse::default()))
}

/// Restart also enables, so restarting a stopped app keeps it running after a
/// reboot, the same as Start.
pub async fn restart(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> AgentResult<Json<OkResponse>> {
    validate_name(&name)?;
    ensure_has_slot(&state, &name).await?;
    let unit = format!("{VPS_APP_PREFIX}{name}.service");
    let _ = exec::run("systemctl", ["enable", &unit]).await;
    app_systemctl(&["restart"], &unit).await?;
    Ok(Json(OkResponse::default()))
}

/// One-click "pull + reinstall + restart" for a panel-managed app. Looks up
/// the existing app's source_dir from panel.db (no body required — the
/// caller only knows the name), then runs `git pull --ff-only` → `npm ci`
/// (or `npm install`) → `npm run build` (if defined) → `systemctl restart
/// vps-app-<name>.service`. Returns a task id so the panel can stream the
/// log and redirect on completion. Skips writing the systemd unit / env
/// file / nginx vhost (those are unchanged by an update).
pub async fn update(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> AgentResult<Json<TaskAccepted>> {
    validate_name(&name)?;
    let (_subdomain, port, source_dir) = lookup_app(&state, &name)
        .await
        .ok_or(AgentError::NotFound)?;
    let has_slot = port != 0;
    if docker_apps::is_docker_app(&name) {
        refuse_if_app_task_running(&state, &name)?;
        let task = state.tasks.create(&format!("apps.update:{name}"));
        let task_id = task.id.clone();
        tokio::spawn(async move {
            task.start();
            match docker_apps::run_update(&name, has_slot.then_some(port), &task).await {
                Ok(()) => {
                    task.log("done");
                    task.finish(true);
                }
                Err(e) => {
                    task.log(format!("error: {e}"));
                    task.finish(false);
                }
            }
        });
        return Ok(Json(TaskAccepted { task_id }));
    }
    if source_dir.is_empty() {
        return Err(AgentError::Invalid(
            "app has no recorded source_dir".into(),
        ));
    }
    let src = PathBuf::from(&source_dir);
    if !src.join(".git").is_dir() {
        return Err(AgentError::Invalid(format!(
            "source_dir {source_dir} is not a git checkout - nothing to pull"
        )));
    }

    let repo_url = stored_repo_url(&state, &name);
    if let Some(url) = repo_url.as_deref() {
        if !is_safe_git_url(url) {
            return Err(AgentError::Invalid(format!(
                "refusing repo URL {url:?} - must be https://… or git@…:…"
            )));
        }
        if let Some(misuse_hint) = misused_alias_hint(url) {
            return Err(AgentError::Invalid(misuse_hint));
        }
    }

    refuse_if_app_task_running(&state, &name)?;
    let unit = format!("{VPS_APP_PREFIX}{name}.service");
    let task = state.tasks.create(&format!("apps.update:{name}"));
    let task_id = task.id.clone();
    let cfg = state.config.clone();

    tokio::spawn(async move {
        task.start();
        let result = run_app_update(
            &cfg,
            &name,
            &source_dir,
            repo_url.as_deref(),
            &unit,
            has_slot.then_some(port),
            &task,
        )
        .await;
        match result {
            Ok(()) => {
                task.log("done");
                task.finish(true);
            }
            Err(e) => {
                task.log(format!("error: {e}"));
                task.finish(false);
            }
        }
    });

    Ok(Json(TaskAccepted { task_id }))
}

/// One deploy stage or update per app at a time. Two runs in the same checkout
/// corrupt node_modules/.next, and they would share the app's build snapshot.
pub(crate) fn refuse_if_app_task_running(state: &AppState, name: &str) -> AgentResult<()> {
    for kind in [format!("apps.create:{name}"), format!("apps.update:{name}")] {
        if state.tasks.any_live_kind(&kind) {
            return Err(AgentError::Conflict(format!(
                "a deploy or update of {name} is already running - wait for it to finish"
            )));
        }
    }
    Ok(())
}

/// `slot_port` is None for an app with no routing slot: it is built but not
/// restarted (see set_routing).
async fn run_app_update(
    cfg: &crate::config::Config,
    name: &str,
    source_dir: &str,
    repo_url: Option<&str>,
    unit: &str,
    slot_port: Option<u16>,
    task: &crate::ipc::tasks::TaskHandle,
) -> AgentResult<()> {
    let was_active = unit_prop(unit, "ActiveState").await == "active";

    // 1) Fetch into the app's mirror as root, pull --ff-only in the checkout
    //    as its owner (app_git.rs).
    normalize_checkout_owner(cfg, source_dir).await;
    let owner = app_git::owner_of(std::path::Path::new(source_dir))?;
    let url = match repo_url {
        Some(u) => Some(u.to_string()),
        None => legacy_fetch_url(name, owner.as_ref(), source_dir).await,
    };
    task.log(format!("pulling latest in {source_dir}"));
    match url {
        Some(url) => {
            let mirror =
                app_git::update_mirror(name, &url, owner.as_ref().map(|o| o.gid), Some(task)).await?;
            app_git::point_origin_at_mirror(owner.as_ref(), source_dir, &mirror).await?;
            app_git::owner_git_logged(
                owner.as_ref(),
                Some(source_dir),
                Some(&mirror),
                &["pull", "--ff-only"],
                task,
            )
            .await?;
        }
        None => {
            task.log("no Repository URL: pulling from the checkout's own origin as its owner");
            app_git::owner_git_logged(owner.as_ref(), Some(source_dir), None, &["pull", "--ff-only"], task)
                .await?;
        }
    }
    if cfg.apps_uid != 0 {
        let _ = exec::run("chown", ["-R", "apps:apps", source_dir]).await;
    }

    // 2+3) Install and build replace the node_modules and build output the
    //      running app starts from. Save them first: a failed install or build
    //      puts them back, and so does a new version that will not stay up.
    let snapshot = ArtifactSnapshot::take(name, source_dir, task).await;
    let build_env = build_env_for(name).await;
    if let Err(e) = install_and_build(cfg, source_dir, &build_env, task).await {
        task.log(format!("install/build failed: {e}"));
        snapshot.restore(task).await;
        if slot_port.is_some() && was_active {
            // The build tools deleted files under the running process; restart
            // it on the restored artifacts so it is consistent again.
            match app_systemctl(&["restart"], unit).await {
                Ok(()) => task.log(format!("restarted {unit} on the previous build")),
                Err(re) => task.log(format!("could not restart {unit}: {re}")),
            }
        }
        return Err(AgentError::Other(anyhow::anyhow!(
            "update failed and was rolled back to the previous build: {e}"
        )));
    }

    // 4) Restart the systemd unit so the new code is picked up. An app with
    //    no routing slot stays down (see set_routing) - the build is still
    //    useful, it just waits for a domain.
    let Some(port) = slot_port else {
        snapshot.discard().await;
        task.log("app has no domain assigned - skipping restart");
        return Ok(());
    };
    task.log(format!("restarting {unit}"));
    app_systemctl(&["restart"], unit).await?;
    task.log(format!(
        "waiting for port {port}, then {APP_STABLE_SECS}s to confirm the new version stays up"
    ));
    match verify_app_up(unit, port, Duration::from_secs(45)).await {
        Ok(()) => {
            snapshot.discard().await;
            task.log("restarted; the new version is up");
            Ok(())
        }
        Err(reason) => {
            task.log(format!("=== {reason} ==="));
            log_journal_tail(unit, 60, task).await;
            if snapshot.names.is_empty() {
                return Err(AgentError::Other(anyhow::anyhow!(
                    "{reason}; there was no previous build to roll back to"
                )));
            }
            task.log("rolling back to the previous build");
            snapshot.restore(task).await;
            let _ = app_systemctl(&["restart"], unit).await;
            let rollback = match verify_app_up(unit, port, Duration::from_secs(45)).await {
                Ok(()) => "the previous build is running again".to_string(),
                Err(e) => format!("the previous build did not come back up either: {e}"),
            };
            Err(AgentError::Other(anyhow::anyhow!(
                "the new version did not stay up ({reason}); {rollback}. The checkout stays on the new commit - fix it and run Update again"
            )))
        }
    }
}

/// npm ci (preferred when package-lock.json exists) or npm install, then
/// `npm run build` when package.json defines one.
async fn install_and_build(
    cfg: &crate::config::Config,
    source_dir: &str,
    build_env: &[(String, String)],
    task: &crate::ipc::tasks::TaskHandle,
) -> AgentResult<()> {
    let src = PathBuf::from(source_dir);
    if src.join("package-lock.json").is_file() {
        task.log("installing deps (npm ci)");
        run_npm(
            cfg,
            &["--prefix", source_dir, "ci", "--no-audit", "--no-fund"],
            &[],
            task,
        )
        .await?;
    } else if src.join("package.json").is_file() {
        task.log("installing deps (npm install)");
        run_npm(
            cfg,
            &[
                "--prefix",
                source_dir,
                "install",
                "--no-audit",
                "--no-fund",
            ],
            &[],
            task,
        )
        .await?;
    } else {
        task.log("no package.json - skipping npm install");
    }
    if cfg.apps_uid != 0 {
        let _ = exec::run("chown", ["-R", "apps:apps", source_dir]).await;
    }

    let pkg_path = src.join("package.json");
    let has_build = match tokio::fs::read_to_string(&pkg_path).await {
        Ok(body) => serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|j| {
                j.get("scripts")
                    .and_then(|s| s.get("build"))
                    .and_then(|v| v.as_str())
                    .map(|_| ())
            })
            .is_some(),
        Err(_) => false,
    };
    if has_build {
        task.log("running npm run build");
        run_npm(cfg, &["--prefix", source_dir, "run", "build"], build_env, task).await?;
        if cfg.apps_uid != 0 {
            let _ = exec::run("chown", ["-R", "apps:apps", source_dir]).await;
        }
    } else {
        task.log("no `build` script in package.json - skipping build");
    }
    Ok(())
}

#[derive(Serialize)]
pub struct PatchEnvResp {
    pub ok: bool,
    /// False when the app has no routing slot (it stays down, see
    /// set_routing) or no unit yet: the values are saved for its next start.
    pub restarted: bool,
}

/// Upsert entries into the app's EnvironmentFile and restart it. Keys not in
/// the request keep their values. It used to rewrite the whole file from the
/// request, so patching one key (MCP apps.patch_env) deleted DATABASE_URL and
/// every other variable. Variables a build reads still need an Update.
pub async fn patch_env(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<PatchEnv>,
) -> AgentResult<Json<PatchEnvResp>> {
    validate_name(&name)?;
    if req.env.is_empty() {
        return Err(AgentError::Invalid("env must contain at least one entry".into()));
    }
    validate_env_keys(&req.env)?;
    let env_path = PathBuf::from(format!("/etc/vps-panel/apps/{name}.env"));
    let mut entries = parse_env_file(&tokio::fs::read_to_string(&env_path).await.unwrap_or_default());
    for kv in req.env {
        match entries.iter_mut().find(|e| e.key == kv.key) {
            Some(existing) => existing.value = kv.value,
            None => entries.push(kv),
        }
    }
    fs_atomic::ensure_dir(env_path.parent().unwrap(), 0o750).await?;
    fs_atomic::write(&env_path, render_env_file(&entries).as_bytes(), 0o640).await?;
    if state.config.apps_gid != 0 {
        let _ = fs_atomic::chown_blocking(&env_path, 0, state.config.apps_gid);
    }
    let unit = format!("{VPS_APP_PREFIX}{name}.service");
    let has_unit = PathBuf::from(format!("/etc/systemd/system/{unit}")).is_file();
    // An app with no slot must not be started on its stale port.
    let restarted = if has_unit && ensure_has_slot(&state, &name).await.is_ok() {
        app_systemctl(&["restart"], &unit).await?;
        true
    } else {
        false
    };
    Ok(Json(PatchEnvResp { ok: true, restarted }))
}

#[derive(Serialize)]
pub struct EnvResp {
    /// The EnvironmentFile's KEY=VALUE pairs in file order, values as stored
    /// (newlines escaped as `\n`). Empty when the file does not exist.
    pub env: Vec<EnvKv>,
}

/// The app's EnvironmentFile (`/etc/vps-panel/apps/<name>.env`). Holds
/// secrets: the panel reads it for admins only and no MCP tool calls it.
pub async fn get_env(Path(name): Path<String>) -> AgentResult<Json<EnvResp>> {
    validate_name(&name)?;
    let body = tokio::fs::read_to_string(format!("/etc/vps-panel/apps/{name}.env"))
        .await
        .unwrap_or_default();
    Ok(Json(EnvResp { env: parse_env_file(&body) }))
}

/// List every `.env*` file inside an app's source directory. These are the
/// files the app's own framework reads (e.g. Next.js `.env.local`), distinct
/// from the systemd EnvironmentFile at `/etc/vps-panel/apps/<name>.env`.
/// Works for panel-managed and external apps alike — source dir is resolved
/// from panel.db / the unit file, falling back to `/srv/apps/<name>`.
pub async fn list_env_files(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> AgentResult<Json<EnvFilesResp>> {
    validate_name(&name)?;
    let source_dir = resolve_source_dir(&state, &name).await?;
    let root = PathBuf::from(&source_dir);

    let mut files: Vec<EnvFileEntry> = Vec::new();
    let mut truncated = false;
    // DFS via an explicit stack so a deep tree can't blow the call stack.
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.clone(), 0)];
    'outer: while let Some((dir, depth)) = stack.pop() {
        let mut rd = match tokio::fs::read_dir(&dir).await {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            let Ok(ft) = entry.file_type().await else {
                continue;
            };
            let fname = entry.file_name();
            let fname = fname.to_string_lossy();
            if ft.is_dir() {
                if depth + 1 > ENV_SCAN_MAX_DEPTH
                    || ENV_SCAN_IGNORE_DIRS.contains(&fname.as_ref())
                {
                    continue;
                }
                stack.push((entry.path(), depth + 1));
            } else if ft.is_file() && fname.starts_with(".env") {
                if files.len() >= ENV_SCAN_MAX_RESULTS {
                    truncated = true;
                    break 'outer;
                }
                let abs = entry.path();
                let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
                let (content, too_large) = if size > ENV_FILE_MAX_INLINE {
                    (String::new(), true)
                } else {
                    match tokio::fs::read(&abs).await {
                        Ok(bytes) => (String::from_utf8_lossy(&bytes).into_owned(), false),
                        Err(_) => (String::new(), false),
                    }
                };
                let rel = abs
                    .strip_prefix(&root)
                    .unwrap_or(&abs)
                    .to_string_lossy()
                    .replace('\\', "/");
                files.push(EnvFileEntry {
                    path: rel,
                    abs_path: abs.to_string_lossy().into_owned(),
                    size,
                    content,
                    too_large,
                });
            }
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Json(EnvFilesResp {
        source_dir,
        files,
        truncated,
    }))
}

/// Create or overwrite a `.env*` file inside an app's source directory, then
/// (optionally) restart the app so the new values take effect. The path is
/// locked to a relative `.env*` path inside the source dir, so this can't be
/// abused to write arbitrary files on the box.
pub async fn save_env_file(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<SaveEnvFile>,
) -> AgentResult<Json<SaveEnvFileResp>> {
    validate_name(&name)?;
    if req.content.len() > ENV_FILE_MAX_WRITE {
        return Err(AgentError::Invalid(format!(
            "env file too large ({} bytes); max is {ENV_FILE_MAX_WRITE}",
            req.content.len()
        )));
    }
    let source_dir = resolve_source_dir(&state, &name).await?;
    let rel = validate_env_rel_path(&req.path)?;
    let target = PathBuf::from(&source_dir).join(&rel);

    // Preserve an existing file's mode; default 0640 for a new one.
    let mode = existing_file_mode(&target).await.unwrap_or(0o640);
    fs_atomic::write(&target, req.content.as_bytes(), mode).await?;

    // Chown to the source dir's owner so the app's own user (apps for panel
    // apps, root for external ones) can read the file it just created.
    if let Some((uid, gid)) = dir_owner(&PathBuf::from(&source_dir)).await {
        let _ = fs_atomic::chown_blocking(&target, uid, gid);
    }

    let restarted = if req.restart {
        restart_unit_for(&state, &name).await
    } else {
        None
    };
    Ok(Json(SaveEnvFileResp {
        ok: true,
        restarted,
    }))
}

/// Delete a `.env*` file from the app's source dir, then optionally restart.
/// `path` is validated the same way as save (relative, `.env*`, can't escape
/// the folder). NotFound if the file isn't there.
pub async fn delete_env_file(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<DeleteEnvFile>,
) -> AgentResult<Json<SaveEnvFileResp>> {
    validate_name(&name)?;
    let source_dir = resolve_source_dir(&state, &name).await?;
    let rel = validate_env_rel_path(&req.path)?;
    let target = PathBuf::from(&source_dir).join(&rel);
    match tokio::fs::metadata(&target).await {
        Ok(m) if m.is_file() => tokio::fs::remove_file(&target).await?,
        Ok(_) => {
            return Err(AgentError::Invalid(
                "target is not a regular file".into(),
            ))
        }
        Err(_) => return Err(AgentError::NotFound),
    }
    let restarted = if req.restart {
        restart_unit_for(&state, &name).await
    } else {
        None
    };
    Ok(Json(SaveEnvFileResp {
        ok: true,
        restarted,
    }))
}

/// Rewrite the `ExecStart=` line of a panel-managed app's systemd unit, then
/// daemon-reload + restart so the new entrypoint takes effect. Only the
/// ExecStart line is touched; every other directive (WorkingDirectory, User,
/// EnvironmentFile, sandboxing) is preserved verbatim.
pub async fn set_entrypoint(
    Path(name): Path<String>,
    Json(req): Json<SetEntrypoint>,
) -> AgentResult<Json<SetEntrypointResp>> {
    validate_name(&name)?;
    if docker_apps::is_docker_app(&name) {
        return Err(AgentError::Invalid(
            "a Docker app runs its image's own command - change the image instead".into(),
        ));
    }
    let exec = req.exec_start.trim();
    if exec.is_empty() {
        return Err(AgentError::Invalid("exec_start must not be empty".into()));
    }
    // Same guard the deploy path uses: a control char could inject extra unit
    // directives (e.g. a newline followed by `User=root`).
    if exec.chars().any(|c| c.is_control()) {
        return Err(AgentError::Invalid(
            "exec_start must be a single line with no control characters".into(),
        ));
    }

    let unit = format!("{VPS_APP_PREFIX}{name}.service");
    let unit_path = PathBuf::from(format!("/etc/systemd/system/{unit}"));
    let body = tokio::fs::read_to_string(&unit_path)
        .await
        .map_err(|_| AgentError::NotFound)?;

    let mut replaced = false;
    let mut lines: Vec<String> = Vec::new();
    for line in body.lines() {
        if line.starts_with("ExecStart=") {
            lines.push(format!("ExecStart={exec}"));
            replaced = true;
        } else {
            lines.push(line.to_string());
        }
    }
    if !replaced {
        return Err(AgentError::Invalid(
            "unit file has no ExecStart line to update".into(),
        ));
    }
    let mut new_body = lines.join("\n");
    new_body.push('\n');
    fs_atomic::write(&unit_path, new_body.as_bytes(), 0o644).await?;

    exec::run("systemctl", ["daemon-reload"]).await?;
    app_systemctl(&["restart"], &unit).await?;
    Ok(Json(SetEntrypointResp {
        ok: true,
        restarted: true,
    }))
}

#[derive(Deserialize)]
pub struct SetRouting {
    /// FQDN of the port-mode site to bind to, or null to take the app off its
    /// slot. Must be given together with `port`.
    pub subdomain: Option<String>,
    /// The site's upstream_port - what the app must listen on.
    pub port: Option<u16>,
}

#[derive(Serialize)]
pub struct SetRoutingResp {
    pub ok: bool,
    /// "assigned" | "unassigned"
    pub state: String,
    /// Assign only: the app came up on the new port within the wait window.
    pub bound: bool,
    /// journalctl tail when `bound` is false, so the panel can show why.
    pub log: Vec<String>,
}

/// Same window run_deploy gives a fresh app: covers compile-on-start and
/// first-run migrations.
const ROUTING_BIND_WAIT_SECS: u64 = 45;

/// Move a panel-managed app onto another routing slot (a port-mode site from
/// /domains) or take it off its slot. The site owns the vhost and the cert,
/// so the app side of a move is only: rewrite PORT, drop a self-owned app
/// vhost if the app still has one, restart. Unassign stops AND disables the
/// unit - an app with no slot must not come back on reboot and quietly serve
/// whatever site still proxies to its old port.
pub async fn set_routing(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<SetRouting>,
) -> AgentResult<Json<SetRoutingResp>> {
    validate_name(&name)?;
    let unit = format!("{VPS_APP_PREFIX}{name}.service");
    let unit_path = PathBuf::from(format!("/etc/systemd/system/{unit}"));
    // Panel-managed apps only: external units use a different name, run as
    // root and carry no PORT= line we could safely rewrite.
    let unit_body = tokio::fs::read_to_string(&unit_path)
        .await
        .map_err(|_| AgentError::NotFound)?;
    let (old_subdomain, old_port, _) = lookup_app(&state, &name).await.unwrap_or_default();

    let target = match (req.subdomain.as_deref(), req.port) {
        (None, None) => None,
        (Some(sub), Some(port)) => {
            let sub = sub.trim().to_lowercase();
            if !is_safe_subdomain(&sub) {
                return Err(AgentError::Invalid("invalid subdomain".into()));
            }
            if !(1024..65535).contains(&port) {
                return Err(AgentError::Invalid("port must be in 1024..65535".into()));
            }
            // Only port-mode sites are valid targets: the site vhost is what
            // routes the name, so there is never a second vhost to write here.
            let site_vhost = format!("/etc/nginx/sites-available/site-{sub}");
            if !PathBuf::from(&site_vhost).exists() {
                return Err(AgentError::Invalid(format!(
                    "no port-mode site for {sub} - create it on /domains first"
                )));
            }
            Some((sub, port))
        }
        _ => {
            return Err(AgentError::Invalid(
                "subdomain and port must be given together, or both null to unassign".into(),
            ))
        }
    };

    // Stop before anything else: releases the old port (so the in-use check
    // below can't trip over ourselves) and guarantees nothing serves a
    // half-rewritten config.
    let _ = exec::run("systemctl", ["stop", &unit]).await;

    if let Some((_, port)) = &target {
        if *port != old_port && port_in_use(*port).await {
            // Put the app back the way we found it before reporting.
            if old_port != 0 {
                let _ = app_systemctl(&["start"], &unit).await;
            }
            return Err(AgentError::Conflict(format!(
                "port {port} is already in use by something else"
            )));
        }
    }

    // A vhost the deploy wrote itself (app deployed before a site existed
    // for the name) leaves together with the app, exactly like delete().
    // `site-*` files are never touched - they are the free slot afterwards.
    let leaving_old = !old_subdomain.is_empty()
        && match &target {
            Some((sub, _)) => sub != &old_subdomain,
            None => true,
        };
    if leaving_old {
        remove_app_vhost(&name, &old_subdomain).await;
    }

    let Some((_, port)) = target else {
        exec::run("systemctl", ["disable", &unit]).await?;
        return Ok(Json(SetRoutingResp {
            ok: true,
            state: "unassigned".into(),
            bound: false,
            log: Vec::new(),
        }));
    };

    // A Docker app's port is the host side of its `-p` publish, so its unit
    // is rendered again; the container keeps listening on its own port and
    // PORT in its env file is the container's business.
    let docker = docker_apps::read_config(&name).await;
    if port != old_port {
        match &docker {
            Some(spec) => docker_apps::write_unit(&name, port, spec).await?,
            None => {
                write_unit_port(&unit_path, &unit_body, port).await?;
                rewrite_env_file_port(&state.config, &name, port).await?;
            }
        }
        exec::run("systemctl", ["daemon-reload"]).await?;
    }
    app_systemctl(&["enable", "--now"], &unit).await?;

    let up = match docker {
        Some(_) => docker_apps::verify_up(&unit, port).await,
        None => verify_app_up(&unit, port, Duration::from_secs(ROUTING_BIND_WAIT_SECS)).await,
    };
    let bound = up.is_ok();
    let mut log = Vec::new();
    if let Err(reason) = up {
        // The binding itself is what the operator asked for and stays; only
        // the app is misbehaving (typically a start script with its own
        // hard-coded -p). Hand back the journal and end the crash loop:
        // stopped AND disabled, so it does not return on reboot either.
        log.push(reason);
        if let Ok(out) = exec::run(
            "journalctl",
            ["-u", &unit, "-n", "40", "--no-pager", "--output=short-iso"],
        )
        .await
        {
            log.extend(out.stdout.lines().map(str::to_string));
        }
        let _ = exec::run("systemctl", ["disable", "--now", &unit]).await;
    }

    Ok(Json(SetRoutingResp {
        ok: true,
        state: "assigned".into(),
        bound,
        log,
    }))
}

/// A panel row with port=0 means the app was taken off its routing slot on
/// purpose (set_routing). Starting it would bind the stale port and serve
/// whatever site still points there, so every start path checks this.
async fn ensure_has_slot(state: &AppState, name: &str) -> AgentResult<()> {
    if let Some((_, 0, _)) = lookup_app(state, name).await {
        return Err(AgentError::Conflict(format!(
            "{name} has no domain assigned - pick one on /apps/{name} first"
        )));
    }
    Ok(())
}

/// Remove `/etc/nginx/sites-available/<subdomain>` (+ symlink) only when its
/// first line is this app's own marker, then reload nginx.
async fn remove_app_vhost(name: &str, subdomain: &str) {
    let avail = PathBuf::from(format!("/etc/nginx/sites-available/{subdomain}"));
    let Ok(body) = tokio::fs::read_to_string(&avail).await else {
        return;
    };
    let marker = format!("# vps-app:{name}");
    if body.lines().next().map(str::trim) != Some(marker.as_str()) {
        return;
    }
    let _ = fs_atomic::remove_if_exists(&PathBuf::from(format!(
        "/etc/nginx/sites-enabled/{subdomain}"
    )))
    .await;
    let _ = fs_atomic::remove_if_exists(&avail).await;
    let _ = crate::handlers::redirects::remove_domain_files(subdomain).await;
    if nginx_handler::test_config().await.is_ok() {
        let _ = exec::run("systemctl", ["reload", "nginx"]).await;
    }
}

/// Replace the unit's `Environment=PORT=` line, or insert one after
/// `EnvironmentFile=` if a hand-edited unit lost it.
async fn write_unit_port(unit_path: &std::path::Path, body: &str, port: u16) -> AgentResult<()> {
    let mut lines: Vec<String> = Vec::new();
    let mut replaced = false;
    for line in body.lines() {
        if line.starts_with("Environment=PORT=") {
            lines.push(format!("Environment=PORT={port}"));
            replaced = true;
        } else {
            lines.push(line.to_string());
        }
    }
    if !replaced {
        let at = lines
            .iter()
            .position(|l| l.starts_with("EnvironmentFile="))
            .map_or(lines.len(), |i| i + 1);
        lines.insert(at, format!("Environment=PORT={port}"));
    }
    let mut out = lines.join("\n");
    out.push('\n');
    fs_atomic::write(unit_path, out.as_bytes(), 0o644).await
}

/// systemd lets `EnvironmentFile=` override `Environment=`, so a PORT= line
/// in /etc/vps-panel/apps/<name>.env would silently win over the unit.
/// Rewrite it when present; when absent the unit's value already rules.
async fn rewrite_env_file_port(
    cfg: &crate::config::Config,
    name: &str,
    port: u16,
) -> AgentResult<()> {
    let env_path = PathBuf::from(format!("/etc/vps-panel/apps/{name}.env"));
    let Ok(body) = tokio::fs::read_to_string(&env_path).await else {
        return Ok(());
    };
    if !body.lines().any(|l| l.starts_with("PORT=")) {
        return Ok(());
    }
    let mut out = body
        .lines()
        .map(|l| {
            if l.starts_with("PORT=") {
                format!("PORT={port}")
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    out.push('\n');
    fs_atomic::write(&env_path, out.as_bytes(), 0o640).await?;
    // Atomic write leaves the file root:root; the unit's user must read it.
    if cfg.apps_uid != 0 {
        let _ = fs_atomic::chown_blocking(&env_path, 0, cfg.apps_gid);
    }
    Ok(())
}

/// Resolve an app's source directory: panel.db / unit-file lookup first, then
/// the `/srv/apps/<name>` convention. Errors with NotFound if neither exists
/// on disk.
async fn resolve_source_dir(state: &AppState, name: &str) -> AgentResult<String> {
    if let Some((_, _, dir)) = lookup_app(state, name).await {
        if !dir.is_empty() && PathBuf::from(&dir).is_dir() {
            return Ok(dir);
        }
    }
    let fallback = format!("{APPS_BASE_DIR}/{name}");
    if PathBuf::from(&fallback).is_dir() {
        return Ok(fallback);
    }
    Err(AgentError::NotFound)
}

/// Validate a relative `.env*` path supplied by the operator. Enforces:
/// relative (no leading `/`), no `..`/`.` segments, no backslashes, safe
/// characters, at most 4 segments, and a file name starting with `.env`.
fn validate_env_rel_path(p: &str) -> AgentResult<PathBuf> {
    let p = p.trim();
    if p.is_empty() || p.len() > 256 {
        return Err(AgentError::Invalid(
            "env file path must be 1..256 characters".into(),
        ));
    }
    if p.starts_with('/') || p.contains('\\') {
        return Err(AgentError::Invalid(
            "env file path must be relative (no leading '/', no backslashes)".into(),
        ));
    }
    let segments: Vec<&str> = p.split('/').collect();
    if segments.len() > 4 {
        return Err(AgentError::Invalid(
            "env file path is too deep (max 4 segments)".into(),
        ));
    }
    for (i, seg) in segments.iter().enumerate() {
        if seg.is_empty() || *seg == "." || *seg == ".." {
            return Err(AgentError::Invalid(format!(
                "invalid path segment {seg:?}"
            )));
        }
        if !seg
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
        {
            return Err(AgentError::Invalid(format!(
                "path segment {seg:?} has unsafe characters"
            )));
        }
        if i == segments.len() - 1 && !seg.starts_with(".env") {
            return Err(AgentError::Invalid(format!(
                "env file name must start with '.env' (got {seg:?})"
            )));
        }
    }
    Ok(PathBuf::from(p))
}

/// Restart the systemd unit backing an app. Prefers the panel-managed
/// `vps-app-<name>.service`; falls back to the external `<name>.service`
/// convention. Returns the unit it restarted, or None when no unit exists
/// (e.g. the operator is editing env files before the first deploy).
async fn restart_unit_for(state: &AppState, name: &str) -> Option<String> {
    let panel_unit = format!("{VPS_APP_PREFIX}{name}.service");
    if PathBuf::from(format!("/etc/systemd/system/{panel_unit}")).is_file() {
        if ensure_has_slot(state, name).await.is_err() {
            return None;
        }
        return app_systemctl(&["restart"], &panel_unit)
            .await
            .ok()
            .map(|_| panel_unit);
    }
    // External app: `<name>.service`. external_service_status returns empty
    // strings when the unit doesn't exist (LoadState=not-found).
    let (active, _) = external_service_status(name).await;
    if active.is_empty() {
        return None;
    }
    let ext_unit = format!("{name}.service");
    exec::run("systemctl", ["restart", &ext_unit])
        .await
        .ok()
        .map(|_| ext_unit)
}

#[cfg(unix)]
async fn existing_file_mode(path: &std::path::Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::metadata(path)
        .await
        .ok()
        .map(|m| m.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
async fn existing_file_mode(_path: &std::path::Path) -> Option<u32> {
    None
}

#[cfg(unix)]
async fn dir_owner(path: &std::path::Path) -> Option<(u32, u32)> {
    use std::os::unix::fs::MetadataExt;
    tokio::fs::metadata(path).await.ok().map(|m| (m.uid(), m.gid()))
}

#[cfg(not(unix))]
async fn dir_owner(_path: &std::path::Path) -> Option<(u32, u32)> {
    None
}

fn is_valid_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn validate_env_keys(env: &[EnvKv]) -> AgentResult<()> {
    for kv in env {
        if !is_valid_env_key(&kv.key) {
            return Err(AgentError::Invalid(format!(
                "invalid env key {:?}: must match [A-Za-z_][A-Za-z0-9_]*",
                kv.key
            )));
        }
    }
    Ok(())
}

/// KEY=VALUE pairs of an EnvironmentFile written by `render_env_file`, in
/// file order. Values come back as stored (newlines stay escaped), so
/// rendering them again is lossless. Comments, blank and malformed lines are
/// skipped.
pub(crate) fn parse_env_file(body: &str) -> Vec<EnvKv> {
    body.lines()
        .filter_map(|line| {
            let line = line.trim_end_matches('\r');
            if line.trim_start().starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            is_valid_env_key(key).then(|| EnvKv {
                key: key.to_string(),
                value: value.to_string(),
            })
        })
        .collect()
}

/// The app's EnvironmentFile as (key, value) pairs for a build, so variables
/// read at build time (`NEXT_PUBLIC_*`, anything next.config reads) reach it.
/// Empty when the file does not exist yet.
async fn build_env_for(name: &str) -> Vec<(String, String)> {
    tokio::fs::read_to_string(format!("/etc/vps-panel/apps/{name}.env"))
        .await
        .map(|body| {
            parse_env_file(&body)
                .into_iter()
                .map(|kv| (kv.key, kv.value))
                .collect()
        })
        .unwrap_or_default()
}

/// ExecStart of an existing unit file, for a re-deploy that did not send one.
async fn existing_exec_start(unit_path: &std::path::Path) -> Option<String> {
    let body = tokio::fs::read_to_string(unit_path).await.ok()?;
    body.lines()
        .find_map(|l| l.strip_prefix("ExecStart="))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn render_env_file(env: &[EnvKv]) -> String {
    let mut out = String::new();
    for kv in env {
        let value = kv.value.replace('\n', "\\n").replace('\r', "");
        out.push_str(&format!("{}={}\n", kv.key, value));
    }
    out
}

/// The port answers, and not because this app's own unit holds it (a
/// re-deploy of a running app is not a conflict).
pub(crate) async fn port_held_by_other(port: u16, own_unit: &str) -> bool {
    port_in_use(port).await && unit_prop(own_unit, "ActiveState").await != "active"
}

async fn port_in_use(port: u16) -> bool {
    use tokio::net::TcpListener;
    TcpListener::bind(("127.0.0.1", port)).await.is_err()
}

async fn dns_resolves_locally(host: &str) -> AgentResult<()> {
    use tokio::net::lookup_host;
    let addrs = lookup_host((host, 80))
        .await
        .map_err(AgentError::Io)?
        .collect::<Vec<_>>();
    if addrs.is_empty() {
        return Err(AgentError::Invalid("no DNS records resolved".into()));
    }
    Ok(())
}

/// The Repository URL saved on the app page, if any.
fn stored_repo_url(state: &AppState, name: &str) -> Option<String> {
    state
        .panel_db
        .with(|conn| {
            conn.query_row(
                "SELECT repo_url FROM apps WHERE name = ?1",
                rusqlite::params![name],
                |r| r.get::<_, Option<String>>(0),
            )
        })
        .ok()
        .flatten()
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
}

/// A panel checkout under /srv/apps belongs to the apps user; the deploy
/// chowns it after every pull. If the folder and its .git disagree (a pull
/// by an older version, a manual edit), git refuses to run in it as either
/// user ("dubious ownership"), so hand it back to apps first. Only when they
/// differ: a recursive chown of node_modules on every update check is slow.
async fn normalize_checkout_owner(cfg: &crate::config::Config, dir: &str) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if cfg.apps_uid == 0 || !dir.starts_with(&format!("{APPS_BASE_DIR}/")) {
            return;
        }
        let owned = |p: std::path::PathBuf| {
            std::fs::symlink_metadata(p).map(|m| m.uid() == cfg.apps_uid).unwrap_or(true)
        };
        let path = std::path::Path::new(dir);
        if owned(path.to_path_buf()) && owned(path.join(".git")) {
            return;
        }
        let _ = exec::run("chown", ["-R", "apps:apps", dir]).await;
    }
    #[cfg(not(unix))]
    {
        let _ = (cfg, dir);
    }
}

/// Where the app's code comes from: the Repository URL saved on the app page,
/// else what an older checkout recorded (legacy_fetch_url).
async fn fetch_url(state: &AppState, name: &str, owner: Option<&app_git::Owner>, dir: &str) -> Option<String> {
    match stored_repo_url(state, name) {
        Some(u) => Some(u),
        None => legacy_fetch_url(name, owner, dir).await,
    }
}

/// For apps with no Repository URL saved: the URL the mirror was made from,
/// or the checkout's own origin if it is still a remote URL. Either way it
/// must pass is_safe_git_url, since root fetches it.
async fn legacy_fetch_url(name: &str, owner: Option<&app_git::Owner>, dir: &str) -> Option<String> {
    if let Some(u) = app_git::mirror_url(name).await {
        return Some(u);
    }
    let origin = app_git::owner_git(owner, Some(dir), None, &["remote", "get-url", "origin"])
        .await
        .ok()?
        .stdout
        .trim()
        .to_string();
    (is_safe_git_url(&origin) && misused_alias_hint(&origin).is_none()).then_some(origin)
}

pub(crate) async fn lookup_app(state: &AppState, name: &str) -> Option<(String, u16, String)> {
    let from_db: Option<(String, u16, String)> = state
        .panel_db
        .with(|conn| {
            conn.query_row(
                "SELECT subdomain, port, source_dir FROM apps WHERE name = ?1",
                rusqlite::params![name],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, u16>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
        })
        .ok()
        .flatten();
    if from_db.is_some() {
        return from_db;
    }

    // Fallback: scan disk if the panel.db row is missing (manual cleanup, panel out of sync).
    let unit_path = std::path::PathBuf::from(format!(
        "/etc/systemd/system/{VPS_APP_PREFIX}{name}.service"
    ));
    let body = tokio::fs::read_to_string(&unit_path).await.ok()?;
    let mut port = 0u16;
    let mut workdir = String::new();
    for l in body.lines() {
        if let Some(v) = l.strip_prefix("Environment=PORT=") {
            port = v.parse().unwrap_or(0);
        } else if let Some(v) = l.strip_prefix("WorkingDirectory=") {
            workdir = v.to_string();
        }
    }
    let subdomain = scan_marker(name).await.unwrap_or_default();
    Some((subdomain, port, workdir))
}

async fn scan_marker(name: &str) -> Option<String> {
    let mut entries = tokio::fs::read_dir("/etc/nginx/sites-available").await.ok()?;
    let needle = format!("# vps-app:{name}\n");
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if let Ok(body) = tokio::fs::read_to_string(&path).await {
            if body.contains(&needle) {
                return path.file_name().map(|s| s.to_string_lossy().into_owned());
            }
        }
    }
    None
}

fn field(s: &str, key: &str) -> Option<String> {
    s.lines()
        .find_map(|l| l.strip_prefix(key))
        .map(|v| v.trim().to_string())
}
