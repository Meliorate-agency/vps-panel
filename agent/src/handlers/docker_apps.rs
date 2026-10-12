//! Docker image apps (runtime "docker"). One app is one container, run by the
//! app's own systemd unit `vps-app-<name>.service`, so
//! Start/Stop/Restart, the Apps list, the Logs page, the crash-loop limit and
//! the "service failed" alert work exactly as they do for Node apps.
//!
//! What an app owns:
//! - `/etc/vps-panel/apps/<name>.docker.json`: its settings (DockerSpec), the
//!   source of truth. Its existence is what makes an app a Docker app.
//! - `/etc/vps-panel/apps/<name>.env`: the container's environment (--env-file).
//! - `vps-app-<name>.service`: rendered from the two plus the slot port.
//! - image tags `vps-app-<name>:current` (what the unit runs) and `:previous`
//!   (what Update rolls back to), container `vps-app-<name>`.
//! - named volumes `vps-app-<name>-<path>` (label vps-panel.app=<name>) for the
//!   folders kept across restarts and updates. Backups include them
//!   (backups.rs: /var/lib/docker/volumes).
//!
//! The container publishes only on 127.0.0.1:<slot port> and nginx routes the
//! domain to it. Docker's publishing skips ufw, so a 0.0.0.0 publish would be
//! public whatever the firewall says: there is no option for it. There are no
//! free-form docker flags either, so privileged mode, the host network, the
//! Docker socket and host paths cannot be asked for.

