use crate::error::{AgentError, AgentResult};
use crate::exec;
use crate::fs_atomic;
use crate::ipc::tasks::TaskHandle;
use crate::ipc::TaskAccepted;
use crate::nginx_template::{PanelVhostTemplate, ACME_WEBROOT, PEER_API_LOCATION};
use crate::state::AppState;
use askama::Template;
use axum::{
    extract::{Path, Query, State},
    Json,
};
use futures::future::join_all;
use serde::{Deserialize, Serialize};
use std::process::Stdio;
use tokio::process::Command;

#[derive(Serialize)]
pub struct ServiceState {
    pub name: &'static str,
    pub package: &'static str,
    pub unit: &'static str,
    pub installed: bool,
    pub active: bool,
    pub enabled: bool,
    pub quick_install_slug: Option<&'static str>,
}

struct ServiceSpec {
    name: &'static str,
    package: &'static str,
    unit: &'static str,
    quick_install_slug: Option<&'static str>,
}

const CORE_SERVICES: &[ServiceSpec] = &[
    ServiceSpec {
        name: "nginx",
        package: "nginx",
        unit: "nginx",
        quick_install_slug: Some("nginx"),
    },
    ServiceSpec {
        name: "mariadb",
        package: "mariadb-server",
        unit: "mariadb",
        quick_install_slug: Some("mariadb"),
    },
    ServiceSpec {
        name: "postfix",
        package: "postfix",
        unit: "postfix",
        quick_install_slug: None,
    },
    ServiceSpec {
        name: "dovecot",
        package: "dovecot-core",
        unit: "dovecot",
        quick_install_slug: None,
    },
    ServiceSpec {
        name: "pdns",
        package: "pdns-server",
        unit: "pdns",
        quick_install_slug: None,
    },
    ServiceSpec {
        name: "rspamd",
        package: "rspamd",
        unit: "rspamd",
        quick_install_slug: None,
    },
    ServiceSpec {
        name: "fail2ban",
        package: "fail2ban",
        unit: "fail2ban",
        quick_install_slug: None,
    },
    ServiceSpec {
        name: "vps-panel-agent",
        package: "",
        unit: "vps-panel-agent",
        quick_install_slug: None,
    },
    ServiceSpec {
        name: "vps-panel",
        package: "",
        unit: "vps-panel",
        quick_install_slug: None,
    },
];

async fn dpkg_installed(pkg: &str) -> bool {
    if pkg.is_empty() {
        return true;
    }
    Command::new("dpkg-query")
        .args(["-W", "-f=${Status}", pkg])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await
        .map(|o| {
            o.status.success()
                && String::from_utf8_lossy(&o.stdout).contains("install ok installed")
        })
        .unwrap_or(false)
}

async fn systemd_is(verb: &str, unit: &str) -> bool {
    let out = Command::new("systemctl")
        .args([verb, unit])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await;
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim() == verb_to_state(verb),
        Err(_) => false,
    }
}

fn verb_to_state(verb: &str) -> &'static str {
    match verb {
        "is-active" => "active",
        "is-enabled" => "enabled",
        _ => "",
    }
}

pub async fn services_status() -> AgentResult<Json<Vec<ServiceState>>> {
    let lookups = CORE_SERVICES.iter().map(|spec| async move {
        let (installed, active, enabled) = tokio::join!(
            dpkg_installed(spec.package),
            systemd_is("is-active", spec.unit),
            systemd_is("is-enabled", spec.unit),
        );
        ServiceState {
            name: spec.name,
            package: spec.package,
            unit: spec.unit,
            installed,
            active,
            enabled,
            quick_install_slug: spec.quick_install_slug,
        }
    });
    let rows = join_all(lookups).await;
    Ok(Json(rows))
}

const ENV_PATH: &str = "/etc/vps-panel/.env";
const DOVECOT_CONF: &str = "/etc/dovecot/conf.d/99-vps-panel.conf";
const NGINX_VHOST: &str = "/etc/nginx/sites-available/vps-panel.conf";

#[derive(Deserialize)]
pub struct PanelHostReq {
    /// Empty string switches the panel back to IP-only mode (nginx becomes a
    /// default_server, cookies become non-Secure). Any non-empty value must
    /// be a real FQDN — Postfix refuses numeric myhostname and the panel's
    /// Origin checks expect a real name.
    pub host: String,
    /// When `host` is a subdomain (3+ labels) whose apex isn't yet managed in
    /// PowerDNS, also create a fresh top-level zone for the apex with the
    /// standard SOA/NS/A/AAAA/CAA seed records, then add an A record for the
    /// label inside it. Default true. Ignored in IP-only mode.
    #[serde(default = "default_true")]
    pub create_parent_zone: bool,
    /// After DNS is in place, run an ACME pre-flight against 1.1.1.1 and (if
    /// it passes) issue a Let's Encrypt cert for `host`. Only on success do
    /// we flip `COOKIE_SECURE=1` and re-render the nginx vhost with the 443
    /// server block — otherwise the panel stays HTTP-only so the operator
    /// can still log in. Default true. Ignored in IP-only mode.
    #[serde(default = "default_true")]
    pub issue_ssl: bool,
}

fn default_true() -> bool {
    true
}

/// Re-points the panel at a new hostname (or back to IP-only mode) as a
/// `TaskTracker` job so the UI can stream live progress. Returns a task id
/// immediately; the work runs in a `tokio::spawn` background task that
/// orchestrates env / postfix / dovecot / nginx rewrites, PowerDNS zone +
/// record creation (with optional parent-zone bootstrap), Let's Encrypt
/// issuance, and a deferred `systemctl restart vps-panel.service`.
///
/// `COOKIE_SECURE` is written as `0` up front and only flipped to `1` if a
/// cert lands — this is the entire point of the rewrite, and the reason the
/// old synchronous handler kept locking operators out over plain HTTP after
/// rename.
pub async fn set_panel_host(
    State(state): State<AppState>,
    Json(req): Json<PanelHostReq>,
) -> AgentResult<Json<TaskAccepted>> {
    let host = req.host.trim().to_lowercase();
    if !host.is_empty() {
        // Fail fast on bad input — invalid FQDNs should be a 400 from the
        // handler, not a "task failed" the UI has to discover via SSE.
        validate_fqdn(&host)?;
        // An app's domain cannot become the panel's: taking it over would
        // take the app offline. (A plain site is taken over in the task.)
        let app: Option<String> = state
            .panel_db
            .with(|conn| {
                conn.query_row(
                    "SELECT name FROM apps WHERE subdomain = ?1",
                    rusqlite::params![&host],
                    |r| r.get(0),
                )
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })
            })
            .unwrap_or(None);
        if let Some(app) = app {
            return Err(AgentError::Conflict(format!(
                "{host} is the domain of app \"{app}\"; move the app to another domain first"
            )));
        }
    }

    let kind_suffix = if host.is_empty() { "ip-only" } else { host.as_str() };
    let task = state.tasks.create(&format!("system:panel-host:{kind_suffix}"));
    let task_id = task.id.clone();
    let state_clone = state.clone();
    let cfg = state.config.clone();
    let panel_db = state.panel_db.clone();
    let create_parent_zone = req.create_parent_zone;
    let issue_ssl_opt = req.issue_ssl;

    tokio::spawn(async move {
        task.start();
        match run_panel_host_task(
            &state_clone,
            &cfg,
            &panel_db,
            host,
            create_parent_zone,
            issue_ssl_opt,
            &task,
        )
        .await
        {
            Ok(()) => task.finish(true),
            Err(e) => {
                task.log(format!("error: {e}"));
                task.finish(false);
            }
        }
    });

    Ok(Json(TaskAccepted { task_id }))
}

