use crate::config::Config;
use crate::error::{AgentError, AgentResult};
use crate::exec;
use crate::fs_atomic;
use crate::ipc::{tasks::TaskHandle, TaskAccepted};
use crate::state::AppState;
use axum::{extract::State, Json};
use mysql_async::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Deserialize)]
pub struct IssueReq {
    pub domain: String,
}

#[derive(Serialize)]
pub struct CertRow {
    pub domain: String,
    pub not_after: String,
    pub days_remaining: i64,
    /// false when nothing uses the cert any more: no nginx vhost serves this
    /// name (site deleted, cert kept on purpose) AND it covers no live mail
    /// tenant's `mail.<d>`. The renewal loop skips these - there is nothing to
    /// answer the HTTP-01 challenge and nothing presents the cert - and the
    /// dashboard does not count them as "expiring". A mail-only cert stays
    /// `true` so it can never expire unnoticed.
    pub serving: bool,
}

pub(crate) fn resolve_acme_email(panel_db: &crate::db::PanelDb, cfg: &Config) -> String {
    let from_db = panel_db
        .with(|conn| {
            conn.query_row(
                "SELECT value FROM settings WHERE key = 'panel.acme_email'",
                [],
                |row| row.get::<_, String>(0),
            )
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(String::new()),
                other => Err(other),
            })
        })
        .ok();
    match from_db {
        Some(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => cfg.acme_email.clone(),
    }
}

pub async fn list(State(state): State<AppState>) -> AgentResult<Json<Vec<CertRow>>> {
    // Enumerate both the panel's own cert dir AND certbot's, so the dashboard
    // count and any "list all certs" view match what's actually serving traffic.
    // existing_cert_paths() (sites.rs) and mail_cert_pair_for() (ssl.rs) both
    // fall back to /etc/letsencrypt/live/ when picking a cert for a vhost or
    // mail SNI; without the merge here, the dashboard understated reality.
    //
    // Dedupe by domain — if both dirs have a cert for the same name, the
    // panel-managed one wins (matches the precedence in existing_cert_paths).
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut rows = Vec::new();
    for dir in ["/etc/vps-panel/certs", "/etc/letsencrypt/live"] {
        read_certs_dir(&state, std::path::Path::new(dir), &mut rows, &mut seen).await;
    }
    rows.sort_by(|a, b| a.domain.cmp(&b.domain));
    Ok(Json(rows))
}

async fn read_certs_dir(
    state: &AppState,
    dir: &std::path::Path,
    rows: &mut Vec<CertRow>,
    seen: &mut std::collections::HashSet<String>,
) {
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(e) => e,
        Err(_) => return,
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let Ok(ft) = entry.file_type().await else {
            continue;
        };
        if !ft.is_dir() {
            continue;
        }
        let path = entry.path().join("fullchain.pem");
        if !path.exists() {
            continue;
        }
        let domain = entry.file_name().to_string_lossy().into_owned();
        if !seen.insert(domain.clone()) {
            continue;
        }
        let (not_after, days_remaining) = cert_expiry(&path).await.unwrap_or_default();
        let serving = has_serving_vhost(state, &domain).await
            || !mail_tenants_covered(state, &path).await.is_empty();
        rows.push(CertRow {
            domain,
            not_after,
            days_remaining,
            serving,
        });
    }
}

pub async fn issue(
    State(state): State<AppState>,
    Json(req): Json<IssueReq>,
) -> AgentResult<Json<TaskAccepted>> {
    spawn_issue_task(&state, "ssl.issue", req.domain)
}

pub async fn renew(
    State(state): State<AppState>,
    Json(req): Json<IssueReq>,
) -> AgentResult<Json<TaskAccepted>> {
    spawn_issue_task(&state, "ssl.renew", req.domain)
}

/// Body of `/ssl/issue` and `/ssl/renew` (the Domains page Renew button, MCP
/// `ssl.renew`): order the cert, then make every consumer pick it up. nginx
/// keeps serving the old cert from memory until it reloads, so a task that
/// writes the new files but cannot reload nginx is reported as failed.
fn spawn_issue_task(state: &AppState, kind: &str, domain: String) -> AgentResult<Json<TaskAccepted>> {
    let domain = domain.trim().trim_end_matches('.').to_lowercase();
    if !crate::validate::is_safe_subdomain(&domain) {
        return Err(AgentError::Invalid("invalid domain".into()));
    }
    let task = state.tasks.create(&format!("{kind}:{domain}"));
    let task_id = task.id.clone();
    let state_clone = state.clone();
    let cfg = state.config.clone();
    let panel_db = state.panel_db.clone();
    tokio::spawn(async move {
        task.start();
        let acme_email = resolve_acme_email(&panel_db, &cfg);
        let names = desired_san_set(&state_clone, &domain).await;
        if !run_acme_preflight(&state_clone, &names, &task).await {
            task.finish(false);
            return;
        }
        match issue_for_names(&cfg, &acme_email, &domain, &names, &task).await {
            Ok(_) => {
                if let Err(e) = regenerate_mail_sni(&state_clone).await {
                    task.log(format!("mail sni regen warning: {e}"));
                }
                match reload_nginx_checked().await {
                    Ok(()) => {
                        task.log("nginx reloaded; the new certificate is live");
                        task.finish(true);
                    }
                    Err(e) => {
                        task.log(format!("certificate saved, but {e}"));
                        task.finish(false);
                    }
                }
            }
            Err(e) => {
                task.log(format!("error: {e}"));
                task.finish(false);
            }
        }
    });
    Ok(Json(TaskAccepted { task_id }))
}

