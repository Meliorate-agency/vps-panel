use crate::error::{AgentError, AgentResult};
use crate::exec;
use crate::fs_atomic;
use crate::handlers::nginx as nginx_handler;
use crate::ipc::OkResponse;
use crate::nginx_template::{SiteVhostTemplate, ACME_WEBROOT};
use crate::state::AppState;
use crate::validate::{is_safe_folder_path, is_safe_subdomain};
use askama::Template;
use axum::{
    extract::{Path, State},
    Json,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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

fn default_mode() -> String {
    "port".to_string()
}

#[derive(Deserialize)]
pub struct CreateSite {
    pub domain: String,
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default)]
    pub upstream_port: Option<u16>,
    #[serde(default)]
    pub folder_path: Option<String>,
    #[serde(default)]
    pub force_www_redirect: bool,
    #[serde(default)]
    pub issue_ssl: bool,
}

#[derive(Deserialize)]
pub struct UpdateSite {
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default)]
    pub upstream_port: Option<u16>,
    #[serde(default)]
    pub folder_path: Option<String>,
    #[serde(default)]
    pub force_www_redirect: bool,
    /// Order a certificate when the one on disk does not cover the site.
    /// False orders nothing; it does not take HTTPS away (see `remove_ssl`).
    #[serde(default)]
    pub issue_ssl: bool,
    /// Rewrite the site to HTTP only although it serves HTTPS with a valid
    /// certificate.
    #[serde(default)]
    pub remove_ssl: bool,
}

#[derive(Serialize)]
pub struct CreateSiteResponse {
    pub ok: bool,
    pub task_id: Option<String>,
}

#[derive(Serialize)]
pub struct SiteStatus {
    pub cert_exists: bool,
    pub cert_not_after: Option<String>,
    pub days_remaining: Option<i64>,
    /// Set when `domain` has no cert directory of its own but the parent
    /// domain's cert lists it as a SAN - `mail.<d>` and `www.<d>` normally
    /// ride on `<d>`'s cert. The UI shows "via <parent>" and sends renewals
    /// to the parent.
    pub covered_by: Option<String>,
    /// The sites-enabled file, other than the ones a delete of `domain`
    /// removes, whose vhost serves `www.<domain>`. While there is one, a
    /// create of `domain` with Force www is refused. The panel's Replace
    /// asks this before it deletes the site, because the refusal would only
    /// come after the delete and leave the site gone.
    pub www_served_by: Option<String>,
    /// For `www.<d>`: `<d>` when its site vhost already answers this name
    /// through Force www, which refuses a create of `www.<d>` (asked by the
    /// panel's Replace for the same reason).
    pub force_www_parent: Option<String>,
}

#[derive(Serialize)]
pub struct SiteListEntry {
    pub domain: String,
    /// "site"        - panel-managed Sites vhost (filename `site-<domain>`)
    /// "app"         - panel-deployed app vhost (marker `# vps-app:<name>`)
    /// "panel-host"  - vhost serving the panel UI itself (marker `# vps-panel-host:`)
    /// "external"    - manual deploy / certbot-touched, no panel marker
    pub kind: String,
}

#[derive(Clone)]
struct RouteSpec {
    mode: String,
    port: Option<u16>,
    folder: Option<String>,
    force_www_redirect: bool,
}


fn vhost_paths(domain: &str) -> (PathBuf, PathBuf) {
    (
        PathBuf::from(format!("/etc/nginx/sites-available/site-{domain}")),
        PathBuf::from(format!("/etc/nginx/sites-enabled/site-{domain}")),
    )
}

fn cert_paths(domain: &str) -> (String, String) {
    (
        format!("/etc/vps-panel/certs/{domain}/fullchain.pem"),
        format!("/etc/vps-panel/certs/{domain}/privkey.pem"),
    )
}

fn certbot_paths(domain: &str) -> (String, String) {
    (
        format!("/etc/letsencrypt/live/{domain}/fullchain.pem"),
        format!("/etc/letsencrypt/live/{domain}/privkey.pem"),
    )
}

// Returns the (fullchain, privkey) of an actually-existing certificate for the
// domain. Panel-managed certs take priority; falls back to certbot's directory
// so externally-issued certs (e.g. from running `certbot --nginx` by hand) are
// still recognized by the status endpoint and reused by the vhost template.
fn existing_cert_paths(domain: &str) -> Option<(String, String)> {
    let panel = cert_paths(domain);
    if std::path::Path::new(&panel.0).exists() {
        return Some(panel);
    }
    let cb = certbot_paths(domain);
    if std::path::Path::new(&cb.0).exists() {
        return Some(cb);
    }
    None
}