use crate::error::{AgentError, AgentResult};
use crate::exec;
use crate::fs_atomic;
use crate::handlers::apps::{self, CreateApp, Readiness, VPS_APP_PREFIX};
use crate::handlers::quick_install::{DOCKER_LOG_MAX_FILE, DOCKER_LOG_MAX_SIZE};
use crate::ipc::tasks::TaskHandle;
use crate::validate::validate_name;
use axum::{
    extract::{Path, Query},
    Json,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::time::Duration;

const APPS_ETC_DIR: &str = "/etc/vps-panel/apps";
/// Both on the agent's PATH, where its own `docker` calls find the binary.
const DOCKER_CANDIDATES: &[&str] = &["/usr/bin/docker", "/usr/local/bin/docker"];
const MAX_VOLUMES: usize = 10;
/// Threads count as processes: generous enough for a JVM, still a ceiling
/// for a fork bomb.
const PIDS_LIMIT: u32 = 4096;
/// First starts can run migrations or warm up a JVM.
const START_WAIT_SECS: u64 = 90;
/// `docker manifest inspect` against the registry.
const REGISTRY_TIMEOUT_SECS: u64 = 10;

fn default_memory_mb() -> u32 {
    1024
}

fn default_cpus() -> f64 {
    1.0
}

/// A Docker app's settings, saved as `<name>.docker.json`.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
pub struct DockerSpec {
    /// e.g. `louislam/uptime-kuma:1`, `ghcr.io/owner/app:1.4`.
    pub image: String,
    /// The port the app listens on inside the container.
    pub container_port: u16,
    /// Folders inside the container kept in named volumes.
    #[serde(default)]
    pub volumes: Vec<String>,
    /// 0 = no limit.
    #[serde(default = "default_memory_mb")]
    pub memory_mb: u32,
    /// 0 = no limit.
    #[serde(default = "default_cpus")]
    pub cpus: f64,
}

pub fn config_path(name: &str) -> PathBuf {
    PathBuf::from(format!("{APPS_ETC_DIR}/{name}.docker.json"))
}

pub fn is_docker_app(name: &str) -> bool {
    config_path(name).is_file()
}

pub async fn read_config(name: &str) -> Option<DockerSpec> {
    let body = tokio::fs::read_to_string(config_path(name)).await.ok()?;
    serde_json::from_str(&body).ok()
}

fn container_name(name: &str) -> String {
    format!("{VPS_APP_PREFIX}{name}")
}

fn current_tag(name: &str) -> String {
    format!("{VPS_APP_PREFIX}{name}:current")
}

fn previous_tag(name: &str) -> String {
    format!("{VPS_APP_PREFIX}{name}:previous")
}

fn unit_name(name: &str) -> String {
    format!("{VPS_APP_PREFIX}{name}.service")
}

/// `/app/data` -> `app-data`.
fn volume_slug(path: &str) -> String {
    let mut out = String::new();
    for ch in path.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

pub fn volume_name(app: &str, path: &str) -> String {
    format!("{VPS_APP_PREFIX}{app}-{}", volume_slug(path))
}

/// Trimmed image, folders without a trailing slash, blank and repeated
/// folders dropped.
pub fn normalize_spec(mut spec: DockerSpec) -> DockerSpec {
    spec.image = spec.image.trim().to_string();
    let mut volumes: Vec<String> = Vec::new();
    for v in &spec.volumes {
        let v = v.trim();
        let v = if v.len() > 1 { v.trim_end_matches('/') } else { v };
        if !v.is_empty() && !volumes.iter().any(|x| x == v) {
            volumes.push(v.to_string());
        }
    }
    spec.volumes = volumes;
    spec
}

pub fn validate_spec(spec: &DockerSpec) -> AgentResult<()> {
    validate_image(&spec.image)?;
    if spec.container_port == 0 {
        return Err(AgentError::Invalid("container port must be 1-65535".into()));
    }
    if spec.memory_mb != 0 && !(64..=1_048_576).contains(&spec.memory_mb) {
        return Err(AgentError::Invalid(
            "memory limit must be 0 (none) or 64-1048576 MB".into(),
        ));
    }
    if !spec.cpus.is_finite() || spec.cpus < 0.0 || (spec.cpus > 0.0 && spec.cpus < 0.1) {
        return Err(AgentError::Invalid("CPU limit must be 0 (none) or at least 0.1".into()));
    }
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f64;
    if spec.cpus > cores {
        return Err(AgentError::Invalid(format!(
            "CPU limit is {}, but this server has {cores} CPU(s)",
            spec.cpus
        )));
    }
    if spec.volumes.len() > MAX_VOLUMES {
        return Err(AgentError::Invalid(format!("at most {MAX_VOLUMES} folders to keep")));
    }
    let mut slugs = std::collections::HashSet::new();
    for v in &spec.volumes {
        validate_volume_path(v)?;
        if !slugs.insert(volume_slug(v)) {
            return Err(AgentError::Invalid(format!(
                "folder {v:?} maps to the same volume name as another folder in the list"
            )));
        }
    }
    Ok(())
}

fn validate_volume_path(p: &str) -> AgentResult<()> {
    let bad = |why: &str| AgentError::Invalid(format!("folder {p:?} {why}"));
    if !p.starts_with('/') || p == "/" {
        return Err(bad("must be an absolute folder inside the container, like /app/data"));
    }
    if p.len() > 200 {
        return Err(bad("is longer than 200 characters"));
    }
    if !p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-')) {
        return Err(bad("may only use letters, digits and / . _ -"));
    }
    if p[1..].split('/').any(|s| s.is_empty() || s == "." || s == "..") {
        return Err(bad("must not contain empty, . or .. segments"));
    }
    if volume_slug(p).is_empty() {
        return Err(bad("needs at least one letter or digit"));
    }
    Ok(())
}

/// A Docker image reference: `[host[:port]/]path[:tag][@sha256:<hex>]`, the
/// path made of Docker's lowercase components. Nothing that could start
/// with `-` (it is passed as an argument) or carry systemd's `%`/`$`.
pub fn validate_image(image: &str) -> AgentResult<()> {
    let bad = || {
        AgentError::Invalid(format!(
            "{image:?} is not an image name like nginx:1.27, louislam/uptime-kuma:1 or ghcr.io/owner/app:tag"
        ))
    };
    if image.is_empty() || image.len() > 255 {
        return Err(bad());
    }
    let (rest, digest) = match image.split_once('@') {
        Some((r, d)) => (r, Some(d)),
        None => (image, None),
    };
    if let Some(d) = digest {
        let hex = d.strip_prefix("sha256:").ok_or_else(bad)?;
        if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)) {
            return Err(bad());
        }
    }
    // A ':' after the last '/' starts the tag; one before it is a registry port.
    let name_start = rest.rfind('/').map_or(0, |i| i + 1);
    let (repo, tag) = match rest[name_start..].find(':') {
        Some(i) => (&rest[..name_start + i], Some(&rest[name_start + i + 1..])),
        None => (rest, None),
    };
    if let Some(t) = tag {
        let first_ok = t.chars().next().is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        if !first_ok || t.len() > 128 || !t.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')) {
            return Err(bad());
        }
    }
    let parts: Vec<&str> = repo.split('/').collect();
    let has_host = parts.len() > 1
        && (parts[0].contains('.') || parts[0].contains(':') || parts[0] == "localhost");
    let path = if has_host {
        let (host, port) = match parts[0].split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (parts[0], None),
        };
        let host_ok = !host.is_empty()
            && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
            && host.starts_with(|c: char| c.is_ascii_alphanumeric());
        let port_ok = match port {
            None => true,
            Some(p) => !p.is_empty() && p.len() <= 5 && p.chars().all(|c| c.is_ascii_digit()),
        };
        if !host_ok || !port_ok {
            return Err(bad());
        }
        &parts[1..]
    } else {
        &parts[..]
    };
    if path.is_empty() || !path.iter().all(|c| is_path_component(c)) {
        return Err(bad());
    }
    // The app's own tags live under vps-app-*: running one would run another
    // app's image.
    if !has_host && path.len() == 1 && path[0].starts_with(VPS_APP_PREFIX) {
        return Err(AgentError::Invalid(format!(
            "{image:?} is the panel's own tag for an app - use the image it was deployed from"
        )));
    }
    Ok(())
}