async fn run_panel_host_task(
    state: &AppState,
    cfg: &crate::config::Config,
    panel_db: &crate::db::PanelDb,
    host: String,
    create_parent_zone: bool,
    issue_ssl_opt: bool,
    task: &TaskHandle,
) -> AgentResult<()> {
    let ip_only = host.is_empty();
    if ip_only {
        task.log("switching to IP-only mode (no FQDN)".to_string());
    } else {
        task.log(format!("setting panel hostname to {host}"));
    }

    let (mail_host, server_name, default_server, env_host) = if ip_only {
        let mail = system_mail_host().await;
        let ip = crate::server_ip::ipv4_on_interfaces()
            .await
            .unwrap_or_else(|| "127.0.0.1".to_string());
        (mail, "_".to_string(), true, ip)
    } else {
        (
            host.clone(),
            host.clone(),
            !other_default_server().await,
            host.clone(),
        )
    };

    // 1. .env: write COOKIE_SECURE=0 up front. We only flip it back to 1 if
    //    SSL succeeds below — that's the whole footgun-fix.
    task.log("rewriting /etc/vps-panel/.env (COOKIE_SECURE=0)".to_string());
    let env_after = rewrite_env(&env_host, "0").await?;
    fs_atomic::write(std::path::Path::new(ENV_PATH), env_after.as_bytes(), 0o640).await?;

    // 2. Postfix myhostname.
    task.log(format!("setting Postfix myhostname={mail_host}"));
    exec::run("postconf", ["-e", &format!("myhostname = {mail_host}")]).await?;

    // 3. Dovecot postmaster_address.
    task.log(format!(
        "setting Dovecot postmaster_address=postmaster@{mail_host}"
    ));
    let dovecot = tokio::fs::read_to_string(DOVECOT_CONF).await?;
    let dovecot_after = replace_postmaster(&dovecot, &mail_host);
    if dovecot_after != dovecot {
        fs_atomic::write(
            std::path::Path::new(DOVECOT_CONF),
            dovecot_after.as_bytes(),
            0o644,
        )
        .await?;
    }

    // 4. nginx vhost (HTTP-only for now; we re-render with SSL after issuance).
    if !ip_only {
        take_over_site(panel_db, &host, task).await?;
    }
    task.log("rewriting /etc/nginx/sites-available/vps-panel.conf".to_string());
    crate::handlers::panel_access::ensure_file().await?;
    let nginx_body = render_panel_vhost(&server_name, default_server, None)?;
    fs_atomic::write(std::path::Path::new(NGINX_VHOST), nginx_body.as_bytes(), 0o644).await?;

    // 5. Reload services. nginx -t first so a broken template doesn't take
    //    nginx down — we want the failure surfaced before we touch postfix.
    task.log("nginx -t && systemctl reload nginx".to_string());
    exec::run("nginx", ["-t"]).await?;
    let _ = exec::run("systemctl", ["reload", "nginx"]).await;
    task.log("restarting postfix and dovecot".to_string());
    let _ = exec::run("systemctl", ["restart", "postfix"]).await;
    let _ = exec::run("systemctl", ["restart", "dovecot"]).await;

    // 6. DNS — skipped for IP-only mode. The name must end up answering with
    //    this server and nothing else (dns::point_name_here), whether it is
    //    the zone's apex or a label in it.
    if !ip_only {
        task.log("checking managed PowerDNS zones".to_string());
        let zone_names = crate::handlers::dns::list_zone_names(state)
            .await
            .unwrap_or_default();
        let labels: Vec<&str> = host.split('.').collect();
        let zone = if zone_names.iter().any(|z| z == &host) {
            Some(host.clone())
        } else if let Some(parent) = find_parent_managed(&host, &zone_names) {
            Some(parent)
        } else if labels.len() >= 3 && create_parent_zone {
            // Subdomain, apex unmanaged → create the apex zone first, then
            // add the label inside it. This is the "treat it like creating
            // the main domain" path the user asked for.
            let parent_name = labels[1..].join(".");
            task.log(format!("creating parent zone {parent_name}"));
            let parent_zone = crate::handlers::dns::create_zone_inner(
                state,
                crate::handlers::dns::CreateZone {
                    name: parent_name.clone(),
                    seed_ip: None,
                },
            )
            .await?;
            task.log(format!(
                "created zone {} with {} records",
                parent_zone.name, parent_zone.records
            ));
            Some(parent_name)
        } else if labels.len() >= 3 {
            task.log(format!(
                "no managed zone for {host} and creating one is off: DNS left to you"
            ));
            None
        } else {
            task.log(format!("creating PowerDNS zone {host}"));
            let z = crate::handlers::dns::create_zone_inner(
                state,
                crate::handlers::dns::CreateZone {
                    name: host.clone(),
                    seed_ip: None,
                },
            )
            .await?;
            task.log(format!("created zone {} with {} records", z.name, z.records));
            Some(host.clone())
        };
        if let Some(zone) = zone {
            let ipv4 = match crate::server_ip::ipv4_on_interfaces().await {
                Some(ip) => Some(ip),
                None => crate::server_ip::resolve_ipv4(None, &state.config.panel_host).await,
            };
            match ipv4 {
                None => task.log(
                    "warning: could not work out this server's IPv4 - DNS for the name left as is"
                        .to_string(),
                ),
                Some(ip) => {
                    let ipv6s = server_ipv6s().await;
                    for line in
                        crate::handlers::dns::point_name_here(state, &zone, &host, &ip, &ipv6s).await?
                    {
                        task.log(line);
                    }
                }
            }
        }
    }

    // 7. SSL — best-effort. Failure leaves COOKIE_SECURE=0 so the operator
    //    can still log in over HTTP at the new hostname.
    let mut ssl_ok = false;
    if !ip_only && issue_ssl_opt {
        task.log(format!("verifying public DNS for {host}"));
        match crate::handlers::dns::verify_public_dns(state, &host).await {
            Ok(()) => {
                task.log("public DNS check passed".to_string());
                let acme_email = crate::handlers::ssl::resolve_acme_email(panel_db, cfg);
                task.log(format!(
                    "requesting Let's Encrypt cert for {host} ({acme_email})"
                ));
                match crate::handlers::ssl::issue_for_names(
                    cfg,
                    &acme_email,
                    &host,
                    &[host.clone()],
                    task,
                )
                .await
                {
                    Ok(()) => {
                        task.log("certificate issued".to_string());
                        ssl_ok = true;
                    }
                    Err(e) => {
                        task.log(format!(
                            "SSL issuance failed: {e} — keeping COOKIE_SECURE=0 so HTTP login still works"
                        ));
                    }
                }
            }
            Err(e) => {
                task.log(format!(
                    "skipping SSL: {e} — keeping COOKIE_SECURE=0 so HTTP login still works"
                ));
            }
        }
    }

    // 8. SSL post-processing. If a cert landed, re-render the vhost so nginx
    //    actually serves 443, point postfix/dovecot at the new cert, and flip
    //    COOKIE_SECURE=1.
    if ssl_ok {
        task.log("re-rendering nginx vhost with SSL enabled".to_string());
        let cert_path = format!("/etc/vps-panel/certs/{host}/fullchain.pem");
        let key_path = format!("/etc/vps-panel/certs/{host}/privkey.pem");
        crate::handlers::panel_access::ensure_file().await?;
        let body = render_panel_vhost(
            &host,
            !other_default_server().await,
            Some((&cert_path, &key_path)),
        )?;
        fs_atomic::write(std::path::Path::new(NGINX_VHOST), body.as_bytes(), 0o644).await?;

        task.log("nginx -t && systemctl reload nginx (SSL)".to_string());
        exec::run("nginx", ["-t"]).await?;
        let _ = exec::run("systemctl", ["reload", "nginx"]).await;

        task.log("rewiring postfix/dovecot to use the new cert".to_string());
        let cert_dir = std::path::PathBuf::from(format!("/etc/vps-panel/certs/{host}"));
        if let Err(e) = crate::handlers::ssl::apply_mail_cert(&host, &cert_dir).await {
            task.log(format!("mail cert update warning: {e}"));
        }

        task.log("flipping COOKIE_SECURE=1".to_string());
        let env_after = rewrite_env(&host, "1").await?;
        fs_atomic::write(std::path::Path::new(ENV_PATH), env_after.as_bytes(), 0o640).await?;
    }

    // 9. Schedule the panel restart. 5s instead of 3s so the SSE `Done` event
    //    flushes through the panel's proxy before the panel goes down.
    task.log("scheduling panel restart in 5s".to_string());
    let _ = exec::run(
        "systemd-run",
        [
            "--on-active=5",
            "--unit=vps-panel-restart-after-rename.timer",
            "--",
            "systemctl",
            "restart",
            "vps-panel.service",
        ],
    )
    .await;

    task.log("done".to_string());
    Ok(())
}

/// Right-to-left label walk: return the first ancestor of `host` that's in
/// `zone_names`, or None. Same semantics as `_dns_ensure.ts::findParentManagedZone`
/// on the panel side.
fn find_parent_managed(host: &str, zone_names: &[String]) -> Option<String> {
    let labels: Vec<&str> = host.split('.').collect();
    for i in 1..labels.len() {
        let candidate = labels[i..].join(".");
        if zone_names.iter().any(|z| z == &candidate) {
            return Some(candidate);
        }
    }
    None
}