fn build_route(
    mode: &str,
    port: Option<u16>,
    folder: Option<String>,
    force_www_redirect: bool,
) -> AgentResult<RouteSpec> {
    match mode {
        "port" => {
            let p = port.ok_or_else(|| {
                AgentError::Invalid("upstream_port required for mode=port".into())
            })?;
            if !(1..=65535).contains(&p) {
                return Err(AgentError::Invalid(
                    "upstream_port must be 1..=65535".into(),
                ));
            }
            // nginx itself listens on 80 and 443. A site that proxies there
            // proxies to itself: every request loops until nginx is out of
            // connections, for all sites.
            if p == 80 || p == 443 {
                return Err(AgentError::Invalid(format!(
                    "upstream_port {p} is nginx itself, so the site would proxy to itself; \
                     use the port the app listens on"
                )));
            }
            if folder.is_some() {
                return Err(AgentError::Invalid(
                    "folder_path must be empty for mode=port".into(),
                ));
            }
            Ok(RouteSpec {
                mode: "port".into(),
                port: Some(p),
                folder: None,
                force_www_redirect,
            })
        }
        "folder" => {
            let f = folder
                .ok_or_else(|| AgentError::Invalid("folder_path required for mode=folder".into()))?;
            if !is_safe_folder_path(&f) {
                return Err(AgentError::Invalid("invalid folder_path".into()));
            }
            if port.is_some() {
                return Err(AgentError::Invalid(
                    "upstream_port must be empty for mode=folder".into(),
                ));
            }
            Ok(RouteSpec {
                mode: "folder".into(),
                port: None,
                folder: Some(f),
                force_www_redirect,
            })
        }
        other => Err(AgentError::Invalid(format!(
            "invalid mode: {other} (expected 'port' or 'folder')"
        ))),
    }
}

async fn ensure_folder(path: &str) -> AgentResult<()> {
    tokio::fs::create_dir_all(path)
        .await
        .map_err(AgentError::Io)?;
    exec::run("chown", ["-R", "www-data:www-data", path]).await?;
    Ok(())
}

async fn cert_covers_names(cert_path: &str, names: &[String]) -> bool {
    let Some(out) = exec::run("openssl", ["x509", "-in", cert_path, "-noout", "-text"])
        .await
        .ok()
    else {
        return false;
    };
    names
        .iter()
        .all(|n| out.stdout.contains(&format!("DNS:{n}")))
}

/// The names the site's certificate must list. `www.<domain>` whenever the
/// flag is set, because the vhost then has a www block: it used to be asked
/// for two-label domains only, so on example.co.uk or a subdomain (the flag
/// reaches those through the MCP tool) the block answered on 443 with a
/// certificate that did not list the name.
fn ssl_required_names(domain: &str, force_www_redirect: bool) -> Vec<String> {
    if force_www_redirect {
        vec![domain.to_string(), format!("www.{domain}")]
    } else {
        vec![domain.to_string()]
    }
}

async fn render_and_install(
    domain: &str,
    route: &RouteSpec,
    ssl_enabled: bool,
) -> AgentResult<()> {
    let (vhost_path, vhost_link) = vhost_paths(domain);
    // Prefer an existing cert (panel- or certbot-managed) so vhost rewrites
    // never leave nginx pointing at a missing file. For freshly issued certs
    // the panel will have written to its own path by the time we render.
    let (cert_path, key_path) = existing_cert_paths(domain).unwrap_or_else(|| cert_paths(domain));
    // The other half of ssl_required_names: a certificate without www (the
    // one in use while a new one is ordered, or a renewal that had to leave
    // the name out) gets no www block on 443.
    let www_in_cert = ssl_enabled
        && route.force_www_redirect
        && crate::handlers::ssl::cert_covers(
            std::path::Path::new(&cert_path),
            &format!("www.{domain}"),
        )
        .await;
    let body = SiteVhostTemplate {
        domain,
        mode: &route.mode,
        upstream_port: route.port,
        folder_path: route.folder.as_deref(),
        force_www_redirect: route.force_www_redirect,
        ssl_enabled,
        www_in_cert,
        cert_path: &cert_path,
        key_path: &key_path,
        acme_webroot: ACME_WEBROOT,
    }
    .render()
    .map_err(|e| AgentError::Other(anyhow::anyhow!("askama: {e}")))?;
    fs_atomic::write(&vhost_path, body.as_bytes(), 0o644).await?;
    if !vhost_link.exists() {
        symlink(&vhost_path, &vhost_link).map_err(AgentError::Io)?;
    }
    Ok(())
}