/// `nginx -t`, then reload. nginx reads certificate files only at (re)load, so
/// every path that writes a cert a vhost uses must end here. Returns the
/// reason when the config does not test clean; nothing is reloaded then.
pub(crate) async fn reload_nginx_checked() -> Result<(), String> {
    if let Err(e) = crate::handlers::nginx::test_config().await {
        return Err(format!("nginx -t failed, so nginx was not reloaded: {e}"));
    }
    exec::run("systemctl", ["reload", "nginx"])
        .await
        .map(|_| ())
        .map_err(|e| format!("nginx reload failed: {e}"))
}

/// A cert that lists `mail.<tenant>` is validated over HTTP-01 through the
/// tenant's port-80 helper vhost. That vhost can be missing (removed by hand,
/// or a tenant older than the helper), which would fail the whole order -
/// including the site names it shares a cert with. Put it back first.
pub(crate) async fn ensure_mail_acme_helpers(state: &AppState, names: &[String], task: &TaskHandle) {
    for name in names {
        let Some(tenant) = name.strip_prefix("mail.") else {
            continue;
        };
        if !has_mail_domain(state, tenant).await {
            continue;
        }
        if let Err(e) = crate::handlers::mail::ensure_mail_http_vhost(tenant).await {
            task.log(format!("could not install the ACME helper vhost for {name}: {e}"));
        }
    }
}

/// Preflight every SAN before ACME so a stale/missing DNS record fails fast
/// instead of timing out inside `instant-acme`. Returns false to abort.
async fn run_acme_preflight(state: &AppState, names: &[String], task: &TaskHandle) -> bool {
    ensure_mail_acme_helpers(state, names, task).await;
    let mut ok = true;
    for name in names {
        if let Err(e) = crate::handlers::dns::preflight_acme_dns(state, name, task).await {
            task.log(format!("ssl preflight failed for {name}: {e}"));
            ok = false;
        }
    }
    ok
}

pub async fn issue_for(
    cfg: &Config,
    acme_email: &str,
    domain: &str,
    task: &TaskHandle,
) -> AgentResult<()> {
    issue_for_names(cfg, acme_email, domain, &[domain.to_string()], task).await
}