/// The name was a site under Domains: remove that site so the panel serves
/// it. nginx loads `site-*` before vps-panel.conf, so the site kept
/// answering for the name (an empty folder site gave 403). The operator chose
/// this name for the panel, so it takes over; DNS records and certificates
/// are left alone (the handler already refused an app's domain).
async fn take_over_site(
    panel_db: &crate::db::PanelDb,
    host: &str,
    task: &TaskHandle,
) -> AgentResult<()> {
    let available = std::path::PathBuf::from(format!("/etc/nginx/sites-available/site-{host}"));
    let enabled = std::path::PathBuf::from(format!("/etc/nginx/sites-enabled/site-{host}"));
    let had_vhost = tokio::fs::symlink_metadata(&available).await.is_ok()
        || tokio::fs::symlink_metadata(&enabled).await.is_ok();
    fs_atomic::remove_if_exists(&enabled).await?;
    fs_atomic::remove_if_exists(&available).await?;
    let _ = crate::handlers::redirects::remove_domain_files(host).await;
    let rows = panel_db
        .with(|conn| {
            let sites = conn.execute("DELETE FROM sites WHERE domain = ?1", rusqlite::params![host])?;
            let redirects =
                conn.execute("DELETE FROM redirects WHERE domain = ?1", rusqlite::params![host])?;
            Ok(sites + redirects)
        })
        .unwrap_or(0);
    if had_vhost || rows > 0 {
        task.log(format!(
            "{host} was a site under Domains: removed that site so the panel serves the name \
             (DNS records and certificates kept)"
        ));
    }
    Ok(())
}

/// This server's global IPv6 addresses (an AAAA on the panel's name may
/// point at these and nothing else).
async fn server_ipv6s() -> Vec<String> {
    let Ok(out) = exec::run("ip", ["-6", "-o", "addr", "show", "scope", "global"]).await else {
        return Vec::new();
    };
    out.stdout
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            it.find(|t| *t == "inet6")?;
            let addr = it.next()?.split('/').next()?;
            addr.parse::<std::net::Ipv6Addr>().ok().map(|a| a.to_string())
        })
        .collect()
}

fn validate_fqdn(s: &str) -> AgentResult<()> {
    if s.len() > 253 {
        return Err(AgentError::Invalid("hostname too long".into()));
    }
    if !s.contains('.') {
        return Err(AgentError::Invalid(
            "hostname must contain a dot (FQDN required)".into(),
        ));
    }
    if s.parse::<std::net::IpAddr>().is_ok() {
        return Err(AgentError::Invalid(
            "hostname must not be an IP literal — Postfix rejects numeric myhostname".into(),
        ));
    }
    // Conservative label charset: a-z 0-9 - . (also allow uppercase).
    let ok = s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
    if !ok {
        return Err(AgentError::Invalid(
            "hostname contains invalid characters".into(),
        ));
    }
    Ok(())
}

async fn system_mail_host() -> String {
    if let Ok(out) = Command::new("hostname")
        .args(["-f"])
        .stdin(Stdio::null())
        .output()
        .await
    {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty()
                && s.contains('.')
                && s.parse::<std::net::IpAddr>().is_err()
            {
                return s;
            }
        }
    }
    "vps-panel.local".to_string()
}

/// PANEL_HOST as it is in .env now. The agent's own config is read once at
/// start, so it still has the old value after a hostname change until the
/// agent restarts.
pub(crate) async fn current_panel_host() -> String {
    tokio::fs::read_to_string(ENV_PATH)
        .await
        .unwrap_or_default()
        .lines()
        .find_map(|l| l.strip_prefix("PANEL_HOST="))
        .map(|v| v.trim().to_lowercase())
        .unwrap_or_default()
}