/// Lowercase letters and digits, separated by one `.`, one or two `_`, or
/// any number of `-`.
fn is_path_component(c: &str) -> bool {
    let b = c.as_bytes();
    let alnum = |x: u8| x.is_ascii_lowercase() || x.is_ascii_digit();
    if b.is_empty() || !alnum(b[0]) || !alnum(b[b.len() - 1]) {
        return false;
    }
    let mut i = 0;
    while i < b.len() {
        if alnum(b[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && !alnum(b[i]) {
            i += 1;
        }
        let sep = &c[start..i];
        if !(sep == "." || sep == "_" || sep == "__" || sep.bytes().all(|x| x == b'-')) {
            return false;
        }
    }
    true
}

/// 1.0 -> "1", 0.5 -> "0.5".
fn format_cpus(cpus: f64) -> String {
    let s = format!("{cpus:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// The systemd unit for a Docker app. Pure, so it is unit-tested.
pub fn render_unit(name: &str, port: u16, spec: &DockerSpec, docker: &str) -> String {
    let container = container_name(name);
    let mut run: Vec<String> = vec![
        format!("{docker} run --detach --name {container} --pull=never"),
        format!("--label vps-panel.app={name}"),
        format!("--env-file {APPS_ETC_DIR}/{name}.env"),
        format!("-p 127.0.0.1:{port}:{}", spec.container_port),
        "--add-host=host.docker.internal:host-gateway".into(),
        "--security-opt no-new-privileges".into(),
        format!("--pids-limit {PIDS_LIMIT}"),
    ];
    if spec.memory_mb > 0 {
        run.push(format!("--memory {}m", spec.memory_mb));
    }
    if spec.cpus > 0.0 {
        run.push(format!("--cpus {}", format_cpus(spec.cpus)));
    }
    run.push(format!("--log-opt {DOCKER_LOG_MAX_SIZE} --log-opt {DOCKER_LOG_MAX_FILE}"));
    for v in &spec.volumes {
        run.push(format!("-v {}:{v}", volume_name(name, v)));
    }
    run.push(current_tag(name));
    let docker_run = run.join(" \\\n  ");
    format!(
        "# Written by vps-panel-agent (docker_apps.rs) from {APPS_ETC_DIR}/{name}.docker.json.
# Change the app in the panel: a deploy, update or domain change rewrites it.
[Unit]
Description=VPS Panel app: {name} (Docker)
After=network-online.target docker.service
Wants=network-online.target
Requires=docker.service

[Service]
Type=simple
# The routing slot. The container keeps its own port; docker publishes it
# on 127.0.0.1 only and nginx routes the domain here.
Environment=PORT={port}
ExecStartPre=-{docker} rm -f {container}
ExecStartPre={docker_run}
# The main process follows the container's output into the journal and ends
# with it. Not `docker run` in the foreground: the unit would take the exit
# code an app gives SIGTERM (2, 143...) and every Stop would end \"failed\".
ExecStart={docker} logs --follow {container}
ExecStop=-{docker} stop -t 20 {container}
ExecStopPost=-{docker} rm -f {container}
# A container that exits for any reason is started again; Stop is not.
# Crash loops end after 5 starts in 5 minutes: StartLimit* come from
# /etc/systemd/system/vps-app-.service.d/10-vps-panel-start-limit.conf.
Restart=always
RestartSec=2s
TimeoutStopSec=45

[Install]
WantedBy=multi-user.target
"
    )
}

/// The docker binary the unit runs (an absolute path).
fn docker_bin() -> AgentResult<&'static str> {
    DOCKER_CANDIDATES
        .iter()
        .copied()
        .find(|p| std::path::Path::new(p).is_file())
        .ok_or_else(|| {
            AgentError::Invalid(
                "Docker is not installed - install it from the Docker card under Quick install first".into(),
            )
        })
}

pub async fn write_unit(name: &str, port: u16, spec: &DockerSpec) -> AgentResult<()> {
    let body = render_unit(name, port, spec, docker_bin()?);
    let path = PathBuf::from(format!("/etc/systemd/system/{}", unit_name(name)));
    fs_atomic::write(&path, body.as_bytes(), 0o644).await
}

async fn write_config(name: &str, spec: &DockerSpec) -> AgentResult<()> {
    let body = serde_json::to_string_pretty(spec)
        .map_err(|e| AgentError::Other(anyhow::anyhow!("docker settings: {e}")))?;
    fs_atomic::ensure_dir(std::path::Path::new(APPS_ETC_DIR), 0o750).await?;
    fs_atomic::write(&config_path(name), format!("{body}\n").as_bytes(), 0o640).await
}

/// `sha256:...` of a local image (a tag or a reference), or None.
async fn image_id(reference: &str) -> Option<String> {
    exec::run("docker", ["image", "inspect", "--format", "{{.Id}}", reference])
        .await
        .ok()
        .map(|o| o.stdout.trim().to_string())
        .filter(|id| id.starts_with("sha256:"))
}

async fn tag(source: &str, target: &str, task: &TaskHandle) -> AgentResult<()> {
    task.log(format!("$ docker tag {source} {target}"));
    exec::run("docker", ["tag", source, target]).await.map(|_| ())
}

async fn pull(image: &str, task: &TaskHandle) -> AgentResult<()> {
    task.log(format!("$ docker pull {image}"));
    let code = exec::run_streaming("docker", ["pull", image], |l| task.log(l.to_string())).await?;
    if code != 0 {
        return Err(AgentError::Invalid(format!(
            "docker pull {image} failed (exit {code}): check the image name and tag. \
             Private registries are not supported yet."
        )));
    }
    Ok(())
}

/// The ports and folders an image declares, from `docker image inspect`.
#[derive(Serialize, Debug, PartialEq)]
pub struct ImageInfo {
    pub id: String,
    /// TCP ports from EXPOSE, sorted.
    pub exposed_ports: Vec<u16>,
    /// Folders from VOLUME, sorted: data the image expects to keep.
    pub volumes: Vec<String>,
    /// The user the image runs as ("" = root).
    pub user: String,
}

fn parse_image_info(json: &str) -> Option<ImageInfo> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let v = if let Some(first) = v.as_array().and_then(|a| a.first()) { first } else { &v };
    let config = v.get("Config");
    let keys = |field: &str| -> Vec<String> {
        config
            .and_then(|c| c.get(field))
            .and_then(|m| m.as_object())
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    };
    let mut exposed_ports: Vec<u16> = keys("ExposedPorts")
        .iter()
        .filter_map(|k| k.strip_suffix("/tcp").or(if k.contains('/') { None } else { Some(k.as_str()) }))
        .filter_map(|p| p.parse().ok())
        .collect();
    exposed_ports.sort_unstable();
    let mut volumes = keys("Volumes");
    volumes.sort();
    Some(ImageInfo {
        id: v.get("Id")?.as_str()?.to_string(),
        exposed_ports,
        volumes,
        user: config
            .and_then(|c| c.get("User"))
            .and_then(|u| u.as_str())
            .unwrap_or("")
            .to_string(),
    })
}

async fn inspect_image(image: &str) -> Option<ImageInfo> {
    let out = exec::run("docker", ["image", "inspect", image]).await.ok()?;
    parse_image_info(&out.stdout)
}

async fn pull_and_describe(image: &str, task: &TaskHandle) -> AgentResult<()> {
    pull(image, task).await?;
    if let Some(info) = inspect_image(image).await {
        let ports: Vec<String> = info.exposed_ports.iter().map(|p| p.to_string()).collect();
        task.log(format!(
            "image {}: listens on {}; keeps {}",
            short_id(&info.id),
            if ports.is_empty() { "no declared port".to_string() } else { ports.join(", ") },
            if info.volumes.is_empty() { "no declared folders".to_string() } else { info.volumes.join(", ") },
        ));
    }
    Ok(())
}

fn short_id(id: &str) -> &str {
    let hex = id.strip_prefix("sha256:").unwrap_or(id);
    &hex[..hex.len().min(12)]
}

pub async fn verify_up(unit: &str, port: u16) -> Result<(), String> {
    apps::verify_unit_up(unit, port, Duration::from_secs(START_WAIT_SECS), Readiness::HttpAnswer).await
}

/// POST /apps for runtime "docker". Steps: pull, deploy, all (pull + deploy).
pub async fn run_deploy(
    cfg: &crate::config::Config,
    acme_email: &str,
    req: &CreateApp,
    task: &TaskHandle,
) -> AgentResult<()> {
    let spec = match &req.docker {
        Some(spec) => spec.clone(),
        None => read_config(&req.name).await.ok_or_else(|| {
            AgentError::Invalid(format!("{} has no saved Docker settings - give the image and port", req.name))
        })?,
    };
    validate_spec(&spec)?;
    docker_bin()?;
    match req.step.as_deref().unwrap_or("all") {
        "pull" => pull_and_describe(&spec.image, task).await,
        "deploy" => deploy(cfg, acme_email, req, &spec, task).await,
        "all" => {
            pull_and_describe(&spec.image, task).await?;
            deploy(cfg, acme_email, req, &spec, task).await
        }
        "install" | "build" => Err(AgentError::Invalid(
            "a Docker app has no install or build step: Pull, then Deploy".into(),
        )),
        other => Err(AgentError::Invalid(format!(
            "unknown step '{other}' - expected pull / deploy / all"
        ))),
    }
}

async fn deploy(
    cfg: &crate::config::Config,
    acme_email: &str,
    req: &CreateApp,
    spec: &DockerSpec,
    task: &TaskHandle,
) -> AgentResult<()> {
    let name = req.name.as_str();
    let unit = unit_name(name);
    let unit_path = PathBuf::from(format!("/etc/systemd/system/{unit}"));
    let env_path = PathBuf::from(format!("{APPS_ETC_DIR}/{name}.env"));

    if apps::port_held_by_other(req.port, &unit).await {
        return Err(AgentError::Conflict(format!(
            "port {} is in use by something other than {unit}",
            req.port
        )));
    }
    if image_id(&spec.image).await.is_none() {
        task.log(format!("{} is not on this server yet", spec.image));
        pull(&spec.image, task).await?;
    }
    let new_id = image_id(&spec.image)
        .await
        .ok_or_else(|| AgentError::Invalid(format!("{} is not on this server after the pull", spec.image)))?;

    apps::write_deploy_env(cfg, req.env.as_deref(), &env_path, task).await?;
    task.log(format!("writing {}", config_path(name).display()));
    write_config(name, spec).await?;

    for folder in &spec.volumes {
        let volume = volume_name(name, folder);
        if exec::run("docker", ["volume", "inspect", &volume]).await.is_err() {
            task.log(format!("creating volume {volume} for {folder}"));
            exec::run("docker", ["volume", "create", "--label", &format!("vps-panel.app={name}"), &volume])
                .await?;
        }
    }

    // Keep the image the app ran until now as :previous, so a redeploy onto
    // another image can still be rolled back by hand.
    if let Some(cur) = image_id(&current_tag(name)).await {
        if cur != new_id {
            tag(&current_tag(name), &previous_tag(name), task).await?;
        }
    }
    tag(&spec.image, &current_tag(name), task).await?;

    let unit_existed = unit_path.exists();
    task.log(format!("writing systemd unit {}", unit_path.display()));
    write_unit(name, req.port, spec).await?;
    exec::run("systemctl", ["daemon-reload"]).await?;
    task.log(format!("systemctl enable + restart {unit}"));
    let _ = exec::run("systemctl", ["enable", &unit]).await;
    if let Err(e) = apps::app_systemctl(&["restart"], &unit).await {
        // `docker run` failing in ExecStartPre (a port already taken, a bad
        // flag) only shows in the journal.
        apps::log_journal_tail(&unit, 40, task).await;
        let _ = exec::run("systemctl", ["disable", "--now", &unit]).await;
        if !unit_existed {
            apps::rollback_unit(&unit_path).await;
        }
        return Err(e);
    }
    task.log(format!(
        "waiting up to {START_WAIT_SECS}s for the app to answer on 127.0.0.1:{} (container port {}), then 10s to confirm it stays up",
        req.port, spec.container_port
    ));
    if let Err(reason) = verify_up(&unit, req.port).await {
        task.log(format!("=== {reason} ==="));
        apps::log_journal_tail(&unit, 80, task).await;
        let _ = exec::run("systemctl", ["disable", "--now", &unit]).await;
        return Err(AgentError::Conflict(format!(
            "{reason}. See the container output above. The unit is left installed (stopped and disabled): \
             fix the settings and Deploy again."
        )));
    }
    apps::publish_vhost(cfg, acme_email, req, &unit, &unit_path, unit_existed, task).await
}

/// POST /apps/:name/update for a Docker app: pull the image again and move the
/// app onto it if it changed. A new image that does not come up is rolled
/// back to the one that ran before.
pub async fn run_update(name: &str, slot_port: Option<u16>, task: &TaskHandle) -> AgentResult<()> {
    let spec = read_config(name).await.ok_or(AgentError::NotFound)?;
    docker_bin()?;
    let unit = unit_name(name);
    if spec.image.contains('@') {
        task.log("the image is pinned to a digest, so there is nothing newer to pull");
        return Ok(());
    }
    if let Err(e) = pull(&spec.image, task).await {
        return Err(AgentError::Other(anyhow::anyhow!("{e}. The app keeps running the current image.")));
    }
    let new_id = image_id(&spec.image)
        .await
        .ok_or_else(|| AgentError::Invalid(format!("{} is not on this server after the pull", spec.image)))?;
    let cur_id = image_id(&current_tag(name)).await;
    if cur_id.as_deref() == Some(new_id.as_str()) {
        task.log(format!("already on the newest {} ({})", spec.image, short_id(&new_id)));
        return Ok(());
    }
    let old_previous = image_id(&previous_tag(name)).await;
    if cur_id.is_some() {
        tag(&current_tag(name), &previous_tag(name), task).await?;
    }
    tag(&spec.image, &current_tag(name), task).await?;

    let Some(port) = slot_port else {
        task.log("the app has no domain assigned: the new image starts once it has one");
        return Ok(());
    };
    task.log(format!("restarting {unit} on {}", short_id(&new_id)));
    apps::app_systemctl(&["restart"], &unit).await?;
    match verify_up(&unit, port).await {
        Ok(()) => {
            // The image two updates back is no longer a rollback target.
            if let Some(old) = old_previous {
                if Some(&old) != cur_id.as_ref() && old != new_id {
                    let _ = exec::run("docker", ["rmi", &old]).await;
                }
            }
            task.log("the new image is up");
            Ok(())
        }
        Err(reason) => {
            task.log(format!("=== {reason} ==="));
            apps::log_journal_tail(&unit, 60, task).await;
            if cur_id.is_none() {
                return Err(AgentError::Other(anyhow::anyhow!(
                    "{reason}; there was no previous image to roll back to"
                )));
            }
            task.log("rolling back to the previous image");
            tag(&previous_tag(name), &current_tag(name), task).await?;
            let _ = apps::app_systemctl(&["restart"], &unit).await;
            let rollback = match verify_up(&unit, port).await {
                Ok(()) => "the previous image is running again".to_string(),
                Err(e) => format!("the previous image did not come back up either: {e}"),
            };
            Err(AgentError::Other(anyhow::anyhow!(
                "the new image did not stay up ({reason}); {rollback}"
            )))
        }
    }
}

/// Everything but the unit and env file (apps::delete removes those): the
/// container, the app's image tags, the settings file and, with `wipe`, the
/// data volumes, also ones a later deploy no longer listed (found by label).
pub async fn remove_app(name: &str, spec: &DockerSpec, wipe: bool) {
    let _ = exec::run("docker", ["rm", "-f", &container_name(name)]).await;
    // Untags; the image itself goes only when nothing else uses it.
    let _ = exec::run("docker", ["rmi", &current_tag(name)]).await;
    let _ = exec::run("docker", ["rmi", &previous_tag(name)]).await;
    let _ = fs_atomic::remove_if_exists(&config_path(name)).await;
    if !wipe {
        return;
    }
    let mut volumes: Vec<String> = spec.volumes.iter().map(|v| volume_name(name, v)).collect();
    if let Ok(out) = exec::run(
        "docker",
        ["volume", "ls", "-q", "--filter", &format!("label=vps-panel.app={name}")],
    )
    .await
    {
        for v in out.stdout.lines().map(str::trim).filter(|v| !v.is_empty()) {
            if !volumes.iter().any(|x| x == v) {
                volumes.push(v.to_string());
            }
        }
    }
    for v in volumes {
        let _ = exec::run("docker", ["volume", "rm", &v]).await;
    }
}

#[derive(Serialize)]
pub struct VolumeInfo {
    pub container_path: String,
    pub volume: String,
    /// Where the data sits on the server, once the volume exists.
    pub host_path: Option<String>,
}

#[derive(Serialize)]
pub struct DockerAppInfo {
    pub spec: DockerSpec,
    pub current_id: Option<String>,
    /// The image Update rolls back to.
    pub previous_id: Option<String>,
    pub volumes: Vec<VolumeInfo>,
}

/// GET /apps/:name/docker
pub async fn info(Path(name): Path<String>) -> AgentResult<Json<DockerAppInfo>> {
    validate_name(&name)?;
    let spec = read_config(&name).await.ok_or(AgentError::NotFound)?;
    let mut volumes = Vec::new();
    for folder in &spec.volumes {
        let volume = volume_name(&name, folder);
        let host_path = exec::run("docker", ["volume", "inspect", "--format", "{{.Mountpoint}}", &volume])
            .await
            .ok()
            .map(|o| o.stdout.trim().to_string())
            .filter(|p| !p.is_empty());
        volumes.push(VolumeInfo {
            container_path: folder.clone(),
            volume,
            host_path,
        });
    }
    Ok(Json(DockerAppInfo {
        current_id: image_id(&current_tag(&name)).await,
        previous_id: image_id(&previous_tag(&name)).await,
        spec,
        volumes,
    }))
}

#[derive(Serialize)]
pub struct ImageStatus {
    pub image: String,
    pub current_id: Option<String>,
    /// None = could not tell (see `error`).
    pub update_available: Option<bool>,
    pub error: Option<String>,
}

/// GET /apps/:name/image-status: does the registry have a newer image than
/// the one the app runs? The same digest comparison as n8n's update badge.
pub async fn image_status(Path(name): Path<String>) -> AgentResult<Json<ImageStatus>> {
    validate_name(&name)?;
    let spec = read_config(&name).await.ok_or(AgentError::NotFound)?;
    let current_id = image_id(&current_tag(&name)).await;
    let mut status = ImageStatus {
        image: spec.image.clone(),
        current_id: current_id.clone(),
        update_available: None,
        error: None,
    };
    if spec.image.contains('@') {
        status.update_available = Some(false);
        return Ok(Json(status));
    }
    let Some(current) = current_id else {
        status.error = Some("the app's image is not on this server - Deploy it again".into());
        return Ok(Json(status));
    };
    match tokio::time::timeout(
        Duration::from_secs(REGISTRY_TIMEOUT_SECS),
        crate::handlers::quick_install::remote_image_config_digests(&spec.image),
    )
    .await
    {
        Err(_) => status.error = Some(format!("the registry did not answer within {REGISTRY_TIMEOUT_SECS}s")),
        Ok(remote) if remote.is_empty() => {
            status.error = Some(format!("could not read {} from its registry", spec.image))
        }
        Ok(remote) => status.update_available = Some(!remote.contains(&current)),
    }
    Ok(Json(status))
}

#[derive(Serialize)]
pub struct DockerStatus {
    pub installed: bool,
    /// The daemon answers.
    pub running: bool,
    pub version: Option<String>,
}

/// GET /docker/status
pub async fn status() -> Json<DockerStatus> {
    let installed = docker_bin().is_ok();
    let version = if installed {
        exec::run("docker", ["version", "--format", "{{.Server.Version}}"])
            .await
            .ok()
            .map(|o| o.stdout.trim().to_string())
            .filter(|v| !v.is_empty())
    } else {
        None
    };
    Json(DockerStatus {
        installed,
        running: version.is_some(),
        version,
    })
}

#[derive(Deserialize)]
pub struct ImageQuery {
    pub image: String,
}

/// GET /docker/image-info?image=: what a pulled image declares, for the
/// deploy form to fill in the port and folders.
pub async fn image_info(Query(q): Query<ImageQuery>) -> AgentResult<Json<ImageInfo>> {
    let image = q.image.trim();
    validate_image(image)?;
    inspect_image(image).await.map(Json).ok_or(AgentError::NotFound)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> DockerSpec {
        DockerSpec {
            image: "louislam/uptime-kuma:1".into(),
            container_port: 3001,
            volumes: vec!["/app/data".into()],
            memory_mb: 512,
            cpus: 1.0,
        }
    }

    #[test]
    fn image_names_docker_accepts() {
        for ok in [
            "nginx",
            "nginx:1.27-alpine",
            "louislam/uptime-kuma:1",
            "ghcr.io/owner/app:v1.4.0",
            "registry.example.com:5000/team/app",
            "localhost/app:dev",
            "my_org/my__app",
            "a/b-c--d",
            "n8nio/n8n@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "ghcr.io/owner/app:1@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        ] {
            assert!(validate_image(ok).is_ok(), "{ok} should be valid");
        }
    }

    #[test]
    fn image_names_that_are_refused() {
        for bad in [
            "",
            "-v",
            "--privileged",
            "Nginx",
            "nginx:",
            "nginx:-x",
            "nginx latest",
            "nginx;rm",
            "a//b",
            "a/.b",
            "a/b_",
            "a/b___c",
            "nginx@sha256:abc",
            "nginx:%n",
            "nginx:$HOME",
            "-registry.io/app",
            "vps-app-other:current",
        ] {
            assert!(validate_image(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn folders_to_keep() {
        assert!(validate_volume_path("/app/data").is_ok());
        assert!(validate_volume_path("/var/lib/postgresql/data").is_ok());
        for bad in ["app/data", "/", "/a/../b", "/a//b", "/a/./b", "/a b", "/a:b", "/a,b", "/__"] {
            assert!(validate_volume_path(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn volume_names() {
        assert_eq!(volume_name("kuma", "/app/data"), "vps-app-kuma-app-data");
        assert_eq!(volume_name("pg", "/var/lib/postgresql/data"), "vps-app-pg-var-lib-postgresql-data");
        assert_eq!(volume_name("x", "/Data_2"), "vps-app-x-data-2");
    }

    #[test]
    fn normalize_and_collisions() {
        let s = normalize_spec(DockerSpec {
            image: " nginx:1 ".into(),
            volumes: vec!["/data/".into(), " ".into(), "/data".into(), "/cache".into()],
            ..spec()
        });
        assert_eq!(s.image, "nginx:1");
        assert_eq!(s.volumes, vec!["/data".to_string(), "/cache".to_string()]);
        let clash = DockerSpec { volumes: vec!["/app/data".into(), "/app-data".into()], ..spec() };
        assert!(validate_spec(&clash).is_err());
    }

    #[test]
    fn limits() {
        assert!(validate_spec(&spec()).is_ok());
        assert!(validate_spec(&DockerSpec { memory_mb: 0, cpus: 0.0, ..spec() }).is_ok());
        assert!(validate_spec(&DockerSpec { memory_mb: 32, ..spec() }).is_err());
        assert!(validate_spec(&DockerSpec { cpus: 0.05, ..spec() }).is_err());
        assert!(validate_spec(&DockerSpec { cpus: f64::NAN, ..spec() }).is_err());
        assert!(validate_spec(&DockerSpec { cpus: 100_000.0, ..spec() }).is_err());
        assert!(validate_spec(&DockerSpec { container_port: 0, ..spec() }).is_err());
        assert_eq!(format_cpus(1.0), "1");
        assert_eq!(format_cpus(0.5), "0.5");
        assert_eq!(format_cpus(1.25), "1.25");
    }

    #[test]
    fn saved_settings_get_the_default_limits() {
        let s: DockerSpec = serde_json::from_str(r#"{"image":"nginx","container_port":80}"#).unwrap();
        assert_eq!((s.memory_mb, s.cpus, s.volumes.len()), (1024, 1.0, 0));
    }

    #[test]
    fn unit_publishes_on_loopback_only() {
        let unit = render_unit("kuma", 7010, &spec(), "/usr/bin/docker");
        assert!(unit.contains("-p 127.0.0.1:7010:3001"));
        assert!(!unit.contains("0.0.0.0"));
        assert!(unit.contains("Environment=PORT=7010"));
        assert!(unit.contains("--env-file /etc/vps-panel/apps/kuma.env"));
        assert!(unit.contains("-v vps-app-kuma-app-data:/app/data"));
        assert!(unit.contains("--memory 512m"));
        assert!(unit.contains("--cpus 1"));
        assert!(unit.contains("--security-opt no-new-privileges"));
        assert!(unit.contains("--pull=never"));
        assert!(unit.contains("Requires=docker.service"));
        assert!(!unit.contains("--privileged") && !unit.contains("--network"));
        assert!(unit.contains("ExecStart=/usr/bin/docker logs --follow vps-app-kuma\n"));
        assert!(unit.contains("Restart=always"));
        // The image is the last argument of the run line, and every line of
        // it but the last ends with a continuation backslash.
        let run: Vec<&str> = unit
            .lines()
            .skip_while(|l| !l.starts_with("ExecStartPre=/usr/bin/docker run --detach"))
            .take_while(|l| !l.starts_with('#') && !l.starts_with("ExecStart="))
            .collect();
        assert!(run.len() > 5);
        assert_eq!(run[run.len() - 1], "  vps-app-kuma:current");
        assert!(run[..run.len() - 1].iter().all(|l| l.ends_with(" \\")));
    }

    #[test]
    fn unit_without_limits_or_folders() {
        let s = DockerSpec { memory_mb: 0, cpus: 0.0, volumes: vec![], ..spec() };
        let unit = render_unit("web", 7011, &s, "/usr/bin/docker");
        assert!(!unit.contains("--memory") && !unit.contains("--cpus") && !unit.contains(" -v "));
    }

    #[test]
    fn image_info_from_inspect() {
        let json = r#"[{"Id":"sha256:abc","Config":{"User":"node","ExposedPorts":{"3001/tcp":{},"53/udp":{},"80/tcp":{}},"Volumes":{"/app/data":{}}}}]"#;
        assert_eq!(
            parse_image_info(json),
            Some(ImageInfo {
                id: "sha256:abc".into(),
                exposed_ports: vec![80, 3001],
                volumes: vec!["/app/data".into()],
                user: "node".into(),
            })
        );
        let bare = r#"[{"Id":"sha256:def","Config":{"ExposedPorts":null,"Volumes":null}}]"#;
        let info = parse_image_info(bare).unwrap();
        assert!(info.exposed_ports.is_empty() && info.volumes.is_empty() && info.user.is_empty());
    }
}