/// `render_and_install` + `nginx -t`, putting the previous vhost back when the
/// test fails. A config that fails the test must not stay on disk: every later
/// reload fails with it, and an nginx restart would take all sites down.
/// (Update and the SSL task used to leave it in place; create already rolled
/// back.)
async fn install_vhost_tested(
    domain: &str,
    route: &RouteSpec,
    ssl_enabled: bool,
) -> AgentResult<()> {
    let (vhost_path, vhost_link) = vhost_paths(domain);
    let previous = tokio::fs::read(&vhost_path).await.ok();
    let had_link = tokio::fs::symlink_metadata(&vhost_link).await.is_ok();
    render_and_install(domain, route, ssl_enabled).await?;
    if let Err(e) = nginx_handler::test_config().await {
        match previous {
            Some(bytes) => {
                let _ = fs_atomic::write(&vhost_path, &bytes, 0o644).await;
            }
            None => {
                let _ = fs_atomic::remove_if_exists(&vhost_path).await;
            }
        }
        if !had_link {
            let _ = fs_atomic::remove_if_exists(&vhost_link).await;
        }
        return Err(e);
    }
    Ok(())
}

/// A certificate order for the site is running. When it lands, its task
/// writes the vhost with the route it was started with, so a change made in
/// the meantime would be undone without a word (and a deleted site written
/// back): the change waits until the order is done.
fn refuse_while_issuing(state: &AppState, domain: &str) -> AgentResult<()> {
    if state.tasks.live_task_id(&format!("sites.ssl:{domain}")).is_some() {
        return Err(AgentError::Conflict(format!(
            "a certificate for {domain} is being issued - change the site once that is done (a minute or two)"
        )));
    }
    Ok(())
}

async fn spawn_ssl_task(state: &AppState, domain: String, route: RouteSpec) -> String {
    let task = state.tasks.create(&format!("sites.ssl:{domain}"));
    let task_id = task.id.clone();
    let cfg = state.config.clone();
    let panel_db = state.panel_db.clone();
    let state_clone = state.clone();
    tokio::spawn(async move {
        task.start();
        let acme_email = crate::handlers::ssl::resolve_acme_email(&panel_db, &cfg);
        // www comes from the request (the panel.db row may not exist yet at
        // create time); mail.<domain> comes from the agent's view of mail
        // tenants via desired_san_set, so a site whose domain is also a mail
        // domain gets a cert Postfix/Dovecot can present for mail.<domain>.
        let mut names = ssl_required_names(&domain, route.force_www_redirect);
        for extra in crate::handlers::ssl::desired_san_set(&state_clone, &domain).await {
            if !names.contains(&extra) {
                names.push(extra);
            }
        }
        crate::handlers::ssl::ensure_mail_acme_helpers(&state_clone, &names, &task).await;
        // Pre-flight DNS for every SAN so ACME doesn't spend 60s timing out
        // on a missing record. Also auto-creates the record in PowerDNS when
        // the parent zone is panel-managed.
        let mut preflight_failed = Vec::new();
        for name in &names {
            if let Err(e) = crate::handlers::dns::preflight_acme_dns(&state_clone, name, &task).await {
                task.log(format!("ssl preflight failed for {name}: {e}"));
                preflight_failed.push(format!("{name}: {e}"));
            }
        }
        if !preflight_failed.is_empty() {
            crate::handlers::ssl::log_preflight_failures(&preflight_failed, &task);
            task.finish(false);
            return;
        }
        match crate::handlers::ssl::issue_for_names(&cfg, &acme_email, &domain, &names, &task).await {
            Ok(()) => {
                task.log("rewriting vhost with SSL enabled");
                if let Err(e) = install_vhost_tested(&domain, &route, true).await {
                    task.log(format!(
                        "the vhost with SSL failed nginx -t, so the previous vhost was put back: {e}"
                    ));
                    task.finish(false);
                    return;
                }
                if let Err(e) = exec::run("systemctl", ["reload", "nginx"]).await {
                    task.log(format!("reload nginx: {e}"));
                    task.finish(false);
                    return;
                }
                // The cert may carry mail.<domain>: let Postfix/Dovecot
                // present it now instead of at the next regeneration.
                if let Err(e) = crate::handlers::ssl::regenerate_mail_sni(&state_clone).await {
                    task.log(format!("mail sni regen warning: {e}"));
                }
                task.finish(true);
            }
            Err(e) => {
                task.log(format!("ssl issuance failed: {e}"));
                task.finish(false);
            }
        }
    });
    task_id
}

/// Does this vhost have a server block whose server_name lists `name`
/// exactly? A wildcard is not counted: nginx prefers an exact name over it,
/// so the two do not clash.
fn vhost_serves_name(vhost: &str, name: &str) -> bool {
    vhost.lines().any(|line| {
        let Some(rest) = line.trim_start().strip_prefix("server_name") else {
            return false;
        };
        if !rest.starts_with([' ', '\t']) {
            return false;
        }
        let names = rest.split(';').next().unwrap_or("");
        names.split_whitespace().any(|n| n.eq_ignore_ascii_case(name))
    })
}

/// For `www.<d>`, the `<d>` whose Force www vhost may already answer it.
fn www_parent(domain: &str) -> Option<&str> {
    domain
        .strip_prefix("www.")
        .filter(|parent| is_safe_subdomain(parent))
}