async fn rewrite_env(panel_host: &str, cookie_secure: &str) -> AgentResult<String> {
    let raw = tokio::fs::read_to_string(ENV_PATH).await?;
    let mut saw_host = false;
    let mut saw_secure = false;
    let mut out = String::with_capacity(raw.len() + 64);
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("PANEL_HOST=") {
            let _ = rest;
            out.push_str(&format!("PANEL_HOST={panel_host}\n"));
            saw_host = true;
        } else if let Some(rest) = line.strip_prefix("COOKIE_SECURE=") {
            let _ = rest;
            out.push_str(&format!("COOKIE_SECURE={cookie_secure}\n"));
            saw_secure = true;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !saw_host {
        out.push_str(&format!("PANEL_HOST={panel_host}\n"));
    }
    if !saw_secure {
        out.push_str(&format!("COOKIE_SECURE={cookie_secure}\n"));
    }
    Ok(out)
}

fn replace_postmaster(content: &str, host: &str) -> String {
    let mut out = String::with_capacity(content.len());
    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("postmaster_address") {
            let leading = &line[..line.len() - trimmed.len()];
            out.push_str(&format!(
                "{leading}postmaster_address = postmaster@{host}\n"
            ));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// The panel vhost, always from templates/panel_vhost.conf: HTTP only, or
/// with the 443 block when `cert` (fullchain, privkey) is given. A
/// hand-written HTTP-only copy used to be written on every hostname change
/// and left out the upload limits, so until a cert was issued nginx refused
/// any upload over its 1 MB default.
///
/// `default_server` makes the panel answer requests by IP too, so it stays
/// reachable while DNS for a new hostname has not reached this server yet.
/// Without it, saving a hostname before DNS pointed here sent the IP to
/// another vhost's 404 and the domain to nowhere: the operator was locked
/// out.
fn render_panel_vhost(
    server_name: &str,
    default_server: bool,
    cert: Option<(&str, &str)>,
) -> AgentResult<String> {
    PanelVhostTemplate {
        server_name,
        default_server,
        ssl_enabled: cert.is_some(),
        cert_path: cert.map_or("", |c| c.0),
        key_path: cert.map_or("", |c| c.1),
        acme_webroot: ACME_WEBROOT,
        peer_location: PEER_API_LOCATION,
    }
    .render()
    .map_err(|e| AgentError::Other(anyhow::anyhow!("panel vhost render: {e}")))
}

/// Whether an enabled vhost other than the panel's already claims
/// `default_server` on port 80: a second one fails `nginx -t`.
async fn other_default_server() -> bool {
    let panel = std::path::Path::new(NGINX_VHOST);
    let Ok(mut rd) = tokio::fs::read_dir("/etc/nginx/sites-enabled").await else {
        return false;
    };
    while let Ok(Some(entry)) = rd.next_entry().await {
        let path = entry.path();
        let target = tokio::fs::canonicalize(&path).await.unwrap_or_else(|_| path.clone());
        if target == panel || path.file_name() == panel.file_name() {
            continue;
        }
        let Ok(body) = tokio::fs::read_to_string(&path).await else {
            continue;
        };
        if body.lines().map(str::trim_start).any(|l| {
            l.starts_with("listen") && l.contains("default_server") && !l.contains("443")
        }) {
            return true;
        }
    }
    false
}

/// Where the source clone lives on disk. Operator passes their checkout path
/// to install.sh; install.sh writes it into /etc/vps-panel/.env as REPO_DIR;
/// the agent reads it at request time. Default fallback matches the documented
/// convention (`/root/vps-panel-src`) for older installs that didn't have the
/// var written by install.sh.
fn repo_dir() -> String {
    std::env::var("REPO_DIR").unwrap_or_else(|_| "/root/vps-panel-src".to_string())
}

#[derive(Serialize)]
pub struct UpdateCheck {
    pub up_to_date: bool,
    /// How many commits the local clone is behind origin/HEAD.
    pub behind: u32,
    /// How many commits the local clone is AHEAD of origin/HEAD. Non-zero
    /// here usually means the operator made local edits on the VPS — the
    /// update button will still run `git pull --ff-only` which will fail
    /// cleanly in that case instead of silently rewriting their work.
    pub ahead: u32,
    pub current_commit: String,
    pub target_commit: String,
    pub repo_dir: String,
    /// If we couldn't reach the remote (no network, no SSH key, etc.) the
    /// fetch failed and we report it instead of pretending we're up to date.
    pub fetch_error: Option<String>,
}

pub async fn update_check() -> AgentResult<Json<UpdateCheck>> {
    let dir = repo_dir();

    // `git fetch` updates the remote tracking refs without touching the
    // working tree. If the remote is unreachable we still want to report the
    // local HEAD so the UI can show "couldn't check, last known X".
    let fetch_err = fetch_error(&dir).await;

    let current = git_rev_parse(&dir, "HEAD").await.unwrap_or_default();
    let target = git_rev_parse(&dir, "@{u}").await.unwrap_or_default();

    let (ahead, behind) = git_count_diverged(&dir).await.unwrap_or((0, 0));

    Ok(Json(UpdateCheck {
        up_to_date: behind == 0 && fetch_err.is_none(),
        behind,
        ahead,
        current_commit: current,
        target_commit: target,
        repo_dir: dir,
        fetch_error: fetch_err,
    }))
}

async fn git_rev_parse(dir: &str, rev: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["-C", dir, "rev-parse", "--short", rev])
        .stdin(Stdio::null())
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

async fn git_count_diverged(dir: &str) -> Option<(u32, u32)> {
    // `--left-right --count HEAD...@{u}` returns "<ahead>\t<behind>" — ahead
    // = commits local has that remote doesn't, behind = vice versa.
    let out = Command::new("git")
        .args([
            "-C", dir, "rev-list", "--left-right", "--count", "HEAD...@{u}",
        ])
        .stdin(Stdio::null())
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let mut parts = s.split_whitespace();
    let ahead = parts.next().and_then(|x| x.parse().ok()).unwrap_or(0);
    let behind = parts.next().and_then(|x| x.parse().ok()).unwrap_or(0);
    Some((ahead, behind))
}

const PANEL_DEPLOY_KEY: &str = "github_vps_panel";
const ROOT_SSH_CONFIG: &str = "/root/.ssh/config";

async fn fetch_error(dir: &str) -> Option<String> {
    let text = match exec::run("git", ["-C", dir, "fetch", "--quiet"]).await {
        Ok(_) => return None,
        Err(AgentError::Subprocess { stderr, .. }) => stderr.trim().to_string(),
        Err(e) => e.to_string(),
    };
    Some(match source_access_hint(dir, &text).await {
        Some(hint) => format!("{text}\n\n{hint}"),
        None => text,
    })
}

async fn source_access_hint(dir: &str, output: &str) -> Option<String> {
    let origin = exec::run("git", ["-C", dir, "remote", "get-url", "origin"]).await.ok()?;
    let ssh_config = tokio::fs::read_to_string(ROOT_SSH_CONFIG).await.unwrap_or_default();
    publickey_hint(dir, origin.stdout.trim(), &ssh_config, output)
}

fn publickey_hint(dir: &str, origin: &str, ssh_config: &str, output: &str) -> Option<String> {
    if !output.to_lowercase().contains("permission denied (publickey)") {
        return None;
    }
    let SshRemote { user, host, path } = ssh_remote(origin)?;
    let repo = path.strip_suffix(".git").unwrap_or(path);
    let blocks = host_blocks(ssh_config);
    let own = blocks.iter().find(|b| b.is_named(host));
    if own.is_some() || !host.contains('.') {
        let server = own.and_then(|b| b.host_name.as_deref()).unwrap_or("the remote");
        let (key, public) = match own.and_then(|b| b.identity_file.as_deref()) {
            Some(k) => (format!(" ({k})"), format!(" ({k}.pub)")),
            None => (String::new(), String::new()),
        };
        return Some(format!(
            "What this usually means: origin goes through the SSH Host alias {host}, and \
             {server} refused the key behind it{key}: that key is not authorized on {repo}. \
             Add its public key{public} to the repository as a deploy key (read access is \
             enough), or to an account that can read the repository, then try again. To test \
             the key:\n\nssh -T {user}@{host}"
        ));
    }
    let set_url =
        |alias: &str| format!("git -C {dir} remote set-url origin {user}@{alias}:{path}");
    let candidates: Vec<&HostBlock> = blocks
        .iter()
        .filter(|b| b.reaches(host) && b.alias().is_some())
        .collect();
    let deploy_key = candidates
        .iter()
        .find(|b| b.alias() == Some(PANEL_DEPLOY_KEY) || b.key_name() == Some(PANEL_DEPLOY_KEY))
        .or(match candidates.as_slice() {
            [only] => Some(only),
            _ => None,
        });
    let fix = match (deploy_key.and_then(|b| b.alias()), candidates.as_slice()) {
        (Some(alias), _) => format!(
            "The panel's deploy key has the Host alias {alias} in {ROOT_SSH_CONFIG}. Point origin \
             at it, then try again:\n\n{}",
            set_url(alias)
        ),
        (None, []) => format!(
            "No Host block in {ROOT_SSH_CONFIG} leads to {host}. Add one for the panel's deploy \
             key, /root/.ssh/{PANEL_DEPLOY_KEY} (FIRST_INSTALL.md, Installing from a private \
             fork), and point \
             origin at it, then try again:\n\n\
             printf '%s\\n' 'Host {PANEL_DEPLOY_KEY}' '  HostName {host}' '  User git' \
             '  IdentityFile ~/.ssh/{PANEL_DEPLOY_KEY}' '  IdentitiesOnly yes' \
             >> {ROOT_SSH_CONFIG}\n{}",
            set_url(PANEL_DEPLOY_KEY)
        ),
        (None, several) => format!(
            "Point origin at the Host alias whose key can read {repo}, one of {} in \
             {ROOT_SSH_CONFIG}, then try again:\n\n{}",
            several.iter().filter_map(|b| b.alias()).collect::<Vec<_>>().join(", "),
            set_url("<alias>")
        ),
    };
    Some(format!(
        "What this usually means: origin ({origin}) names the host {host} itself, not an SSH \
         Host alias. The panel runs git with its own GIT_SSH_COMMAND, so ssh offers only the \
         keys it finds on its own: default-named ones (/root/.ssh/id_ed25519, id_rsa, ...) and \
         the IdentityFile of a Host block in {ROOT_SSH_CONFIG} whose name matches the host. A \
         core.sshCommand set in the clone is ignored, so a key named only there is never \
         offered. {fix}"
    ))
}

struct SshRemote<'a> {
    user: &'a str,
    host: &'a str,
    path: &'a str,
}

fn ssh_remote(url: &str) -> Option<SshRemote<'_>> {
    let (authority, path) = match url.strip_prefix("ssh://") {
        Some(rest) => {
            let (authority, path) = rest.split_once('/')?;
            (authority.split_once(':').map_or(authority, |(a, _)| a), path)
        }
        None if url.contains("://") => return None,
        None => url.split_once(':').filter(|(a, _)| !a.contains('/'))?,
    };
    let (user, host) = authority.rsplit_once('@').unwrap_or(("git", authority));
    (!host.is_empty() && !path.is_empty()).then_some(SshRemote { user, host, path })
}

#[derive(Default)]
struct HostBlock {
    names: Vec<String>,
    host_name: Option<String>,
    identity_file: Option<String>,
}

impl HostBlock {
    fn alias(&self) -> Option<&str> {
        self.names.iter().map(String::as_str).find(|n| !n.contains(['*', '?', '!']))
    }

    fn is_named(&self, host: &str) -> bool {
        self.names.iter().any(|n| n.eq_ignore_ascii_case(host))
    }

    fn reaches(&self, host: &str) -> bool {
        self.host_name.as_deref().is_some_and(|h| h.eq_ignore_ascii_case(host))
    }

    fn key_name(&self) -> Option<&str> {
        self.identity_file.as_deref().and_then(|f| f.rsplit('/').next())
    }
}

fn host_blocks(ssh_config: &str) -> Vec<HostBlock> {
    let mut blocks = Vec::new();
    let mut current: Option<HostBlock> = None;
    for line in ssh_config.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let split = line.find(|c: char| c.is_whitespace() || c == '=').unwrap_or(line.len());
        let (key, rest) = line.split_at(split);
        let rest = rest.trim_start();
        let value = rest.strip_prefix('=').unwrap_or(rest).trim().trim_matches('"');
        match key.to_ascii_lowercase().as_str() {
            "host" => {
                blocks.extend(current.take());
                let names = value.split_whitespace().map(|n| n.trim_matches('"').to_string());
                current = Some(HostBlock { names: names.collect(), ..HostBlock::default() });
            }
            "match" => blocks.extend(current.take()),
            "hostname" => {
                if let Some(b) = current.as_mut() {
                    b.host_name.get_or_insert_with(|| value.to_string());
                }
            }
            "identityfile" => {
                if let Some(b) = current.as_mut() {
                    b.identity_file.get_or_insert_with(|| value.to_string());
                }
            }
            _ => {}
        }
    }
    blocks.extend(current);
    blocks
}

/// update.sh runs as this transient systemd unit, outside the agent's cgroup:
/// it restarts the agent near the end, which would otherwise kill the script
/// (and everything it printed) along with the agent.
const SELF_UPDATE_UNIT: &str = "vps-panel-self-update";
/// Full update.sh output, truncated at the start of each run.
const SELF_UPDATE_LOG: &str = "/var/log/vps-panel/self-update.log";
/// update.sh's exit code, written by the wrapper once the script ends.
const SELF_UPDATE_EXIT: &str = "/var/lib/vps-panel/self-update.exit";
/// Tail of the update log copied into the task when the next agent settles it.
const SELF_UPDATE_LOG_TAIL_BYTES: usize = 200_000;