pub async fn issue_for_names(
    cfg: &Config,
    acme_email: &str,
    primary: &str,
    names: &[String],
    task: &TaskHandle,
) -> AgentResult<()> {
    use instant_acme::{
        Account, AuthorizationStatus, ChallengeType, Identifier, NewAccount, NewOrder, OrderStatus,
    };

    if names.is_empty() {
        return Err(AgentError::Invalid("no domain names supplied".into()));
    }

    if let Err(e) = crate::validate::validate_acme_email(acme_email) {
        task.log(format!("{e}"));
        return Err(e);
    }

    // Orders for the same primary never overlap: they would write the same
    // cert/key pair and could leave a key that does not match its cert.
    let lock = cert_order_lock(primary);
    let _order_guard = lock.lock().await;

    let account = match load_acme_account(&cfg.acme_directory, acme_email).await {
        Some(account) => {
            task.log(format!("using the saved ACME account at {}", cfg.acme_directory));
            account
        }
        None => {
            task.log(format!("creating ACME account at {}", cfg.acme_directory));
            let (account, credentials) = Account::create(
                &NewAccount {
                    contact: &[&format!("mailto:{}", acme_email)],
                    terms_of_service_agreed: true,
                    only_return_existing: false,
                },
                &cfg.acme_directory,
                None,
            )
            .await
            .map_err(|e| AgentError::Other(anyhow::anyhow!("acme account: {e}")))?;
            save_acme_account(&cfg.acme_directory, acme_email, credentials).await;
            account
        }
    };

    task.log(format!("ordering certificate for {}", names.join(", ")));
    let identifiers: Vec<Identifier> =
        names.iter().map(|n| Identifier::Dns(n.clone())).collect();
    let mut order = account
        .new_order(&NewOrder {
            identifiers: &identifiers,
        })
        .await
        .map_err(|e| AgentError::Other(anyhow::anyhow!("acme new_order: {e}")))?;

    let authorizations = order
        .authorizations()
        .await
        .map_err(|e| AgentError::Other(anyhow::anyhow!("acme authorizations: {e}")))?;

    for auth in &authorizations {
        match auth.status {
            AuthorizationStatus::Pending => {}
            AuthorizationStatus::Valid => continue,
            other => {
                return Err(AgentError::Other(anyhow::anyhow!(
                    "unexpected authorization status: {other:?}"
                )))
            }
        }
        let challenge = auth
            .challenges
            .iter()
            .find(|c| c.r#type == ChallengeType::Http01)
            .ok_or_else(|| AgentError::Other(anyhow::anyhow!("no http-01 challenge offered")))?;

        let key_auth = order.key_authorization(challenge);
        let token_path = PathBuf::from(format!(
            "{}/.well-known/acme-challenge/{}",
            crate::nginx_template::ACME_WEBROOT,
            challenge.token
        ));
        task.log(format!("placing challenge token at {}", token_path.display()));
        fs_atomic::ensure_dir(token_path.parent().unwrap(), 0o755).await?;
        fs_atomic::write(&token_path, key_auth.as_str().as_bytes(), 0o644).await?;

        order
            .set_challenge_ready(&challenge.url)
            .await
            .map_err(|e| AgentError::Other(anyhow::anyhow!("set_challenge_ready: {e}")))?;
    }

    task.log("polling order status");
    let mut tries = 0;
    loop {
        tries += 1;
        if tries > 30 {
            return Err(AgentError::Other(anyhow::anyhow!(
                "order did not become ready in time"
            )));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        let state = order
            .refresh()
            .await
            .map_err(|e| AgentError::Other(anyhow::anyhow!("order refresh: {e}")))?;
        match state.status {
            OrderStatus::Ready => break,
            OrderStatus::Pending => continue,
            OrderStatus::Invalid => {
                let detail = match order.authorizations().await {
                    Ok(auths) => format_invalid_order(&auths),
                    Err(e) => format!("(could not fetch authorizations: {e})"),
                };
                task.log(format!("order invalid: {detail}"));
                return Err(AgentError::Other(anyhow::anyhow!("order invalid: {detail}")));
            }
            _ => continue,
        }
    }

    let mut params = rcgen::CertificateParams::new(names.to_vec())
        .map_err(|e| AgentError::Other(anyhow::anyhow!("rcgen params: {e}")))?;
    params.distinguished_name = rcgen::DistinguishedName::new();
    let key_pair = rcgen::KeyPair::generate()
        .map_err(|e| AgentError::Other(anyhow::anyhow!("rcgen keypair: {e}")))?;
    let csr = params
        .serialize_request(&key_pair)
        .map_err(|e| AgentError::Other(anyhow::anyhow!("rcgen csr: {e}")))?;
    let csr_der = csr.der();

    task.log("finalizing order");
    order
        .finalize(csr_der)
        .await
        .map_err(|e| AgentError::Other(anyhow::anyhow!("finalize: {e}")))?;

    let cert_pem = loop {
        match order
            .certificate()
            .await
            .map_err(|e| AgentError::Other(anyhow::anyhow!("certificate: {e}")))?
        {
            Some(c) => break c,
            None => tokio::time::sleep(Duration::from_secs(2)).await,
        }
    };

    let cert_dir = PathBuf::from(format!("/etc/vps-panel/certs/{primary}"));
    fs_atomic::ensure_dir(&cert_dir, 0o750).await?;
    fs_atomic::write(&cert_dir.join("fullchain.pem"), cert_pem.as_bytes(), 0o640).await?;
    fs_atomic::write(
        &cert_dir.join("privkey.pem"),
        key_pair.serialize_pem().as_bytes(),
        0o640,
    )
    .await?;
    if cfg.vps_panel_gid != 0 {
        let _ = fs_atomic::chown_blocking(&cert_dir.join("fullchain.pem"), 0, cfg.vps_panel_gid);
        let _ = fs_atomic::chown_blocking(&cert_dir.join("privkey.pem"), 0, cfg.vps_panel_gid);
    }

    task.log(format!("certificate written to {}", cert_dir.display()));

    if primary == cfg.panel_host {
        task.log("rewiring postfix and dovecot to use the new certificate");
        if let Err(e) = apply_mail_cert(primary, &cert_dir).await {
            task.log(format!("mail tls update failed: {e}"));
        }
    }

    Ok(())
}

/// Saved ACME account credentials (root-only file). Every order used to
/// register a brand-new account, and Let's Encrypt allows only 10 new
/// accounts per IP per 3 hours: a day with many renewals, or a few agent
/// restarts, hit that limit.
const ACME_ACCOUNT_FILE: &str = "/etc/vps-panel/acme-account.json";

#[derive(Serialize, Deserialize)]
struct SavedAcmeAccount {
    directory: String,
    email: String,
    credentials: instant_acme::AccountCredentials,
}

/// The saved account, if it was made for this directory and contact email
/// and still loads. Anything else means a new account is created.
async fn load_acme_account(directory: &str, email: &str) -> Option<instant_acme::Account> {
    let body = tokio::fs::read(ACME_ACCOUNT_FILE).await.ok()?;
    let saved: SavedAcmeAccount = serde_json::from_slice(&body).ok()?;
    if saved.directory != directory || saved.email != email {
        return None;
    }
    match instant_acme::Account::from_credentials(saved.credentials).await {
        Ok(account) => Some(account),
        Err(e) => {
            tracing::warn!(error = %e, "saved ACME account could not be loaded; creating a new one");
            None
        }
    }
}

async fn save_acme_account(directory: &str, email: &str, credentials: instant_acme::AccountCredentials) {
    let saved = SavedAcmeAccount {
        directory: directory.to_string(),
        email: email.to_string(),
        credentials,
    };
    match serde_json::to_vec(&saved) {
        Ok(body) => {
            if let Err(e) = fs_atomic::write(std::path::Path::new(ACME_ACCOUNT_FILE), &body, 0o600).await {
                tracing::warn!(error = %e, "could not save the ACME account");
            }
        }
        Err(e) => tracing::warn!(error = %e, "could not serialize the ACME account"),
    }
}

/// One async lock per cert primary name, shared by every issuing path.
fn cert_order_lock(primary: &str) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .entry(primary.to_string())
        .or_default()
        .clone()
}

fn format_invalid_order(auths: &[instant_acme::Authorization]) -> String {
    use instant_acme::{AuthorizationStatus, Identifier};
    let mut parts = Vec::new();
    for auth in auths {
        let id = match &auth.identifier {
            Identifier::Dns(d) => d.as_str(),
        };
        if auth.status == AuthorizationStatus::Valid {
            continue;
        }
        let mut errs = Vec::new();
        for ch in &auth.challenges {
            if let Some(err) = &ch.error {
                let typ = err.r#type.as_deref().unwrap_or("?");
                let det = err.detail.as_deref().unwrap_or("(no detail)");
                errs.push(format!("{:?}={typ} {det}", ch.r#type));
            }
        }
        if errs.is_empty() {
            parts.push(format!("{id} status={:?}", auth.status));
        } else {
            parts.push(format!("{id}: {}", errs.join(", ")));
        }
    }
    if parts.is_empty() {
        "(no challenge errors reported)".to_string()
    } else {
        parts.join(" | ")
    }
}

pub(crate) async fn apply_mail_cert(domain: &str, cert_dir: &std::path::Path) -> AgentResult<()> {
    let cert = cert_dir.join("fullchain.pem");
    let key = cert_dir.join("privkey.pem");
    let cert_str = cert.to_str().unwrap_or("");
    let key_str = key.to_str().unwrap_or("");

    exec::run(
        "postconf",
        ["-e", &format!("smtpd_tls_cert_file={cert_str}")],
    )
    .await?;
    exec::run(
        "postconf",
        ["-e", &format!("smtpd_tls_key_file={key_str}")],
    )
    .await?;

    let dovecot_drop = format!(
        "ssl_cert = <{cert_str}\nssl_key = <{key_str}\n"
    );
    crate::fs_atomic::write(
        std::path::Path::new("/etc/dovecot/conf.d/98-vps-panel-tls.conf"),
        dovecot_drop.as_bytes(),
        0o644,
    )
    .await?;

    exec::run("systemctl", ["reload", "postfix"]).await?;
    exec::run("systemctl", ["reload", "dovecot"]).await?;
    let _ = domain;
    Ok(())
}

pub(crate) async fn cert_expiry(path: &std::path::Path) -> Option<(String, i64)> {
    let out = exec::run(
        "openssl",
        ["x509", "-in", path.to_str()?, "-noout", "-enddate"],
    )
    .await
    .ok()?;
    let line = out.stdout.lines().next()?;
    let after = line.strip_prefix("notAfter=")?.trim().to_string();
    // openssl emits e.g. "Jul 29 17:30:31 2026 GMT". chrono's %Z parser is
    // unreliable for timezone abbreviations, so strip the trailing token and
    // parse the remainder as a NaiveDateTime treated as UTC (GMT == UTC for
    // X.509 notAfter purposes).
    let date_part = after.rsplit_once(' ').map(|(d, _tz)| d).unwrap_or(&after);
    let parsed = chrono::NaiveDateTime::parse_from_str(date_part, "%b %e %H:%M:%S %Y").ok()?;
    let parsed_utc = parsed.and_utc();
    let days = (parsed_utc - chrono::Utc::now()).num_days();
    Some((after, days))
}

pub async fn renewal_loop(state: AppState) {
    loop {
        let _ = scan_and_renew(&state).await;
        tokio::time::sleep(Duration::from_secs(86_400)).await;
    }
}

async fn scan_and_renew(state: &AppState) -> AgentResult<()> {
    let dir = PathBuf::from("/etc/vps-panel/certs");
    let mut entries = match tokio::fs::read_dir(&dir).await {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let domain = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path().join("fullchain.pem");
        if let Some((_, days)) = cert_expiry(&path).await {
            if days < 30 {
                if !has_serving_vhost(state, &domain).await {
                    // No vhost answers HTTP-01 for this name any more. If the
                    // cert still covers a live tenant's mail.<d> (site deleted,
                    // mail kept; or a standalone mail.<d> cert whose helper
                    // vhost is gone), Postfix/Dovecot still present it: get
                    // mail.<d> a cert of its own through the helper vhost
                    // instead of letting mail TLS expire. Once that cert
                    // exists mail_cert_plan reports Covered and this stops.
                    let tenants = mail_tenants_covered(state, &path).await;
                    if tenants.is_empty() {
                        // Nothing uses it: a site removed with its cert kept.
                        // Ordering would fail every day forever (and burn Let's
                        // Encrypt quota). Deleting the directory under
                        // /etc/vps-panel/certs silences this log line.
                        tracing::info!(
                            domain = %domain,
                            days,
                            "skipping renewal: nothing serves this cert (orphaned)"
                        );
                        continue;
                    }
                    for tenant in tenants {
                        if let Err(e) = crate::handlers::mail::ensure_mail_http_vhost(&tenant).await {
                            tracing::warn!(tenant = %tenant, error = %e, "mail cert renewal: helper vhost setup failed");
                            continue;
                        }
                        if let Some(task_id) = spawn_mail_cert_task(state, &tenant).await {
                            tracing::info!(
                                tenant = %tenant,
                                cert = %domain,
                                days,
                                task_id = %task_id,
                                "mail-only cert expiring: ordering a mail certificate"
                            );
                        }
                    }
                    continue;
                }
                // The loop also runs at agent start, so a restart in the
                // middle of a renewal must not stack a second order.
                if state.tasks.any_live_with_prefix(&format!("ssl.renew:{domain}")) {
                    continue;
                }
                let names = desired_san_set(state, &domain).await;
                let task = state.tasks.create(&format!("ssl.renew:{domain}"));
                let cfg = state.config.clone();
                let panel_db = state.panel_db.clone();
                let state_clone = state.clone();
                let domain_owned = domain.clone();
                tokio::spawn(async move {
                    task.start();
                    ensure_mail_acme_helpers(&state_clone, &names, &task).await;
                    let acme_email = resolve_acme_email(&panel_db, &cfg);
                    match issue_for_names(&cfg, &acme_email, &domain_owned, &names, &task).await {
                        Ok(()) => {
                            if let Err(e) = regenerate_mail_sni(&state_clone).await {
                                task.log(format!("mail sni regen warning: {e}"));
                            }
                            match reload_nginx_checked().await {
                                Ok(()) => task.finish(true),
                                Err(e) => {
                                    task.log(format!("certificate renewed, but {e}"));
                                    task.finish(false);
                                }
                            }
                        }
                        Err(e) => {
                            task.log(format!("renewal error: {e}"));
                            task.finish(false);
                        }
                    }
                });
            }
        }
    }
    // Certs renewed outside the panel (certbot's own timer) change on disk
    // without anyone rebuilding the Postfix map, which embeds the PEM, or
    // reloading Dovecot. Catch that, and any tenant whose best cert changed.
    if let Err(e) = refresh_mail_sni_if_stale(state).await {
        tracing::warn!(error = %e, "mail sni refresh failed");
    }
    Ok(())
}

/// The full SAN list a cert for `domain` should cover, given the panel's
/// current state. Always includes the bare domain; adds `www.<domain>` if the
/// site is configured to force the www redirect; adds `mail.<domain>` if the
/// domain is also a mail tenant (so the same cert can serve SMTP/IMAP for
/// that hostname via Postfix/Dovecot SNI maps).
pub(crate) async fn desired_san_set(state: &AppState, domain: &str) -> Vec<String> {
    let mut names = vec![domain.to_string()];
    let force_www = state
        .panel_db
        .with(|conn| {
            conn.query_row(
                "SELECT force_www_redirect FROM sites WHERE domain = ?1",
                rusqlite::params![domain],
                |row| row.get::<_, i64>(0),
            )
            .or(Ok(0))
        })
        .unwrap_or(0)
        != 0;
    if force_www {
        names.push(format!("www.{domain}"));
    }
    if has_mail_domain(state, domain).await {
        names.push(format!("mail.{domain}"));
    }
    names
}

async fn has_mail_domain(state: &AppState, domain: &str) -> bool {
    let Ok(mut conn) = state.mariadb.pool().get_conn().await else {
        return false;
    };
    if conn.query_drop("USE `mail`").await.is_err() {
        return false;
    }
    let res: Result<Option<u32>, _> = conn
        .exec_first("SELECT id FROM virtual_domains WHERE name = ?", (domain,))
        .await;
    matches!(res, Ok(Some(_)))
}

const POSTFIX_SNI_MAP: &str = "/etc/postfix/sni-map";
// Must sort AFTER 98-vps-panel-tls.conf: a global ssl_cert that follows a
// local_name filter makes doveconf warn on every run. The per-tenant blocks
// win either way; only the warning depends on the order.
const DOVECOT_SNI_CONF: &str = "/etc/dovecot/conf.d/99-vps-panel-sni.conf";
// Where DOVECOT_SNI_CONF used to live. Removed on every regeneration so the
// old and new files never both define the same local_name blocks.
const LEGACY_DOVECOT_SNI_CONF: &str = "/etc/dovecot/conf.d/97-vps-panel-sni.conf";

/// Names in the certificate's subjectAltName extension, lowercased.
pub(crate) async fn cert_san_names(cert_path: &std::path::Path) -> Vec<String> {
    let Some(p) = cert_path.to_str() else {
        return Vec::new();
    };
    let Ok(out) = exec::run("openssl", ["x509", "-in", p, "-noout", "-ext", "subjectAltName"]).await
    else {
        return Vec::new();
    };
    parse_san_names(&out.stdout)
}

/// Pull the `DNS:` entries out of `openssl x509 -ext subjectAltName` output.
pub(crate) fn parse_san_names(text: &str) -> Vec<String> {
    text.split(|c: char| c == ',' || c.is_whitespace())
        .filter_map(|tok| tok.trim().strip_prefix("DNS:"))
        .map(|n| n.trim().to_ascii_lowercase())
        .filter(|n| !n.is_empty())
        .collect()
}

/// Does the certificate at `cert_path` list `name` as a SAN?
pub(crate) async fn cert_covers(cert_path: &std::path::Path, name: &str) -> bool {
    let want = name.to_ascii_lowercase();
    cert_san_names(cert_path).await.iter().any(|n| *n == want)
}

/// Is there an nginx vhost in sites-enabled that serves `name`? A panel site
/// (`site-<name>`), an app / ACME-helper / external vhost (`<name>`), or the
/// panel host itself (`vps-panel.conf`). Without one there is nothing to
/// answer an HTTP-01 challenge and nothing that uses the cert.
pub(crate) async fn has_serving_vhost(state: &AppState, name: &str) -> bool {
    for candidate in [
        format!("/etc/nginx/sites-enabled/site-{name}"),
        format!("/etc/nginx/sites-enabled/{name}"),
    ] {
        if tokio::fs::try_exists(&candidate).await.unwrap_or(false) {
            return true;
        }
    }
    !state.config.panel_host.is_empty()
        && name == state.config.panel_host
        && tokio::fs::try_exists("/etc/nginx/sites-enabled/vps-panel.conf")
            .await
            .unwrap_or(false)
}

/// Live mail tenants whose `mail.<tenant>` host the cert at `cert_path`
/// lists. Postfix/Dovecot present such a cert, so it is in use even when no
/// nginx vhost serves any of its names.
pub(crate) async fn mail_tenants_covered(state: &AppState, cert_path: &std::path::Path) -> Vec<String> {
    let mut tenants = Vec::new();
    for name in cert_san_names(cert_path).await {
        if let Some(tenant) = name.strip_prefix("mail.") {
            if has_mail_domain(state, tenant).await {
                tenants.push(tenant.to_string());
            }
        }
    }
    tenants
}

/// The cert pair Postfix/Dovecot should present for `mail.<domain>`: of the
/// candidates whose SAN list actually contains that hostname, the one that
/// expires last, so an expiring or expired site cert never shadows a fresh
/// standalone one. Ties keep this order: panel `<d>` (site cert carrying the
/// mail SAN), panel `mail.<d>` (standalone), then the certbot equivalents. A
/// cert that does not list mail.<d> is never returned - presenting it would
/// be a name mismatch.
pub(crate) async fn mail_cert_pair_for(domain: &str) -> Option<(PathBuf, PathBuf)> {
    let host = format!("mail.{domain}");
    let candidates = [
        format!("/etc/vps-panel/certs/{domain}"),
        format!("/etc/vps-panel/certs/{host}"),
        format!("/etc/letsencrypt/live/{domain}"),
        format!("/etc/letsencrypt/live/{host}"),
    ];
    let mut best: Option<(i64, PathBuf, PathBuf)> = None;
    for dir in candidates {
        let full = PathBuf::from(format!("{dir}/fullchain.pem"));
        let key = PathBuf::from(format!("{dir}/privkey.pem"));
        if !(full.exists() && key.exists() && cert_covers(&full, &host).await) {
            continue;
        }
        let days = cert_expiry(&full).await.map(|(_, d)| d).unwrap_or(i64::MIN);
        if best.as_ref().map_or(true, |(best_days, _, _)| days > *best_days) {
            best = Some((days, full, key));
        }
    }
    best.map(|(_, full, key)| (full, key))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MailCertPlan {
    /// A valid cert with at least 30 days left already lists mail.<d>.
    Covered,
    /// Re-issue the tenant's own site cert (panel-managed vhost + panel cert
    /// dir both exist) so it gains the mail.<d> SAN. One cert for the site
    /// and its mail host, renewed together.
    ReissueSite,
    /// Order a standalone cert for mail.<d>, validated through the ACME
    /// helper vhost `mail.rs::ensure_mail_http_vhost` installs.
    Standalone,
}

pub(crate) async fn mail_cert_plan(_state: &AppState, tenant: &str) -> MailCertPlan {
    if let Some((full, _)) = mail_cert_pair_for(tenant).await {
        if let Some((_, days)) = cert_expiry(&full).await {
            if days >= 30 {
                return MailCertPlan::Covered;
            }
        }
    }
    let panel_site_cert =
        PathBuf::from(format!("/etc/vps-panel/certs/{tenant}/fullchain.pem"));
    let site_vhost = format!("/etc/nginx/sites-enabled/site-{tenant}");
    let app_vhost = format!("/etc/nginx/sites-enabled/{tenant}");
    let apex_served = tokio::fs::try_exists(&site_vhost).await.unwrap_or(false)
        || tokio::fs::try_exists(&app_vhost).await.unwrap_or(false);
    if panel_site_cert.exists() && apex_served {
        MailCertPlan::ReissueSite
    } else {
        MailCertPlan::Standalone
    }
}

/// Kick off the task that gets `mail.<tenant>` a valid cert and wires it
/// into Postfix/Dovecot. Returns None when nothing has to be ordered: the
/// name is already covered (SNI maps are regenerated anyway, so a cert that
/// appeared out of band gets picked up) or an SSL task for this domain is
/// already in flight.
pub(crate) async fn spawn_mail_cert_task(state: &AppState, tenant: &str) -> Option<String> {
    let plan = mail_cert_plan(state, tenant).await;
    if plan == MailCertPlan::Covered {
        if let Err(e) = regenerate_mail_sni(state).await {
            tracing::warn!(tenant, error = %e, "mail sni regen failed");
        }
        return None;
    }
    for prefix in [
        format!("sites.ssl:{tenant}"),
        format!("ssl.renew:{tenant}"),
        format!("ssl.issue:{tenant}"),
        format!("mail.ssl:{tenant}"),
    ] {
        if state.tasks.any_live_with_prefix(&prefix) {
            tracing::info!(
                tenant,
                %prefix,
                "mail cert: an SSL task for this domain is already running; not starting another"
            );
            return None;
        }
    }

    let host = format!("mail.{tenant}");
    let (primary, mut names) = match plan {
        MailCertPlan::ReissueSite => (tenant.to_string(), desired_san_set(state, tenant).await),
        _ => (host.clone(), Vec::new()),
    };
    if !names.contains(&host) {
        names.push(host.clone());
    }

    let task = state.tasks.create(&format!("mail.ssl:{tenant}"));
    let task_id = task.id.clone();
    let cfg = state.config.clone();
    let panel_db = state.panel_db.clone();
    let state_clone = state.clone();
    tokio::spawn(async move {
        task.start();
        task.log(match plan {
            MailCertPlan::ReissueSite => {
                format!("re-issuing the {primary} certificate so it also covers {host}")
            }
            _ => format!("ordering a standalone certificate for {host}"),
        });
        let acme_email = resolve_acme_email(&panel_db, &cfg);
        if !run_acme_preflight(&state_clone, &names, &task).await {
            task.finish(false);
            return;
        }
        match issue_for_names(&cfg, &acme_email, &primary, &names, &task).await {
            Ok(()) => {
                if plan == MailCertPlan::ReissueSite {
                    let _ = exec::run("systemctl", ["reload", "nginx"]).await;
                }
                match regenerate_mail_sni(&state_clone).await {
                    Ok(()) => task.log(format!(
                        "postfix and dovecot now present the new certificate for {host}"
                    )),
                    Err(e) => task.log(format!("mail sni regen warning: {e}")),
                }
                task.finish(true);
            }
            Err(e) => {
                task.log(format!("error: {e}"));
                task.finish(false);
            }
        }
    });
    Some(task_id)
}

/// Regenerate the Postfix and Dovecot SNI maps so each `mail.<tenant>`
/// hostname is served the tenant's own cert (per-tenant SAN strategy). The
/// panel-host cert wired by `apply_mail_cert` remains the fallback for
/// clients that don't send SNI.
///
/// Idempotent — safe to call on every cert success and on every mail-domain
/// create. If no mail tenant has a usable cert yet, the maps are written
/// empty and Postfix/Dovecot keep using the fallback cert only.
pub(crate) async fn regenerate_mail_sni(state: &AppState) -> AgentResult<()> {
    let Some(sni) = build_mail_sni(state).await else {
        return Ok(());
    };
    write_mail_sni(&sni).await
}

/// Daily check from the renewal loop: rewrite and reload only when the wiring
/// would change, or when a wired cert file was replaced after the Postfix map
/// was last built. `postmap -F` embeds the PEM contents and Dovecot reads
/// certs only on reload, so a renewal that did not go through
/// `regenerate_mail_sni` (certbot's own timer) would otherwise keep being
/// served stale until it expired. Returns whether it rewrote the maps.
pub(crate) async fn refresh_mail_sni_if_stale(state: &AppState) -> AgentResult<bool> {
    let Some(sni) = build_mail_sni(state).await else {
        return Ok(false);
    };
    let current = tokio::fs::read_to_string(DOVECOT_SNI_CONF).await.unwrap_or_default();
    let mut stale = current != sni.dovecot
        || tokio::fs::metadata(LEGACY_DOVECOT_SNI_CONF).await.is_ok();
    if !stale && sni.wired > 0 {
        let map_built = tokio::fs::metadata(format!("{POSTFIX_SNI_MAP}.db"))
            .await
            .and_then(|m| m.modified())
            .ok();
        match map_built {
            None => stale = true,
            Some(built) => {
                for file in &sni.files {
                    if let Ok(changed) = tokio::fs::metadata(file).await.and_then(|m| m.modified()) {
                        if changed > built {
                            stale = true;
                            break;
                        }
                    }
                }
            }
        }
    }
    if !stale {
        return Ok(false);
    }
    write_mail_sni(&sni).await?;
    Ok(true)
}

/// What the Postfix SNI map and the Dovecot `local_name` file should contain.
struct MailSni {
    postfix: String,
    dovecot: String,
    /// Every cert and key file the maps point at.
    files: Vec<PathBuf>,
    wired: u32,
}

/// None when the mail DB is unreachable. Treat that as "no mail configured
/// yet": this is best-effort and must not break the cert path on a fresh
/// install, and the files on disk are left as they are.
async fn build_mail_sni(state: &AppState) -> Option<MailSni> {
    let mut conn = state.mariadb.pool().get_conn().await.ok()?;
    if conn.query_drop("USE `mail`").await.is_err() {
        return None;
    }
    let domains: Vec<String> = conn
        .query("SELECT name FROM virtual_domains ORDER BY name")
        .await
        .unwrap_or_default();
    drop(conn);

    let mut postfix_lines = String::new();
    let mut dovecot_blocks = String::new();
    let mut files = Vec::new();
    let mut wired = 0u32;
    for domain in &domains {
        let Some((full, key)) = mail_cert_pair_for(domain).await else {
            // A cert that does not list mail.<d> must not be wired: the
            // client would get a name mismatch. Falling back to the panel
            // host default at least fails honestly, and the Domains page
            // shows an Issue SSL button for the mail row.
            tracing::info!(
                tenant = %domain,
                "mail sni: no certificate covers mail.{domain}; keeping the default cert for it"
            );
            continue;
        };
        let host = format!("mail.{domain}");
        let full_str = full.to_string_lossy();
        let key_str = key.to_string_lossy();
        postfix_lines.push_str(&format!("{host} {key_str} {full_str}\n"));
        dovecot_blocks.push_str(&format!(
            "local_name {host} {{\n  ssl_cert = <{full_str}\n  ssl_key = <{key_str}\n}}\n"
        ));
        files.push(full);
        files.push(key);
        wired += 1;
    }
    Some(MailSni {
        postfix: postfix_lines,
        dovecot: dovecot_blocks,
        files,
        wired,
    })
}

async fn write_mail_sni(sni: &MailSni) -> AgentResult<()> {
    let wired = sni.wired;
    // Write atomically even when empty: clears stale entries cleanly.
    fs_atomic::write(
        std::path::Path::new(POSTFIX_SNI_MAP),
        sni.postfix.as_bytes(),
        0o644,
    )
    .await?;
    fs_atomic::write(
        std::path::Path::new(DOVECOT_SNI_CONF),
        sni.dovecot.as_bytes(),
        0o644,
    )
    .await?;
    let _ = tokio::fs::remove_file(LEGACY_DOVECOT_SNI_CONF).await;

    // postmap -F builds the hash DB with absolute paths to cert/key alongside
    // the hostname key. Required for `tls_server_sni_maps` to actually pick
    // up the file contents at handshake time.
    if let Err(e) = exec::run("postmap", ["-F", &format!("hash:{POSTFIX_SNI_MAP}")]).await {
        tracing::warn!("postmap failed (postfix not installed?): {e}");
        return Ok(());
    }

    // Idempotent: postconf -e just rewrites the line if value already matches.
    let _ = exec::run(
        "postconf",
        ["-e", &format!("tls_server_sni_maps=hash:{POSTFIX_SNI_MAP}")],
    )
    .await;

    let _ = exec::run("systemctl", ["reload", "postfix"]).await;
    let _ = exec::run("systemctl", ["reload", "dovecot"]).await;

    tracing::info!(wired, "regenerated mail SNI maps");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_san_names;

    #[test]
    fn parses_openssl_san_output() {
        let text = "X509v3 Subject Alternative Name: \n    DNS:example.org, DNS:www.example.org\n";
        assert_eq!(parse_san_names(text), vec!["example.org", "www.example.org"]);
        assert!(parse_san_names("").is_empty());
        assert_eq!(parse_san_names("DNS:Mail.Example.COM"), vec!["mail.example.com"]);
    }
}