/// Two vhosts with the same server_name only make `nginx -t` warn and pass,
/// and nginx then serves whichever loads first: the other one is ignored
/// without a word while the panel lists it as created. A `www.<d>` site
/// next to `<d>` with Force www (its vhost has a block for www.<d>) is that
/// clash, so it is refused here with what to change instead. The vhost on
/// disk is asked rather than the panel.db row: it is what nginx loads, and a
/// site made through the MCP tool has no row.
///
/// This half is for creating `www.<d>` while `<d>` already answers it.
async fn refuse_www_site_under_force_www(domain: &str) -> AgentResult<()> {
    if let Some(parent) = force_www_parent_serving(domain).await {
        return Err(AgentError::Conflict(format!(
            "{parent} already serves {domain} through Force www; \
             turn Force www off on {parent} first, then add {domain} as its own site"
        )));
    }
    Ok(())
}

/// For `www.<d>`: `<d>` when its site vhost already answers `www.<d>`
/// through Force www.
async fn force_www_parent_serving(domain: &str) -> Option<&str> {
    let parent = www_parent(domain)?;
    let parent_vhost = format!("/etc/nginx/sites-enabled/site-{parent}");
    tokio::fs::read_to_string(&parent_vhost)
        .await
        .is_ok_and(|vhost| vhost_serves_name(&vhost, domain))
        .then_some(parent)
}

/// The other half: Force www on `<d>` while another vhost answers `www.<d>`.
async fn refuse_force_www_over_www_site(domain: &str) -> AgentResult<()> {
    let www = format!("www.{domain}");
    let own = format!("site-{domain}");
    if let Some(file) = other_vhost_serving(&www, &[own.as_str()]).await {
        return Err(AgentError::Conflict(format!(
            "{www} is served by a vhost of its own ({file}), so nginx would ignore \
             Force www on {domain}; remove the {www} site first, or leave Force www off"
        )));
    }
    Ok(())
}

/// The files in sites-enabled that a delete of `domain` removes: the
/// panel's `site-<d>` and an external vhost named `<d>`.
fn files_removed_by_delete(domain: &str) -> [String; 2] {
    [format!("site-{domain}"), domain.to_string()]
}

/// Is this sites-enabled file one to look into for a clash? Dot files are
/// left out because nginx's include glob does not match them.
fn is_other_vhost_file(file: &str, own_files: &[&str]) -> bool {
    !file.starts_with('.') && !own_files.contains(&file)
}

/// The file in sites-enabled, other than `own_files`, whose vhost has a
/// server block for `name`. Every file there is loaded by nginx, so a
/// backup copy or an app vhost clashes as much as a site does.
async fn other_vhost_serving(name: &str, own_files: &[&str]) -> Option<String> {
    let mut rd = tokio::fs::read_dir("/etc/nginx/sites-enabled").await.ok()?;
    while let Ok(Some(entry)) = rd.next_entry().await {
        let file = entry.file_name().to_string_lossy().into_owned();
        if !is_other_vhost_file(&file, own_files) {
            continue;
        }
        let Ok(vhost) = tokio::fs::read_to_string(entry.path()).await else {
            continue;
        };
        if vhost_serves_name(&vhost, name) {
            return Some(file);
        }
    }
    None
}

pub async fn create(
    State(state): State<AppState>,
    Json(req): Json<CreateSite>,
) -> AgentResult<Json<CreateSiteResponse>> {
    if !is_safe_subdomain(&req.domain) {
        return Err(AgentError::Invalid("invalid domain".into()));
    }
    // The panel's own hostname is served by the panel vhost. A site with the
    // same name is loaded first by nginx and hides the panel (an empty folder
    // site answered 403).
    if req.domain.eq_ignore_ascii_case(&crate::handlers::system::current_panel_host().await) {
        return Err(AgentError::Conflict(format!(
            "{} is the panel's own hostname; the panel already serves it",
            req.domain
        )));
    }
    let route = build_route(
        &req.mode,
        req.upstream_port,
        req.folder_path,
        req.force_www_redirect,
    )?;

    let (vhost_path, vhost_link) = vhost_paths(&req.domain);
    if vhost_path.exists() {
        return Err(AgentError::Conflict(format!(
            "site for {} already exists",
            req.domain
        )));
    }
    refuse_www_site_under_force_www(&req.domain).await?;
    if route.force_www_redirect {
        refuse_force_www_over_www_site(&req.domain).await?;
    }

    if let Some(ref f) = route.folder {
        ensure_folder(f).await?;
    }

    let needed_names = ssl_required_names(&req.domain, route.force_www_redirect);
    let cert_ok = match existing_cert_paths(&req.domain) {
        Some((cert_path, _)) => cert_covers_names(&cert_path, &needed_names).await,
        None => false,
    };
    let initial_ssl = cert_ok && req.issue_ssl;

    render_and_install(&req.domain, &route, initial_ssl).await?;

    if let Err(e) = nginx_handler::test_config().await {
        let _ = std::fs::remove_file(&vhost_link);
        let _ = std::fs::remove_file(&vhost_path);
        return Err(e);
    }
    // A mail tenant's ACME helper vhost for mail.<tenant> is loaded before
    // this site and keeps port 80 for the name (its 404) until it is gone.
    // It removes itself once the site exists, but that ran only at the next
    // certificate order or renewal check. With the site in place the call
    // never installs anything.
    if let Some(tenant) = req.domain.strip_prefix("mail.") {
        if let Err(e) = crate::handlers::mail::ensure_mail_http_vhost(tenant).await {
            tracing::warn!(domain = %req.domain, error = %e, "could not remove the ACME helper vhost");
        }
    }
    exec::run("systemctl", ["reload", "nginx"]).await?;

    let task_id = if req.issue_ssl && !cert_ok {
        Some(spawn_ssl_task(&state, req.domain.clone(), route).await)
    } else {
        None
    };

    Ok(Json(CreateSiteResponse { ok: true, task_id }))
}