async fn self_update_exit_code() -> Option<i32> {
    tokio::fs::read_to_string(SELF_UPDATE_EXIT)
        .await
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

async fn self_update_unit_active() -> bool {
    exec::run(
        "systemctl",
        ["is-active", "--quiet", &format!("{SELF_UPDATE_UNIT}.service")],
    )
    .await
    .is_ok()
}

/// Stream lines appended to the update log since `offset` into the task.
/// Returns the new offset; an unterminated last line waits in `partial`.
async fn pump_update_log(offset: u64, partial: &mut String, task: &TaskHandle) -> u64 {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let Ok(mut file) = tokio::fs::File::open(SELF_UPDATE_LOG).await else {
        return offset;
    };
    if file.seek(std::io::SeekFrom::Start(offset)).await.is_err() {
        return offset;
    }
    let mut buf = Vec::new();
    let Ok(n) = file.read_to_end(&mut buf).await else {
        return offset;
    };
    partial.push_str(&String::from_utf8_lossy(&buf));
    while let Some(pos) = partial.find('\n') {
        let line: String = partial.drain(..=pos).collect();
        task.log(line.trim_end_matches(['\n', '\r']).to_string());
    }
    offset + n as u64
}

/// Settle self-update tasks the previous agent left running. Spawned at every
/// agent start. update.sh restarts the agent before it restarts the panel, so
/// wait for the script to exit, then record its real outcome and full output.
/// Before this every such task was marked succeeded, whatever update.sh did.
pub async fn finalize_self_update(state: AppState) {
    let ids = state.tasks.unfinished_self_updates();
    if ids.is_empty() {
        return;
    }
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15 * 60);
    let code = loop {
        if let Some(code) = self_update_exit_code().await {
            break Some(code);
        }
        if !self_update_unit_active().await {
            // The wrapper writes the exit file right after the script ends.
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            break self_update_exit_code().await;
        }
        if tokio::time::Instant::now() >= deadline {
            break None;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    };
    let log = tokio::fs::read_to_string(SELF_UPDATE_LOG).await.ok();
    let (success, verdict) = match (code, &log) {
        (Some(0), _) => (true, "update.sh finished successfully (exit 0)".to_string()),
        (Some(c), _) => (false, format!("update.sh failed with exit code {c}")),
        // Started by an agent older than this mechanism: update.sh ran as its
        // child and never wrote a log file or an exit code.
        (None, None) => (true, "completed via agent restart (no update log from this run)".to_string()),
        (None, Some(_)) => (
            false,
            "update.sh left no exit code (killed, or still running after 15 minutes)".to_string(),
        ),
    };
    let mut appended = String::from("\n[the agent was restarted by update.sh]");
    if let Some(log) = log {
        let tail = if log.len() > SELF_UPDATE_LOG_TAIL_BYTES {
            let mut cut = log.len() - SELF_UPDATE_LOG_TAIL_BYTES;
            while !log.is_char_boundary(cut) {
                cut += 1;
            }
            format!("[... earlier output in {SELF_UPDATE_LOG}]\n{}", &log[cut..])
        } else {
            log
        };
        appended.push_str(&format!("\n--- update.sh output ({SELF_UPDATE_LOG}) ---\n{tail}"));
    }
    appended.push_str(&format!("\n{verdict}"));
    for id in ids {
        state.tasks.finalize_persisted(&id, success, &appended);
    }
    tracing::info!(success, "settled self-update task after agent restart");
}

pub async fn self_update(State(state): State<AppState>) -> AgentResult<Json<TaskAccepted>> {
    // update.sh restarts the agent, which kills every task it is running (an
    // app build included) and loses their work. Wait for them instead.
    let running = state.tasks.live_kinds();
    if !running.is_empty() {
        return Err(AgentError::Conflict(format!(
            "wait for the running tasks to finish before updating the panel: {}",
            running.join(", ")
        )));
    }
    if self_update_unit_active().await {
        return Err(AgentError::Conflict("a panel update is already running".into()));
    }
    let dir = repo_dir();
    let task = state.tasks.create("system:self-update");
    let task_id = task.id.clone();

    tokio::spawn(async move {
        task.start();
        task.log(format!("source: {dir}"));

        // 1. Pull latest. --ff-only refuses if local has diverged — better to
        //    fail loud than silently rewrite the operator's manual edits.
        let mut output = String::new();
        let mut log_line = |line: &str| {
            task.log(line.to_string());
            output.push_str(line);
            output.push('\n');
        };
        task.log("$ git fetch".to_string());
        let _ = exec::run_streaming("git", ["-C", dir.as_str(), "fetch"], &mut log_line).await;

        task.log("$ git pull --ff-only".to_string());
        let res = exec::run_streaming(
            "git",
            ["-C", dir.as_str(), "pull", "--ff-only"],
            &mut log_line,
        )
        .await;
        let failed = match res {
            Ok(0) => None,
            Ok(code) => Some(format!("git pull exited with code {code}")),
            Err(e) => Some(format!("git pull error: {e}")),
        };
        if let Some(failed) = failed {
            task.log(failed);
            if let Some(hint) = source_access_hint(&dir, &output).await {
                task.log(format!("\n{hint}"));
            }
            task.finish(false);
            return;
        }

        // 2. Run update.sh in its own transient unit, so the agent restart it
        //    performs near the end cannot kill it. Its output goes to
        //    SELF_UPDATE_LOG and its exit code to SELF_UPDATE_EXIT; this task
        //    tails the log until the agent is restarted, and the next agent
        //    settles the task from the exit code (finalize_self_update).
        let script = format!("{dir}/scripts/update.sh");
        let _ = tokio::fs::remove_file(SELF_UPDATE_EXIT).await;
        let _ = fs_atomic::ensure_dir(std::path::Path::new("/var/log/vps-panel"), 0o755).await;
        if let Err(e) = tokio::fs::write(SELF_UPDATE_LOG, b"").await {
            task.log(format!("could not reset {SELF_UPDATE_LOG}: {e}"));
            task.finish(false);
            return;
        }
        task.log(format!(
            "$ bash {script}   (systemd unit {SELF_UPDATE_UNIT}, full output in {SELF_UPDATE_LOG})"
        ));
        task.log(
            "(the agent and panel restart near the end; the log stream cuts off then. \
             Refresh after ~30s: this task is completed with update.sh's real result)"
                .to_string(),
        );
        let unit_arg = format!("--unit={SELF_UPDATE_UNIT}");
        let out_arg = format!("--property=StandardOutput=append:{SELF_UPDATE_LOG}");
        let err_arg = format!("--property=StandardError=append:{SELF_UPDATE_LOG}");
        let started = exec::run(
            "systemd-run",
            [
                unit_arg.as_str(),
                "--collect",
                "--property=EnvironmentFile=/etc/vps-panel/.env",
                "--setenv=HOME=/root",
                out_arg.as_str(),
                err_arg.as_str(),
                "/bin/bash",
                "-c",
                r#"bash "$1"; echo $? > "$2""#,
                SELF_UPDATE_UNIT,
                script.as_str(),
                SELF_UPDATE_EXIT,
            ],
        )
        .await;
        if let Err(e) = started {
            task.log(format!("could not start update.sh: {e}"));
            task.finish(false);
            return;
        }

        let mut offset = 0u64;
        let mut partial = String::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60 * 60);
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            offset = pump_update_log(offset, &mut partial, &task).await;
            let code = match self_update_exit_code().await {
                Some(code) => Some(code),
                None if !self_update_unit_active().await => {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    pump_update_log(offset, &mut partial, &task).await;
                    Some(self_update_exit_code().await.unwrap_or(-1))
                }
                None => None,
            };
            match code {
                Some(0) => {
                    task.log("update.sh finished; panel restarting".to_string());
                    task.finish(true);
                    return;
                }
                Some(-1) => {
                    task.log("update.sh stopped without an exit code".to_string());
                    task.finish(false);
                    return;
                }
                Some(c) => {
                    task.log(format!(
                        "update.sh failed with exit code {c}. The running panel was not replaced \
                         unless the log above shows the swap step"
                    ));
                    task.finish(false);
                    return;
                }
                None if tokio::time::Instant::now() >= deadline => {
                    task.log(format!(
                        "update.sh is still running after an hour; see {SELF_UPDATE_LOG}"
                    ));
                    task.finish(false);
                    return;
                }
                None => {}
            }
        }
    });

    Ok(Json(TaskAccepted { task_id }))
}

#[derive(Deserialize)]
pub struct SetRootPasswordReq {
    pub password: String,
}

#[derive(Serialize)]
pub struct OkResp {
    pub ok: bool,
}