/// Whether an updated site is written with its 443 block. With `issue_ssl`
/// as before: whenever a certificate covers the site. Without it the site
/// keeps the HTTPS it has, as long as that certificate is valid: "Auto-issue
/// SSL" unticked on the routing form (an adopted site, a certificate
/// installed by hand) and `issue_ssl: false` from the MCP tool mean "order
/// nothing". They used to rewrite a working HTTPS site to HTTP only, and
/// browsers that had seen its HSTS header then refused to open it. Only
/// `remove_ssl` takes HTTPS away.
fn https_after_update(
    issue_ssl: bool,
    remove_ssl: bool,
    cert_covers_site: bool,
    cert_valid: bool,
    serves_https: bool,
) -> bool {
    if remove_ssl || !cert_covers_site {
        return false;
    }
    issue_ssl || (serves_https && cert_valid)
}

/// Does this vhost have a server block on 443?
fn vhost_serves_https(vhost: &str) -> bool {
    vhost.lines().map(str::trim_start).any(|l| {
        l.strip_prefix("listen")
            .is_some_and(|rest| rest.starts_with([' ', '\t']) && rest.contains("443"))
    })
}

pub async fn update(
    State(state): State<AppState>,
    Path(domain): Path<String>,
    Json(req): Json<UpdateSite>,
) -> AgentResult<Json<CreateSiteResponse>> {
    if !is_safe_subdomain(&domain) {
        return Err(AgentError::Invalid("invalid domain".into()));
    }
    if req.issue_ssl && req.remove_ssl {
        return Err(AgentError::Invalid(
            "issue_ssl and remove_ssl cannot both be set".into(),
        ));
    }
    refuse_while_issuing(&state, &domain)?;
    let route = build_route(
        &req.mode,
        req.upstream_port,
        req.folder_path,
        req.force_www_redirect,
    )?;

    let (vhost_path, _vhost_link) = vhost_paths(&domain);
    if !vhost_path.exists() {
        return Err(AgentError::NotFound);
    }
    // Only the Force www half: an update keeps the site's own server_name,
    // so an update of a www.<d> site adds no clash of its own.
    if route.force_www_redirect {
        refuse_force_www_over_www_site(&domain).await?;
    }
    let serves_https = tokio::fs::read_to_string(&vhost_path)
        .await
        .is_ok_and(|vhost| vhost_serves_https(&vhost));

    if let Some(ref f) = route.folder {
        ensure_folder(f).await?;
    }

    let needed_names = ssl_required_names(&domain, route.force_www_redirect);
    let (cert_ok, cert_covers_site, cert_valid) = match existing_cert_paths(&domain) {
        Some((cert_path, _)) => (
            cert_covers_names(&cert_path, &needed_names).await,
            cert_covers_names(&cert_path, &[domain.clone()]).await,
            crate::handlers::ssl::cert_expiry(std::path::Path::new(&cert_path))
                .await
                .is_some_and(|(_, days)| days >= 0),
        ),
        None => (false, false, false),
    };
    // A cert that covers the site but not yet www stays in use while the new
    // one is ordered. Turning "Force www" on used to rewrite the vhost
    // without SSL first, and when the order then failed (no DNS record for
    // www) the site was left on plain HTTP with a valid cert on disk.
    let ssl_now = https_after_update(
        req.issue_ssl,
        req.remove_ssl,
        cert_covers_site,
        cert_valid,
        serves_https,
    );

    install_vhost_tested(&domain, &route, ssl_now).await?;
    exec::run("systemctl", ["reload", "nginx"]).await?;

    let task_id = if req.issue_ssl && !cert_ok {
        Some(spawn_ssl_task(&state, domain.clone(), route).await)
    } else {
        None
    };

    Ok(Json(CreateSiteResponse { ok: true, task_id }))
}