/// Reset the Linux `root` account password by piping `root:<pw>` into
/// `chpasswd`. Lets the operator recover web-console access (Hetzner / OVH
/// console-login auth) without leaving the panel UI. SSH still won't accept
/// passwords because install.sh sets `PasswordAuthentication no` — that's
/// out of scope for this endpoint and noted on the panel side.
pub async fn set_root_password(
    Json(req): Json<SetRootPasswordReq>,
) -> AgentResult<Json<OkResp>> {
    if req.password.len() < 12 {
        return Err(AgentError::Invalid(
            "password must be at least 12 characters".into(),
        ));
    }
    if req.password.contains('\n') || req.password.contains('\r') {
        return Err(AgentError::Invalid(
            "password must not contain newlines".into(),
        ));
    }
    let input = format!("root:{}\n", req.password);
    exec::run_with_input("chpasswd", Vec::<&str>::new(), &input).await?;
    Ok(Json(OkResp { ok: true }))
}

pub async fn apt_upgrade(State(state): State<AppState>) -> AgentResult<Json<TaskAccepted>> {
    let task = state.tasks.create("system:apt-upgrade");
    let task_id = task.id.clone();

    tokio::spawn(async move {
        task.start();
        task.log("starting apt-get update".to_string());

        let envs = [
            ("DEBIAN_FRONTEND", "noninteractive"),
            ("NEEDRESTART_MODE", "a"),
            ("APT_LISTCHANGES_FRONTEND", "none"),
        ];

        for (label, args) in [
            ("apt-get update", vec!["update", "-q"]),
            (
                "apt-get upgrade",
                vec![
                    "upgrade",
                    "-y",
                    "-q",
                    "-o",
                    "Dpkg::Options::=--force-confdef",
                    "-o",
                    "Dpkg::Options::=--force-confold",
                ],
            ),
        ] {
            task.log(format!("$ apt-get {}", args.join(" ")));
            let res = exec::run_streaming_envs(
                "apt-get",
                args.iter().copied(),
                envs.iter().copied(),
                |line: &str| task.log(line.to_string()),
            )
            .await;
            match res {
                Ok(0) => {}
                Ok(code) => {
                    task.log(format!("{label} exited with code {code}"));
                    task.finish(false);
                    return;
                }
                Err(e) => {
                    task.log(format!("{label} error: {e}"));
                    task.finish(false);
                    return;
                }
            }
        }

        task.log("system upgrade completed".to_string());
        task.finish(true);
    });

    Ok(Json(TaskAccepted { task_id }))
}

#[derive(Serialize)]
pub struct SystemUser {
    pub name: String,
    pub uid: u32,
    pub home: String,
    pub shell: String,
    /// True for real operator accounts (UID >= 1000) and `root`. Lets the
    /// panel render two tables: "human" accounts in the manageable section,
    /// service users (www-data, redis, …) elsewhere.
    pub is_human: bool,
    /// True when this user is in the `sudo` group on Debian/Ubuntu. Surfaced
    /// so the /users page can flag administrative accounts at a glance.
    pub sudo: bool,
    /// In RESERVED_SYSTEM_USERS: password reset and delete are refused, so
    /// the page hides them (e.g. `ubuntu`, which has a human-range UID).
    pub reserved: bool,
}

/// List Linux users that are sensible candidates for running a cron job
/// (or, in the future, owning an app folder, an SSH keypair, etc.). Reads
/// /etc/passwd directly rather than running `getent` so it works in any
/// environment.
///
/// Filter:
/// - root (UID 0) — always included
/// - UID >= 33 — covers `www-data` (UID 33 on Debian/Ubuntu), `apps`,
///   `redis`, real human accounts (UID >= 1000), etc. Excludes the
///   classical low-UID pseudo-users (`bin`=2, `daemon`=1, `sys`=3, `mail`=8,
///   …) that no operator ever runs cron jobs as.
/// - Explicit name blocklist for noisy / dangerous service accounts
///   (`nobody`, `systemd-*`, `messagebus`, `sshd`, `_apt`) so they don't
///   clutter the dropdown.
///
/// Sorted with `root` pinned first so it's the obvious default in the UI;
/// the rest alphabetical.
pub async fn list_system_users() -> AgentResult<Json<Vec<SystemUser>>> {
    const BLOCK_NAMES: &[&str] = &[
        "nobody",
        "messagebus",
        "sshd",
        "_apt",
    ];
    let body = tokio::fs::read_to_string("/etc/passwd")
        .await
        .map_err(AgentError::Io)?;
    let sudo_members = read_group_members("sudo").await;
    let mut users: Vec<SystemUser> = body
        .lines()
        .filter_map(|line| {
            // /etc/passwd: name:passwd:uid:gid:gecos:home:shell
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() < 7 {
                return None;
            }
            let name = parts[0];
            let uid: u32 = parts[2].parse().ok()?;
            let home = parts[5];
            let shell = parts[6];
            if name.is_empty() {
                return None;
            }
            if uid != 0 && uid < 33 {
                return None;
            }
            if BLOCK_NAMES.contains(&name) {
                return None;
            }
            if name.starts_with("systemd-") {
                return None;
            }
            let is_human = uid == 0 || uid >= 1000;
            let sudo = sudo_members.iter().any(|m| m == name);
            Some(SystemUser {
                name: name.to_string(),
                uid,
                home: home.to_string(),
                shell: shell.to_string(),
                is_human,
                sudo,
                reserved: RESERVED_SYSTEM_USERS.contains(&name),
            })
        })
        .collect();

    users.sort_by(|a, b| {
        // root first, then alphabetical by name.
        if a.name == "root" && b.name != "root" {
            std::cmp::Ordering::Less
        } else if b.name == "root" && a.name != "root" {
            std::cmp::Ordering::Greater
        } else {
            a.name.cmp(&b.name)
        }
    });
    users.dedup_by(|a, b| a.name == b.name);

    Ok(Json(users))
}

/// Parse `/etc/group` and return the members of `group_name`. Empty vec on
/// I/O error or if the group is absent — callers treat that as "no one is in
/// the group", which is the safe default for the sudo flag in the UI.
async fn read_group_members(group_name: &str) -> Vec<String> {
    let body = match tokio::fs::read_to_string("/etc/group").await {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    for line in body.lines() {
        // /etc/group: name:passwd:gid:members,comma,separated
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() < 4 {
            continue;
        }
        if parts[0] != group_name {
            continue;
        }
        return parts[3]
            .split(',')
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty())
            .collect();
    }
    Vec::new()
}

#[derive(Deserialize)]
pub struct CreateSystemUserReq {
    pub name: String,
    pub password: String,
    /// When true, add the new user to the `sudo` group so they can run
    /// privileged commands. Default false. Off-by-default keeps "regular"
    /// developer accounts safe; the operator can flip it on explicitly.
    #[serde(default)]
    pub sudo: bool,
    /// Login shell. Defaults to `/bin/bash` when empty so SSH lands in a
    /// usable shell. Restricted to the known-safe whitelist below to avoid
    /// the operator typo'ing themselves into a broken account.
    #[serde(default)]
    pub shell: Option<String>,
}

#[derive(Serialize)]
pub struct CreateSystemUserResp {
    pub name: String,
    pub uid: u32,
    pub home: String,
}

/// Reserved Linux account names the panel refuses to (re)create. The shape
/// of `useradd` would refuse most of these anyway with "user already exists",
/// but we want a clearer 400 with a friendly message before we shell out.
const RESERVED_SYSTEM_USERS: &[&str] = &[
    "root",
    "daemon",
    "bin",
    "sys",
    "sync",
    "games",
    "man",
    "lp",
    "mail",
    "news",
    "uucp",
    "proxy",
    "www-data",
    "backup",
    "list",
    "irc",
    "gnats",
    "nobody",
    "_apt",
    "systemd-network",
    "systemd-resolve",
    "systemd-timesync",
    "messagebus",
    "sshd",
    "syslog",
    "uuidd",
    "tcpdump",
    "tss",
    "landscape",
    "fwupd-refresh",
    "pollinate",
    "ubuntu",
    "mysql",
    "mariadb",
    "postgres",
    "redis",
    "pdns",
    "rspamd",
    "dovecot",
    "dovenull",
    "postfix",
    "vps-panel",
];

const ALLOWED_SHELLS: &[&str] = &[
    "/bin/bash",
    "/bin/sh",
    "/bin/zsh",
    "/usr/bin/bash",
    "/usr/bin/zsh",
    "/usr/bin/fish",
    "/usr/sbin/nologin",
    "/bin/false",
];