pub async fn delete(
    State(state): State<AppState>,
    Path(domain): Path<String>,
) -> AgentResult<Json<OkResponse>> {
    if !is_safe_subdomain(&domain) {
        return Err(AgentError::Invalid("invalid domain".into()));
    }
    refuse_while_issuing(&state, &domain)?;
    // Hard refuse: never delete the panel-host vhost from here — it would lock
    // the operator out. The UI already hides the Remove button for panel-host
    // rows, this is defense in depth.
    let live_host = crate::handlers::system::current_panel_host().await;
    if (!state.config.panel_host.is_empty() && domain == state.config.panel_host)
        || (!live_host.is_empty() && domain.eq_ignore_ascii_case(&live_host))
    {
        return Err(AgentError::Conflict(
            "refusing to delete the panel host vhost".into(),
        ));
    }
    // The vhost on disk can live under one of two filename conventions:
    //   1. `site-<domain>` — panel-managed sites created via POST /sites.
    //   2. `<domain>`      — external vhosts (manual deploys, mail.<tenant>
    //      ACME helpers, etc.) surfaced by GET /sites with kind="external".
    // Try both so "Remove from agent" actually removes external vhosts instead
    // of silently no-op'ing.
    let (panel_path, panel_link) = vhost_paths(&domain);
    let external_path = PathBuf::from(format!("/etc/nginx/sites-available/{domain}"));
    let external_link = PathBuf::from(format!("/etc/nginx/sites-enabled/{domain}"));
    fs_atomic::remove_if_exists(&panel_link).await?;
    fs_atomic::remove_if_exists(&panel_path).await?;
    fs_atomic::remove_if_exists(&external_link).await?;
    fs_atomic::remove_if_exists(&external_path).await?;
    // Redirect rules ride on the vhost; drop their include file with it.
    let _ = crate::handlers::redirects::remove_domain_files(&domain).await;
    let _ = nginx_handler::test_config().await;
    let _ = exec::run("systemctl", ["reload", "nginx"]).await;
    // Symmetric cleanup: if the panel auto-created A/AAAA for this name in a
    // managed parent zone (see ensure_subdomain_records), remove them so the
    // record disappears from public DNS too. Best-effort — a stale A row is
    // harmless if the cleanup fails and the operator retries.
    let _ = crate::handlers::dns::cleanup_subdomain_records(&state, &domain).await;
    Ok(Json(OkResponse::default()))
}

pub async fn list(State(_state): State<AppState>) -> AgentResult<Json<Vec<SiteListEntry>>> {
    // Surface every nginx vhost that resolves to a valid FQDN so the panel's
    // Domains page can group them all (including manually-deployed ones, panel
    // self-host vhosts, and app vhosts) under their apex. The `kind` field
    // tells the UI how to render actions — only "site" and "external" should
    // get the delete/remove control.
    //
    // Read from sites-enabled/ (not sites-available/) because:
    //   1) It's the actual serving set; disabled-but-still-on-disk vhosts and
    //      .bak/.orig backup files won't be symlinked here.
    //   2) Patcher scripts that write `<file>.bak-YYYYMMDD-HHMMSS` next to the
    //      live vhost leave a name that happens to pass is_safe_subdomain(),
    //      so filtering by sites-enabled is the correct way to ignore them.
    // Live from .env: the agent's config keeps the value it started with, so
    // after a hostname change the panel host lost its row (shown as a plain
    // site, or not at all) until the agent restarted.
    let panel_host = crate::handlers::system::current_panel_host().await;
    let mut out = Vec::new();
    let mut rd = match tokio::fs::read_dir("/etc/nginx/sites-enabled").await {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Json(out)),
        Err(e) => return Err(AgentError::Io(e)),
    };
    while let Some(entry) = rd.next_entry().await.map_err(AgentError::Io)? {
        let name = entry.file_name();
        let s = name.to_string_lossy();
        // Skip Ubuntu default, hidden files, and any leftover backup-style names
        // (defensive — these shouldn't be symlinked into sites-enabled in the
        // first place). `vps-panel.conf` is NOT skipped here anymore — it gets
        // a dedicated special case below so the panel host shows up in the
        // listing with kind="panel-host" (its filename isn't a valid FQDN so it
        // would otherwise fall through `is_safe_subdomain` and be dropped).
        if s == "default" || s.starts_with('.') {
            continue;
        }
        if s.contains(".bak") || s.contains(".orig") || s.contains(".old") || s.ends_with('~') {
            continue;
        }

        let (domain, default_kind) = if s == "vps-panel.conf" {
            // Install-script-managed panel vhost. The filename is a sentinel,
            // not an FQDN — map it to PANEL_HOST and tag as panel-host. In
            // IP-only mode panel_host is empty or a bare IP (no dots); skip in
            // either case because there's no FQDN row to render.
            if !is_safe_subdomain(&panel_host) {
                continue;
            }
            (panel_host.clone(), "panel-host")
        } else if let Some(d) = s.strip_prefix("site-") {
            (d.to_string(), "site")
        } else if is_safe_subdomain(&s) {
            (s.to_string(), "external")
        } else {
            continue;
        };
        if !is_safe_subdomain(&domain) {
            continue;
        }

        // Classify by marker comment in the vhost file. entry.path() points
        // at the symlink in sites-enabled; tokio::fs::read_to_string follows
        // it automatically. The "panel-host" classification additionally
        // requires the domain to match the configured PANEL_HOST — a stale or
        // copy-pasted `# vps-panel-host:` marker on an unrelated vhost (e.g. a
        // Quick Install service) should not falsely claim panel-host status.
        let kind = match read_first_marker(&entry.path()).await {
            Some(m) if m.starts_with("# vps-app:") => "app",
            Some(m) if m.starts_with("# vps-site:") => "site",
            Some(m) if m.starts_with("# vps-panel-host:") && domain == panel_host => {
                "panel-host"
            }
            _ => default_kind,
        };

        out.push(SiteListEntry {
            domain,
            kind: kind.to_string(),
        });
    }
    out.sort_by(|a, b| a.domain.cmp(&b.domain));
    Ok(Json(out))
}