fn is_safe_unix_name(name: &str) -> bool {
    // useradd's NAME_REGEX on Debian: ^[a-z_][a-z0-9_-]*[$]?$, max 32 chars.
    if name.is_empty() || name.len() > 32 {
        return false;
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !(first.is_ascii_lowercase() || first == '_') {
        return false;
    }
    let rest_ok = chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    rest_ok
}

/// Create a Linux account via `useradd -m`, then pipe `<name>:<pw>\n` into
/// `chpasswd`. If `sudo=true`, add the user to the `sudo` group with
/// `usermod -aG sudo <name>`. The account is created with a home directory
/// in `/home/<name>` and the requested login shell (default `/bin/bash`) so
/// the operator can immediately SSH in as that user once a key is added (or
/// password auth is enabled).
///
/// Idempotency: returns 409 if the user already exists. We do NOT silently
/// reset the password of an existing account — that would mask typos and
/// hide a security event behind a "create" call.
pub async fn create_system_user(
    Json(req): Json<CreateSystemUserReq>,
) -> AgentResult<Json<CreateSystemUserResp>> {
    let name = req.name.trim().to_lowercase();
    if !is_safe_unix_name(&name) {
        return Err(AgentError::Invalid(
            "username must start with a-z or _, contain only a-z, 0-9, _ or -, max 32 chars".into(),
        ));
    }
    if RESERVED_SYSTEM_USERS.contains(&name.as_str()) {
        return Err(AgentError::Invalid(format!(
            "'{name}' is a reserved system account — pick a different name"
        )));
    }
    if req.password.len() < 12 {
        return Err(AgentError::Invalid(
            "password must be at least 12 characters".into(),
        ));
    }
    if req.password.contains('\n') || req.password.contains('\r') {
        return Err(AgentError::Invalid(
            "password must not contain newlines".into(),
        ));
    }
    if req.password.contains(':') {
        // chpasswd uses ':' as the field separator. Letting it through would
        // shift the rest of the password into chpasswd's "newpw" slot.
        return Err(AgentError::Invalid(
            "password must not contain a colon".into(),
        ));
    }

    let shell = req.shell.as_deref().unwrap_or("").trim();
    let shell = if shell.is_empty() { "/bin/bash" } else { shell };
    if !ALLOWED_SHELLS.contains(&shell) {
        return Err(AgentError::Invalid(format!(
            "shell '{shell}' is not in the allowed list"
        )));
    }

    // Refuse to clobber an existing account. `getent passwd <name>` exits 0
    // when the user exists, 2 when it doesn't.
    let exists = Command::new("getent")
        .args(["passwd", &name])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false);
    if exists {
        return Err(AgentError::Conflict(format!(
            "user '{name}' already exists"
        )));
    }

    // Pre-flight: confirm the agent can actually write under /home. If the
    // systemd unit hardening (ProtectHome=read-only) hasn't been "punched
    // through" by ReadWritePaths=/home, useradd would crash at step 2 with
    // exit 12 ("cannot create directory") AFTER it has already inserted the
    // /etc/passwd entry — leaving a zombie user behind. Catch that upfront
    // and return a clear, actionable error pointing at the deploy fix.
    let probe = std::path::Path::new("/home").join(format!(".vps-panel-probe-{}", std::process::id()));
    match tokio::fs::write(&probe, b"ok").await {
        Ok(()) => {
            let _ = tokio::fs::remove_file(&probe).await;
        }
        Err(e) => {
            return Err(AgentError::Other(anyhow::anyhow!(
                "/home is not writable from the agent ({e}). The systemd \
                 unit needs `ReadWritePaths=/home` for useradd -m to work \
                 under ProtectHome=read-only. Run `./scripts/update.sh` on \
                 the host, or manually edit /etc/systemd/system/vps-panel-agent.service, \
                 `systemctl daemon-reload`, and `systemctl restart vps-panel-agent`."
            )));
        }
    }

    // 1. Create the account with a home directory + chosen shell.
    //    On ANY useradd failure, run `userdel -f` to wipe the partial entry
    //    so the operator can retry without hitting the "user already exists"
    //    guard. Exit code 12 ("cannot create home directory") and 13 ("cannot
    //    create mail spool") both leave a half-built account behind that
    //    nothing else will clean up.
    if let Err(e) = exec::run("useradd", ["-m", "-s", shell, name.as_str()]).await {
        let _ = exec::run("userdel", ["-f", name.as_str()]).await;
        return Err(e);
    }

    // 2. Set the password via chpasswd. Best-effort cleanup on failure:
    //    if chpasswd fails the account exists but has a locked password,
    //    which is worse than no account at all — userdel it and report.
    let chpasswd_input = format!("{}:{}\n", name, req.password);
    if let Err(e) = exec::run_with_input("chpasswd", Vec::<&str>::new(), &chpasswd_input).await {
        let _ = exec::run("userdel", ["-r", "-f", name.as_str()]).await;
        return Err(e);
    }

    // 3. Optional sudo group.
    if req.sudo {
        if let Err(e) = exec::run("usermod", ["-aG", "sudo", name.as_str()]).await {
            // Don't tear down the account on sudo failure — the user still
            // has a working login. Bubble the error so the panel surfaces it.
            return Err(e);
        }
    }

    // 4. Re-read /etc/passwd to confirm and grab uid/home for the response.
    let body = tokio::fs::read_to_string("/etc/passwd").await.map_err(AgentError::Io)?;
    let (uid, home) = body
        .lines()
        .find_map(|line| {
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() < 7 {
                return None;
            }
            if parts[0] != name {
                return None;
            }
            let uid: u32 = parts[2].parse().ok()?;
            Some((uid, parts[5].to_string()))
        })
        .ok_or_else(|| {
            AgentError::Other(anyhow::anyhow!(
                "user '{name}' missing from /etc/passwd after useradd"
            ))
        })?;

    Ok(Json(CreateSystemUserResp {
        name,
        uid,
        home,
    }))
}

#[derive(Deserialize)]
pub struct SetSystemUserPasswordReq {
    pub password: String,
}

/// Reset an existing Linux user's password. Same pipeline as
/// `set_root_password` — pipes `<name>:<pw>\n` into `chpasswd`. Refuses to
/// touch `root` (operators must use the dedicated /system/root-password
/// endpoint, which is what the existing UI calls) and the reserved system
/// accounts so a fat-fingered request can't, say, lock the postgres service.
pub async fn set_system_user_password(
    Path(name): Path<String>,
    Json(req): Json<SetSystemUserPasswordReq>,
) -> AgentResult<Json<OkResp>> {
    let name = name.trim().to_lowercase();
    if !is_safe_unix_name(&name) {
        return Err(AgentError::Invalid("invalid username".into()));
    }
    if name == "root" {
        return Err(AgentError::Invalid(
            "use /system/root-password to change the root password".into(),
        ));
    }
    if RESERVED_SYSTEM_USERS.contains(&name.as_str()) {
        return Err(AgentError::Invalid(format!(
            "refusing to reset password of reserved system account '{name}'"
        )));
    }
    if req.password.len() < 12 {
        return Err(AgentError::Invalid(
            "password must be at least 12 characters".into(),
        ));
    }
    if req.password.contains('\n')
        || req.password.contains('\r')
        || req.password.contains(':')
    {
        return Err(AgentError::Invalid(
            "password must not contain newlines or colons".into(),
        ));
    }

    let exists = Command::new("getent")
        .args(["passwd", &name])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false);
    if !exists {
        return Err(AgentError::NotFound);
    }

    let input = format!("{}:{}\n", name, req.password);
    exec::run_with_input("chpasswd", Vec::<&str>::new(), &input).await?;
    Ok(Json(OkResp { ok: true }))
}

#[derive(Deserialize, Default)]
pub struct DeleteSystemUserQuery {
    /// When true, pass `-r` to userdel so the user's home directory and mail
    /// spool are deleted as well. Default false — the cautious choice; the
    /// panel UI confirms before flipping it on.
    #[serde(default)]
    pub remove_home: bool,
}

/// Delete a Linux user via `userdel`. Refuses to touch `root` or the
/// reserved system accounts (those are the OS's responsibility). When
/// `remove_home=true`, `-r` is passed so /home/<name> and the mail spool
/// are wiped; otherwise the account is removed but the home dir stays as a
/// salvage option.
pub async fn delete_system_user(
    Path(name): Path<String>,
    Query(q): Query<DeleteSystemUserQuery>,
) -> AgentResult<Json<OkResp>> {
    let name = name.trim().to_lowercase();
    if !is_safe_unix_name(&name) {
        return Err(AgentError::Invalid("invalid username".into()));
    }
    if name == "root" {
        return Err(AgentError::Invalid("refusing to delete root".into()));
    }
    if RESERVED_SYSTEM_USERS.contains(&name.as_str()) {
        return Err(AgentError::Invalid(format!(
            "refusing to delete reserved system account '{name}'"
        )));
    }

    let exists = Command::new("getent")
        .args(["passwd", &name])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false);
    if !exists {
        return Err(AgentError::NotFound);
    }

    // userdel refuses with exit 8 if the user is currently logged in. Try
    // to terminate their processes first (best effort — pkill returns 1 when
    // nothing matched, which is fine).
    let _ = exec::run("pkill", ["-KILL", "-u", name.as_str()]).await;

    // `-f` tolerates a missing home dir / mail spool when `-r` is set, and
    // also lets us clean up half-built accounts left by a previous failed
    // `useradd -m` (entry exists in /etc/passwd, no /home/<name>).
    if q.remove_home {
        exec::run("userdel", ["-r", "-f", name.as_str()]).await?;
    } else {
        exec::run("userdel", ["-f", name.as_str()]).await?;
    }
    Ok(Json(OkResp { ok: true }))
}