async fn read_first_marker(path: &std::path::Path) -> Option<String> {
    let content = tokio::fs::read_to_string(path).await.ok()?;
    let first = content.lines().next()?.trim();
    if first.starts_with("# vps-") || first.starts_with("# patched-by-vps-panel") {
        Some(first.to_string())
    } else {
        None
    }
}

pub async fn status(Path(domain): Path<String>) -> AgentResult<Json<SiteStatus>> {
    if !is_safe_subdomain(&domain) {
        return Err(AgentError::Invalid("invalid domain".into()));
    }
    let [site_file, external_file] = files_removed_by_delete(&domain);
    let www_served_by = other_vhost_serving(
        &format!("www.{domain}"),
        &[site_file.as_str(), external_file.as_str()],
    )
    .await;
    let force_www_parent = force_www_parent_serving(&domain).await.map(str::to_string);
    let (cert_path, covered_by) = match existing_cert_paths(&domain) {
        Some((cert_path, _)) => (cert_path, None),
        None => {
            // No cert of its own. mail.<d> and www.<d> normally ride on the
            // parent's cert as SANs - report that cert so the UI shows a real
            // expiry instead of a dash for a name that is in fact served.
            let Some((parent_cert, parent)) = covering_parent_cert(&domain).await else {
                return Ok(Json(SiteStatus {
                    cert_exists: false,
                    cert_not_after: None,
                    days_remaining: None,
                    covered_by: None,
                    www_served_by,
                    force_www_parent,
                }));
            };
            (parent_cert, Some(parent))
        }
    };
    let p = std::path::Path::new(&cert_path);
    let (not_after, days) = match crate::handlers::ssl::cert_expiry(p).await {
        Some((s, d)) => (Some(s), Some(d)),
        None => (None, None),
    };
    Ok(Json(SiteStatus {
        cert_exists: true,
        cert_not_after: not_after,
        days_remaining: days,
        covered_by,
        www_served_by,
        force_www_parent,
    }))
}