#[cfg(test)]
mod tests {
    use super::{publickey_hint, render_panel_vhost};
    use crate::nginx_template::PEER_API_LOCATION;

    /// Every server block that proxies to the panel lets the servers of a
    /// network through to /api/peer/ (past the access list) and carries the
    /// upload limits: without them nginx refuses any body over 1 MB. The
    /// HTTPS redirect block proxies nothing.
    #[test]
    fn panel_vhosts_carry_the_peer_location_and_upload_limits() {
        for cert in [None, Some(("/c", "/k"))] {
            let body = render_panel_vhost("p.example.com", true, cert).unwrap();
            let ssl = cert.is_some();
            assert!(body.contains(PEER_API_LOCATION), "ssl={ssl}");
            assert_eq!(body.matches("location ^~ /api/peer/").count(), 1, "ssl={ssl}");
            // The peer location, uploads and /, in the one block that proxies.
            assert_eq!(body.matches("proxy_pass http://127.0.0.1:10000;").count(), 3, "ssl={ssl}");
            assert_eq!(body.matches("location = /api/files/upload {").count(), 1, "ssl={ssl}");
            assert_eq!(body.matches("client_max_body_size 5g;").count(), 1, "ssl={ssl}");
            assert_eq!(body.matches("proxy_request_buffering off;").count(), 1, "ssl={ssl}");
            // Server level, in every block: the redirect block too, so a POST
            // to it gets its redirect rather than a 413.
            assert_eq!(body.matches("client_max_body_size 25m;").count(), if ssl { 2 } else { 1 }, "ssl={ssl}");
            assert_eq!(body.matches("listen 80 default_server;").count(), 1, "ssl={ssl}");
            assert!(body.starts_with("# vps-panel-host:p.example.com\n"), "ssl={ssl}");
        }
        let not_default = render_panel_vhost("p.example.com", false, None).unwrap();
        assert!(not_default.contains("listen 80;") && !not_default.contains("default_server"));
    }

    const REFUSED: &str = "git@github.com: Permission denied (publickey).\n\
                           fatal: Could not read from remote repository.\n";
    const ROOT_CONFIG: &str = "Host github_example_web\n  HostName github.com\n  User git\n  \
                               IdentityFile ~/.ssh/github_example_web\n  IdentitiesOnly yes\n\n\
                               Host github_vps_panel\n  HostName github.com\n  User git\n  \
                               IdentityFile ~/.ssh/github_vps_panel\n  IdentitiesOnly yes\n";

    #[test]
    fn a_plain_host_origin_is_pointed_at_the_deploy_keys_alias() {
        let origin = "git@github.com:Meliorate-agency/vps-panel.git";
        let hint = publickey_hint("/root/vps-panel-src", origin, ROOT_CONFIG, REFUSED).unwrap();
        assert!(hint.contains("core.sshCommand set in the clone is ignored"), "{hint}");
        assert!(hint.contains("default-named ones"), "{hint}");
        assert!(
            hint.ends_with(
                "\n\ngit -C /root/vps-panel-src remote set-url origin \
                 git@github_vps_panel:Meliorate-agency/vps-panel.git"
            ),
            "{hint}"
        );
    }

    #[test]
    fn the_one_block_for_the_host_is_the_deploy_key() {
        let config = "Host=panel \"gh-*\"\n  hostname = GitHub.com\n  \
                      IdentityFile \"/root/.ssh/k\"\nMatch exec true\n  HostName gitlab.com\n\
                      Host lab\n  HostName gitlab.com\n";
        let origin = "ssh://deploy@github.com:22/owner/repo.git";
        let output = "PERMISSION DENIED (PUBLICKEY)";
        let hint = publickey_hint("/srv/src", origin, config, output).unwrap();
        assert!(
            hint.ends_with("git -C /srv/src remote set-url origin deploy@panel:owner/repo.git"),
            "{hint}"
        );
    }

    #[test]
    fn without_a_block_for_the_host_the_hint_adds_one() {
        let origin = "git@github.com:Meliorate-agency/vps-panel.git";
        let hint = publickey_hint("/root/vps-panel-src", origin, "", REFUSED).unwrap();
        assert!(
            hint.ends_with(
                "\n\nprintf '%s\\n' 'Host github_vps_panel' '  HostName github.com' '  User git' \
                 '  IdentityFile ~/.ssh/github_vps_panel' '  IdentitiesOnly yes' \
                 >> /root/.ssh/config\n\
                 git -C /root/vps-panel-src remote set-url origin \
                 git@github_vps_panel:Meliorate-agency/vps-panel.git"
            ),
            "{hint}"
        );
    }

    #[test]
    fn several_blocks_and_none_the_deploy_key_are_listed() {
        let config = "Host a\n  HostName github.com\nHost b\n  HostName GitHub.com\n\
                      Host c\n  HostName gitlab.com\n";
        let hint = publickey_hint("/d", "git@github.com:o/r.git", config, REFUSED).unwrap();
        let fix = "\n\ngit -C /d remote set-url origin git@<alias>:o/r.git";
        assert!(hint.contains("can read o/r, one of a, b in /root/.ssh/config"), "{hint}");
        assert!(hint.ends_with(fix), "{hint}");
    }

    #[test]
    fn an_alias_origin_means_its_key_is_not_on_the_repository() {
        let origin = "git@github_vps_panel:Meliorate-agency/vps-panel.git";
        let hint = publickey_hint("/root/vps-panel-src", origin, ROOT_CONFIG, REFUSED).unwrap();
        assert!(
            hint.contains(
                "github.com refused the key behind it (~/.ssh/github_vps_panel): that key is not \
                 authorized on Meliorate-agency/vps-panel"
            ),
            "{hint}"
        );
        assert!(hint.contains("public key (~/.ssh/github_vps_panel.pub)"), "{hint}");
        assert!(hint.ends_with("\n\nssh -T git@github_vps_panel"), "{hint}");
        assert!(!hint.contains("set-url"), "{hint}");

        let unknown = publickey_hint("/d", "git@gh_other:o/r.git", ROOT_CONFIG, REFUSED).unwrap();
        assert!(unknown.contains("the remote refused the key behind it: that key"), "{unknown}");
    }

    #[test]
    fn other_failures_get_no_hint() {
        let unresolved =
            "ssh: Could not resolve hostname github.com: Temporary failure in name resolution";
        for (origin, output) in [
            ("git@github.com:o/r.git", unresolved),
            ("https://github.com/o/r.git", REFUSED),
            ("/srv/mirror.git", REFUSED),
            ("", REFUSED),
        ] {
            assert_eq!(publickey_hint("/d", origin, ROOT_CONFIG, output), None, "{origin}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_update_check_fetches_with_the_agents_ssh_command() {
        use super::fetch_error;
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(d)
                .env_remove("GIT_SSH_COMMAND")
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .expect("git");
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(out.status.success(), "git {args:?}: {stderr}");
        };
        git(&["init", "-q", "--bare", "o/r.git"]);
        git(&["clone", "-q", "o/r.git", "src"]);
        git(&["-C", "src", "commit", "-q", "--allow-empty", "-m", "one"]);
        git(&["-C", "src", "push", "-q", "origin", "HEAD"]);
        let fake_ssh = d.join("fake-ssh");
        std::fs::write(
            &fake_ssh,
            "#!/bin/sh\n\
             [ \"$1\" = -G ] && exit 1\n\
             for last; do :; done\n\
             cd \"$(dirname \"$0\")\" && exec sh -c \"$last\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake_ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
        git(&["-C", "src", "remote", "set-url", "origin", "git@nowhere.invalid:o/r.git"]);
        git(&["-C", "src", "config", "core.sshCommand", &fake_ssh.display().to_string()]);
        git(&["-C", "src", "fetch", "-q"]);

        let src = d.join("src").display().to_string();
        assert!(
            fetch_error(&src).await.is_some(),
            "the check must fetch with the agent's GIT_SSH_COMMAND, which overrides \
             core.sshCommand, or it passes where the update fails"
        );
    }
}