/// For `label.parent`, the parent's cert (panel or certbot) if it lists the
/// full name as a SAN. Returns (fullchain path, parent domain).
async fn covering_parent_cert(domain: &str) -> Option<(String, String)> {
    let (_, parent) = domain.split_once('.')?;
    if !is_safe_subdomain(parent) {
        return None;
    }
    let (full, _) = existing_cert_paths(parent)?;
    if crate::handlers::ssl::cert_covers(std::path::Path::new(&full), domain).await {
        Some((full, parent.to_string()))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_route, files_removed_by_delete, https_after_update, is_other_vhost_file,
        ssl_required_names, vhost_serves_https, vhost_serves_name, www_parent,
    };
    use crate::nginx_template::{SiteVhostTemplate, ACME_WEBROOT};
    use askama::Template;

    fn site_vhost(force_www_redirect: bool, ssl_enabled: bool) -> String {
        SiteVhostTemplate {
            domain: "example.com",
            mode: "port",
            upstream_port: Some(3000),
            folder_path: None,
            force_www_redirect,
            ssl_enabled,
            www_in_cert: ssl_enabled,
            cert_path: "/c/fullchain.pem",
            key_path: "/c/privkey.pem",
            acme_webroot: ACME_WEBROOT,
        }
        .render()
        .unwrap()
    }

    #[test]
    fn a_force_www_vhost_is_seen_to_answer_the_www_name() {
        // The block for www.<d> is there on 80 whether or not the site has
        // HTTPS, so either way a separate www.<d> site would clash with it.
        for ssl in [false, true] {
            assert!(vhost_serves_name(&site_vhost(true, ssl), "www.example.com"), "{ssl}");
            assert!(!vhost_serves_name(&site_vhost(false, ssl), "www.example.com"), "{ssl}");
            assert!(vhost_serves_name(&site_vhost(false, ssl), "example.com"), "{ssl}");
        }
    }

    #[test]
    fn server_name_lines_are_matched_by_exact_name() {
        let external = "server {\n\tserver_name  example.com WWW.example.com; # old\n}\n";
        assert!(vhost_serves_name(external, "www.example.com"));
        assert!(vhost_serves_name(external, "example.com"));
        // A wildcard loses to an exact name in nginx, so it is no clash.
        assert!(!vhost_serves_name("server_name *.example.com;\n", "www.example.com"));
        assert!(!vhost_serves_name("# server_name www.example.com;\n", "www.example.com"));
        assert!(!vhost_serves_name("server_names_hash_bucket_size 128;\n", "128"));
        assert!(!vhost_serves_name("server_name www.example.com.au;\n", "www.example.com"));
    }

    #[test]
    fn only_a_www_name_has_a_parent_to_check() {
        assert_eq!(www_parent("www.example.com"), Some("example.com"));
        assert_eq!(www_parent("www.app.example.com"), Some("app.example.com"));
        assert_eq!(www_parent("example.com"), None);
        assert_eq!(www_parent("app.example.com"), None);
        // A bare TLD is no site of the panel's.
        assert_eq!(www_parent("www.com"), None);
    }

    #[test]
    fn the_replace_check_leaves_out_the_files_the_delete_removes() {
        // Replace deletes both of these before the create, so a www.<d>
        // block in either is gone by then and must not refuse the replace.
        let [site_file, external_file] = files_removed_by_delete("example.com");
        assert_eq!(site_file, "site-example.com");
        assert_eq!(external_file, "example.com");
        let own = [site_file.as_str(), external_file.as_str()];
        assert!(!is_other_vhost_file("site-example.com", &own));
        assert!(!is_other_vhost_file("example.com", &own));
        assert!(is_other_vhost_file("site-www.example.com", &own));
        assert!(is_other_vhost_file("www.example.com", &own));
        assert!(is_other_vhost_file("example.com.bak", &own));
        // nginx's include glob skips dot files, so they serve nothing.
        assert!(!is_other_vhost_file(".example.com.swp", &own));
        // A create only leaves out its own site file.
        assert!(is_other_vhost_file("example.com", &["site-example.com"]));
    }

    #[test]
    fn a_site_cannot_proxy_to_nginx_itself() {
        for port in [80, 443] {
            let err = build_route("port", Some(port), None, false).err().unwrap().to_string();
            assert!(err.contains("is nginx itself"), "{err}");
        }
        for port in [81, 3000, 8080, 8443, 65535] {
            assert!(build_route("port", Some(port), None, false).is_ok(), "{port}");
        }
        assert!(build_route("port", Some(0), None, false).is_err());
        assert!(build_route("port", None, None, false).is_err());
    }

    #[test]
    fn an_update_without_issue_ssl_keeps_the_https_the_site_has() {
        // (issue_ssl, remove_ssl, cert covers the site, cert valid, serves https)
        // Routing saved with "Auto-issue SSL" off on a site that is on HTTPS.
        assert!(https_after_update(false, false, true, true, true));
        // Asked for explicitly.
        assert!(!https_after_update(false, true, true, true, true));
        // An HTTP-only site is not switched to a certificate left on disk.
        assert!(!https_after_update(false, false, true, true, false));
        // An expired certificate is not kept on the caller's behalf.
        assert!(!https_after_update(false, false, true, false, true));
        // With issue_ssl as before: any certificate that covers the site.
        assert!(https_after_update(true, false, true, true, false));
        assert!(https_after_update(true, false, true, false, true));
        // No certificate for the name: nothing to serve HTTPS with.
        assert!(!https_after_update(true, false, false, false, true));
        assert!(!https_after_update(false, false, false, true, true));
    }

    #[test]
    fn finds_the_443_block_of_a_vhost() {
        let https = "server {\n    listen 80;\n}\nserver {\n    listen 443 ssl http2;\n    listen [::]:443 ssl http2;\n}\n";
        assert!(vhost_serves_https(https));
        assert!(vhost_serves_https("server {\n\tlisten\t[::]:443 ssl;\n}\n"));
        let http = "server {\n    listen 80;\n    listen [::]:80;\n    # listen 443 ssl;\n    proxy_pass http://127.0.0.1:4430;\n}\n";
        assert!(!vhost_serves_https(http));
        assert!(!vhost_serves_https(""));
    }

    #[test]
    fn the_certificate_is_asked_for_www_whenever_the_vhost_gets_a_www_block() {
        for domain in ["example.com", "example.co.uk", "app.example.com"] {
            assert_eq!(
                ssl_required_names(domain, true),
                vec![domain.to_string(), format!("www.{domain}")]
            );
            assert_eq!(ssl_required_names(domain, false), vec![domain.to_string()]);
        }
    }
}
