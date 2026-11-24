use crate::error::{AgentError, AgentResult};
use crate::exec;
use crate::fs_atomic;
use crate::ipc::OkResponse;
use crate::nginx_template::ACME_WEBROOT;
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    Json,
};
use mysql_async::prelude::*;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

const MAIL_DB: &str = "mail";
const PDNS_DB: &str = "pdns";
const DKIM_DIR: &str = "/var/lib/rspamd/dkim";
const DKIM_SELECTOR: &str = "default";

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

static MAIL_INITIALIZED: AtomicBool = AtomicBool::new(false);
static MAIL_MIGRATED: AtomicBool = AtomicBool::new(false);
const MAIL_SCHEMA: &str = include_str!("mail_schema.sql");

fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
pub struct CreateDomain {
    pub name: String,
    /// Get a cert covering mail.<name> as part of the create (default). The
    /// panel passes false when it is about to issue the site cert itself,
    /// which carries mail.<name> as a SAN - no point ordering two certs.
    #[serde(default = "default_true")]
    pub issue_ssl: bool,
}

#[derive(Serialize)]
pub struct DomainRow {
    pub id: u32,
    pub name: String,
    pub mailboxes: u32,
    /// default | ipv4 | ipv6: the IP protocol this domain's outgoing mail
    /// prefers (mail_ip.rs).
    pub ip_preference: crate::handlers::mail_ip::Preference,
}

#[derive(Deserialize)]
pub struct CreateMailbox {
    pub email: String,
    pub password: Option<String>,
    #[serde(default = "default_quota")]
    pub quota_mb: u64,
}

fn default_quota() -> u64 {
    1024
}

#[derive(Deserialize)]
pub struct UpdateMailbox {
    pub password: Option<String>,
    pub quota_mb: Option<u64>,
}

#[derive(Serialize)]
pub struct MailboxRow {
    pub id: u32,
    pub email: String,
    pub quota_mb: u64,
}

#[derive(Serialize)]
pub struct MailboxListResp {
    pub server: String,
    pub mailboxes: Vec<MailboxRow>,
}

#[derive(Serialize)]
pub struct MailboxUsageResp {
    pub usage_bytes: u64,
    pub usage_mb: u64,
}

#[derive(Serialize)]
pub struct CreateMailboxResp {
    pub email: String,
    pub initial_password: String,
}

#[derive(Serialize)]
pub struct CreateDomainResp {
    pub name: String,
    pub dkim_status: String,
    pub records_published: bool,
    pub note: String,
    /// Task ordering a cert that covers mail.<name>, or None when one already
    /// does / the caller passed `issue_ssl: false`.
    pub ssl_task_id: Option<String>,
}

#[derive(Serialize)]
pub struct MailSslResp {
    /// None when a valid cert already covers mail.<name> (nothing to do).
    pub task_id: Option<String>,
    pub already_covered: bool,
}

pub(crate) async fn mail_conn(state: &AppState) -> AgentResult<mysql_async::Conn> {
    let mut conn = state.mariadb.pool().get_conn().await?;
    match conn.query_drop(format!("USE `{MAIL_DB}`")).await {
        Ok(()) => {
            migrate_mail_schema(&mut conn).await;
            Ok(conn)
        }
        Err(e) if is_unknown_database(&e) => {
            ensure_mail_schema(state).await?;
            let mut conn = state.mariadb.pool().get_conn().await?;
            conn.query_drop(format!("USE `{MAIL_DB}`")).await?;
            Ok(conn)
        }
        Err(e) => Err(e.into()),
    }
}

/// Columns added after the first release. `ensure_mail_schema` only runs when
/// the database is missing, so a server installed earlier never sees a changed
/// mail_schema.sql: bring its tables up to date here, once per agent start.
/// `IF NOT EXISTS` makes it a no-op afterwards.
async fn migrate_mail_schema(conn: &mut mysql_async::Conn) {
    if MAIL_MIGRATED.load(Ordering::Acquire) {
        return;
    }
    // ip_forced: the IPv6 choice was made with force, so it stands whatever
    // the domain's SPF record says (mail_ip.rs). 0 for every existing row:
    // which earlier choices were forced was never stored.
    match conn
        .query_drop(
            "ALTER TABLE virtual_domains \
             ADD COLUMN IF NOT EXISTS ip_preference VARCHAR(8) NOT NULL DEFAULT 'default', \
             ADD COLUMN IF NOT EXISTS ip_forced TINYINT(1) NOT NULL DEFAULT 0",
        )
        .await
    {
        Ok(()) => MAIL_MIGRATED.store(true, Ordering::Release),
        Err(e) => tracing::warn!(error = %e, "could not add virtual_domains.ip_preference and ip_forced"),
    }
}

fn is_unknown_database(e: &mysql_async::Error) -> bool {
    matches!(
        e,
        mysql_async::Error::Server(mysql_async::ServerError { code: 1049, .. })
    )
}

async fn ensure_mail_schema(state: &AppState) -> AgentResult<()> {
    if MAIL_INITIALIZED.load(Ordering::Acquire) {
        return Ok(());
    }
    let mut conn = state.mariadb.pool().get_conn().await?;
    conn.query_drop(format!(
        "CREATE DATABASE IF NOT EXISTS `{MAIL_DB}` \
         CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci"
    ))
    .await?;
    conn.query_drop(format!("USE `{MAIL_DB}`")).await?;
    for stmt in MAIL_SCHEMA.split(";\n") {
        let trimmed = stmt.trim();
        if trimmed.is_empty() {
            continue;
        }
        conn.query_drop(trimmed).await?;
    }
    MAIL_INITIALIZED.store(true, Ordering::Release);
    tracing::info!("mail schema bootstrapped");
    Ok(())
}

pub async fn list_domains(State(state): State<AppState>) -> AgentResult<Json<Vec<DomainRow>>> {
    let mut conn = mail_conn(&state).await?;
    let rows: Vec<(u32, String, String)> = conn
        .query("SELECT id, name, ip_preference FROM virtual_domains ORDER BY name")
        .await?;
    let mut out = Vec::new();
    for (id, name, ip_preference) in rows {
        let count: u32 = conn
            .exec_first(
                "SELECT COUNT(*) FROM virtual_users WHERE domain_id = ?",
                (id,),
            )
            .await?
            .unwrap_or(0);
        out.push(DomainRow {
            id,
            name,
            mailboxes: count,
            ip_preference: crate::handlers::mail_ip::Preference::from_db(&ip_preference),
        });
    }
    Ok(Json(out))
}

pub async fn create_domain(
    State(state): State<AppState>,
    Json(req): Json<CreateDomain>,
) -> AgentResult<Json<CreateDomainResp>> {
    let name = req.name.trim().trim_end_matches('.').to_lowercase();
    // SECURITY: validate BEFORE the name touches any filesystem path or nginx
    // template. Unlike every other domain entry point (dns::create_zone calls
    // is_valid_zone, sites::create calls is_safe_subdomain), create_domain
    // previously normalised only with trim/lowercase. The name then flows into
    //   - the nginx vhost body (`server_name mail.{name};`) and its on-disk path
    //     /etc/nginx/sites-available/mail.{name}  (newline/`{`/`}`/`/` would let
    //     an attacker inject root-run nginx directives or traverse the path), and
    //   - `openssl genrsa -out /var/lib/rspamd/dkim/{name}.default.key`  (a `../`
    //     would write a root-owned key to an arbitrary directory).
    // is_safe_subdomain is anchored and rejects '/', '..', whitespace, '{', '}',
    // and any non-FQDN byte, closing all three at once. Reachable via the public
    // MCP tool mail.create_domain and any panel session, so the agent must be the
    // enforcement point regardless of UI-side checks.
    if !crate::validate::is_safe_subdomain(&name) {
        return Err(AgentError::Invalid(
            "mail domain must be a valid FQDN (lowercase letters, digits, hyphens and dots only; no '/', '..', spaces or newlines)".into(),
        ));
    }
    let mut conn = mail_conn(&state).await?;
    conn.exec_drop(
        "INSERT INTO virtual_domains (name) VALUES (?) ON DUPLICATE KEY UPDATE id = id",
        (&name,),
    )
    .await?;
    // 0 rows when the domain was there already: the panel calls this before
    // every new mailbox.
    let created = conn.affected_rows() > 0;

    let (dkim_status, dkim_pubkey) = match ensure_dkim_key(&name).await {
        Ok(pubkey) => ("ok".to_string(), Some(pubkey)),
        Err(e) => (format!("failed: {e}"), None),
    };

    let mut records_published = false;
    let mut notes: Vec<String> = Vec::new();
    if let Some(pubkey) = dkim_pubkey {
        match publish_mail_records(&state, &name, &pubkey).await {
            Ok(true) => records_published = true,
            Ok(false) => {
                notes.push(format!(
                    "DKIM key generated. Add zone '{name}' on the Domains page to auto-publish records."
                ));
            }
            Err(e) => {
                notes.push(format!("record publish failed: {e}"));
            }
        }
    }

    // Ensure the minimal HTTP vhost for mail.<tenant> exists so that ACME
    // HTTP-01 validation succeeds when a cert renewal includes the
    // mail.<tenant> SAN. Without it, the order returns 404 on the challenge
    // path and the per-tenant cert never gets issued, which in turn leaves
    // the Postfix/Dovecot SNI maps empty and breaks IMAP/SMTP TLS for the
    // tenant. Best-effort: failure is surfaced via the `note` field so the
    // operator sees it, but doesn't fail the whole create_domain call.
    if let Err(e) = ensure_mail_http_vhost(&name).await {
        notes.push(format!("nginx mail.{name} helper vhost setup failed: {e}"));
        tracing::warn!(tenant = %name, error = %e, "mail acme helper vhost setup failed");
    }

    // Get mail.<d> a cert it can actually present. Either the tenant's site
    // cert is re-issued with the mail SAN, or a standalone mail.<d> cert is
    // ordered through the helper vhost above; the SNI maps are rewired when
    // the task lands. Skipped when the caller is about to issue the site
    // cert itself (createSiteAction passes issue_ssl=false) - that cert
    // carries mail.<d> because the tenant row now exists. Also regenerates
    // the SNI maps right away in case a covering cert already exists.
    let ssl_task_id = if req.issue_ssl {
        crate::handlers::ssl::spawn_mail_cert_task(&state, &name).await
    } else {
        let _ = crate::handlers::ssl::regenerate_mail_sni(&state).await;
        None
    };

    // On a server that sends over IPv6: is the new domain's SPF record fit
    // for it? After the records above were written, so a zone hosted here
    // already answers with them.
    if created {
        notes.extend(crate::handlers::mail_ip::domain_added(&state, &name).await);
    }

    Ok(Json(CreateDomainResp {
        name,
        dkim_status,
        records_published,
        note: notes.join(" ").trim().to_string(),
        ssl_task_id,
    }))
}

/// `POST /mail/domains/{name}/ssl` - make sure a valid cert covers
/// `mail.<name>` and Postfix/Dovecot present it. The Domains page's "Issue
/// SSL" button on a mail row calls this; it is also the repair path for a
/// tenant whose site cert was issued before the mail domain existed.
pub async fn ensure_ssl(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> AgentResult<Json<MailSslResp>> {
    let name = name.trim().trim_end_matches('.').to_lowercase();
    if !crate::validate::is_safe_subdomain(&name) {
        return Err(AgentError::Invalid("invalid mail domain".into()));
    }
    let mut conn = mail_conn(&state).await?;
    let exists: Option<u32> = conn
        .exec_first("SELECT id FROM virtual_domains WHERE name = ?", (&name,))
        .await?;
    if exists.is_none() {
        return Err(AgentError::Conflict(format!(
            "{name} is not a mail domain yet - add it on the Mailboxes page first"
        )));
    }
    drop(conn);
    // The helper vhost is what answers the HTTP-01 challenge for mail.<name>.
    ensure_mail_http_vhost(&name).await?;
    let task_id = crate::handlers::ssl::spawn_mail_cert_task(&state, &name).await;
    let already_covered = task_id.is_none()
        && crate::handlers::ssl::mail_cert_plan(&state, &name).await
            == crate::handlers::ssl::MailCertPlan::Covered;
    Ok(Json(MailSslResp {
        task_id,
        already_covered,
    }))
}

/// Idempotently install a minimal port-80-only nginx vhost for `mail.<tenant>`
/// that serves the panel's ACME webroot on the `/.well-known/acme-challenge/`
/// path. Required so Let's Encrypt's HTTP-01 challenge can validate the
/// `mail.<tenant>` SAN when a cert renewal includes it.
///
/// Mail traffic does NOT pass through nginx — postfix/dovecot bind 25/465/587/993
/// directly and serve their own cert via SNI. This vhost is purely an ACME-
/// validation surface; its only `location /` returns 404 so no real traffic
/// ever lands here.
///
/// Idempotent: when the file already exists with byte-equal content and the
/// symlink is in place, the function is a no-op (no write, no nginx reload).
///
/// Only when nothing else serves `mail.<tenant>` over HTTP:
/// - A panel site on that name (webmail) answers the challenge path itself.
///   The helper's file sorts before `site-mail.<tenant>` in sites-enabled,
///   so nginx gave the helper port 80 for the name and http://mail.<tenant>
///   was a 404 instead of the site. The helper is removed.
/// - An app on that name, or a vhost made by hand, lives in the very file the
///   helper is written to. It was replaced by the helper, taking the app
///   offline, on every certificate order and renewal check. It is left alone.
pub(crate) async fn ensure_mail_http_vhost(tenant: &str) -> AgentResult<()> {
    let body = mail_helper_vhost_body(tenant);
    let vhost_path = PathBuf::from(format!("/etc/nginx/sites-available/mail.{tenant}"));
    let vhost_link = PathBuf::from(format!("/etc/nginx/sites-enabled/mail.{tenant}"));

    let existing = tokio::fs::read(&vhost_path).await.ok();
    let is_helper = is_mail_helper_vhost(existing.as_deref());
    let site_link = format!("/etc/nginx/sites-enabled/site-mail.{tenant}");
    if tokio::fs::symlink_metadata(&site_link).await.is_ok() {
        if is_helper {
            fs_atomic::remove_if_exists(&vhost_link).await?;
            fs_atomic::remove_if_exists(&vhost_path).await?;
            if crate::handlers::nginx::test_config().await.is_ok() {
                let _ = exec::run("systemctl", ["reload", "nginx"]).await;
            }
        }
        return Ok(());
    }
    if existing.is_some() && !is_helper {
        return Ok(());
    }

    let needs_write = match &existing {
        Some(bytes) => bytes.as_slice() != body.as_bytes(),
        None => true,
    };
    let needs_link = !vhost_link.exists();

    if !needs_write && !needs_link {
        return Ok(());
    }

    if needs_write {
        fs_atomic::write(&vhost_path, body.as_bytes(), 0o644).await?;
    }
    if needs_link {
        symlink(&vhost_path, &vhost_link).map_err(AgentError::Io)?;
    }

    // If nginx -t fails, the helper file is what changed — roll it back so we
    // never leave nginx unable to reload because of our write.
    if let Err(e) = crate::handlers::nginx::test_config().await {
        if needs_link {
            let _ = fs_atomic::remove_if_exists(&vhost_link).await;
        }
        if needs_write {
            // Only remove the file if we wrote it (don't delete a pre-existing
            // file the operator may have customised).
            if existing.is_none() {
                let _ = fs_atomic::remove_if_exists(&vhost_path).await;
            }
        }
        return Err(e);
    }
    exec::run("systemctl", ["reload", "nginx"]).await?;
    Ok(())
}

/// Is this vhost file the ACME helper (any version of it), going by the
/// marker on its first line?
fn is_mail_helper_vhost(content: Option<&[u8]>) -> bool {
    content.is_some_and(|c| c.starts_with(b"# vps-acme-helper:"))
}

fn mail_helper_vhost_body(tenant: &str) -> String {
    // Marker comment matches the hand-written vhost the manual setup installs for the
    // primary VPS host so sites::list classifies this as "external" (no
    // panel-managed delete UI) consistently with the existing convention.
    format!(
        r#"# vps-acme-helper: HTTP-01 challenge for the mail.<tenant> SAN.
# Mail traffic does NOT pass through nginx — postfix/dovecot bind 25/465/587/993
# directly and serve their own cert via SNI. This vhost only exists so Let's
# Encrypt can validate the mail.<tenant> identifier over HTTP-01.
server {{
    listen 80;
    listen [::]:80;
    server_name mail.{tenant};
    location ^~ /.well-known/acme-challenge/ {{
        root {ACME_WEBROOT};
        default_type "text/plain";
        allow all;
        try_files $uri =404;
    }}
    location / {{ return 404; }}
}}
"#
    )
}

async fn ensure_dkim_key(domain: &str) -> AgentResult<String> {
    fs_atomic::ensure_dir(std::path::Path::new(DKIM_DIR), 0o750).await?;
    let priv_path = PathBuf::from(format!("{DKIM_DIR}/{domain}.{DKIM_SELECTOR}.key"));
    if !priv_path.exists() {
        exec::run(
            "openssl",
            ["genrsa", "-out", priv_path.to_str().unwrap(), "2048"],
        )
        .await?;
        fs_atomic::chmod(&priv_path, 0o640).await?;
        if let Some(uid) = lookup_uid("_rspamd") {
            if let Some(gid) = lookup_gid("_rspamd") {
                let _ = fs_atomic::chown_blocking(&priv_path, uid, gid);
            }
        }
    }
    let out = exec::run(
        "openssl",
        ["rsa", "-in", priv_path.to_str().unwrap(), "-pubout"],
    )
    .await?;
    let b64 = out
        .stdout
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect::<Vec<_>>()
        .join("");
    if b64.is_empty() {
        return Err(AgentError::Other(anyhow::anyhow!("empty pubkey")));
    }
    Ok(b64)
}

async fn publish_mail_records(
    state: &AppState,
    domain: &str,
    dkim_pubkey: &str,
) -> AgentResult<bool> {
    let mut pconn = state.mariadb.pool().get_conn().await?;
    pconn.query_drop(format!("USE `{PDNS_DB}`")).await?;
    let domain_id: Option<u32> = pconn
        .exec_first("SELECT id FROM domains WHERE name = ?", (domain,))
        .await?;
    let Some(domain_id) = domain_id else {
        return Ok(false);
    };

    let server_ip = crate::server_ip::resolve_ipv4(None, &state.config.panel_host).await;
    let server_ip_v6 = crate::server_ip::published_ipv6(None, &state.config.panel_host).await;

    let dkim_value = format!("\"v=DKIM1; k=rsa; p={dkim_pubkey}\"");
    let spf = crate::handlers::dns::format_spf(server_ip.as_deref(), server_ip_v6.as_deref());
    // Start in monitor mode. Operator ramps to quarantine/reject once DKIM+SPF
    // pass reliably for every outbound source. `reconcile_zone` writes the
    // same policy, so the two code paths agree.
    let dmarc = format!(
        "\"v=DMARC1; p=none; rua=mailto:postmaster@{domain}\""
    );

    upsert_txt_prefixed(&mut pconn, domain_id, &format!("{DKIM_SELECTOR}._domainkey.{domain}"), "v=DKIM1", &dkim_value).await?;
    upsert_txt_prefixed(&mut pconn, domain_id, domain, "v=spf1", &spf).await?;
    upsert_txt_prefixed(&mut pconn, domain_id, &format!("_dmarc.{domain}"), "v=DMARC1", &dmarc).await?;
    upsert_record(&mut pconn, domain_id, domain, "MX", &format!("mail.{domain}"), 10).await?;

    if let Some(ip) = &server_ip {
        upsert_record(&mut pconn, domain_id, &format!("mail.{domain}"), "A", ip, 0).await?;
    }
    if let Some(ip6) = &server_ip_v6 {
        upsert_record(&mut pconn, domain_id, &format!("mail.{domain}"), "AAAA", ip6, 0).await?;
    }

    Ok(true)
}

/// Upsert the TXT record whose content starts with `prefix` (`v=spf1`,
/// `v=DMARC1`, `v=DKIM1`, quoted or not). Other TXT records at the same name
/// (site verification tokens, Microsoft 365) are left alone. It used to
/// update the first TXT at the name whatever it was, so adding a mail domain
/// could overwrite a verification record or leave two SPF records.
async fn upsert_txt_prefixed(
    conn: &mut mysql_async::Conn,
    domain_id: u32,
    name: &str,
    prefix: &str,
    content: &str,
) -> AgentResult<()> {
    if crate::handlers::dns::cname_at(conn, domain_id, name).await? {
        tracing::warn!(name, "mail records: {name} is a CNAME, TXT not written");
        return Ok(());
    }
    let rows: Vec<(u32, String)> = conn
        .exec(
            "SELECT id, content FROM records WHERE domain_id = ? AND name = ? AND type = 'TXT'",
            (domain_id, name),
        )
        .await?;
    let matched = rows
        .iter()
        .find(|(_, c)| c.trim_start_matches('"').starts_with(prefix));
    match matched {
        Some((id, _)) => {
            conn.exec_drop(
                "UPDATE records SET content = ?, ttl = 3600, disabled = 0 WHERE id = ?",
                (content, *id),
            )
            .await?;
        }
        None => {
            conn.exec_drop(
                "INSERT INTO records (domain_id, name, type, content, ttl, prio, disabled)
                 VALUES (?, ?, 'TXT', ?, 3600, 0, 0)",
                (domain_id, name, content),
            )
            .await?;
        }
    }
    Ok(())
}

async fn upsert_record(
    conn: &mut mysql_async::Conn,
    domain_id: u32,
    name: &str,
    rtype: &str,
    content: &str,
    prio: u32,
) -> AgentResult<()> {
    if crate::handlers::dns::cname_at(conn, domain_id, name).await? {
        tracing::warn!(name, rtype, "mail records: {name} is a CNAME, {rtype} not written");
        return Ok(());
    }
    let existing: Option<u32> = conn
        .exec_first(
            "SELECT id FROM records WHERE domain_id = ? AND name = ? AND type = ?",
            (domain_id, name, rtype),
        )
        .await?;
    if let Some(id) = existing {
        conn.exec_drop(
            "UPDATE records SET content = ?, prio = ?, ttl = 3600, disabled = 0 WHERE id = ?",
            (content, prio, id),
        )
        .await?;
    } else {
        conn.exec_drop(
            "INSERT INTO records (domain_id, name, type, content, ttl, prio, disabled)
             VALUES (?, ?, ?, ?, 3600, ?, 0)",
            (domain_id, name, rtype, content, prio),
        )
        .await?;
    }
    Ok(())
}

#[cfg(unix)]
fn lookup_uid(name: &str) -> Option<u32> {
    nix::unistd::User::from_name(name).ok().flatten().map(|u| u.uid.as_raw())
}

#[cfg(unix)]
fn lookup_gid(name: &str) -> Option<u32> {
    nix::unistd::Group::from_name(name).ok().flatten().map(|g| g.gid.as_raw())
}

#[cfg(not(unix))]
fn lookup_uid(_name: &str) -> Option<u32> { None }
#[cfg(not(unix))]
fn lookup_gid(_name: &str) -> Option<u32> { None }

pub async fn list_mailboxes(State(state): State<AppState>) -> AgentResult<Json<MailboxListResp>> {
    let mut conn = mail_conn(&state).await?;
    let rows: Vec<(u32, String, u64)> = conn
        .query("SELECT id, email, quota_mb FROM virtual_users ORDER BY email")
        .await?;
    let mailboxes = rows
        .into_iter()
        .map(|(id, email, quota_mb)| MailboxRow { id, email, quota_mb })
        .collect();
    Ok(Json(MailboxListResp {
        server: state.config.panel_host.clone(),
        mailboxes,
    }))
}

/// Split and check a mailbox address. Its parts name the maildir
/// (`/var/vmail/<domain>/<local>`, Dovecot's `%d/%n`), so anything that could
/// step outside that directory is refused. Returns (email, local, domain),
/// lowercased.
pub(crate) fn mailbox_parts(email: &str) -> AgentResult<(String, String, String)> {
    let email = email.trim().to_lowercase();
    let Some((local, domain)) = email.split_once('@') else {
        return Err(AgentError::Invalid("email must contain @".into()));
    };
    let local_ok = !local.is_empty()
        && local.len() <= 64
        && local
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'))
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..");
    if !local_ok || !crate::validate::is_safe_subdomain(domain) {
        return Err(AgentError::Invalid("invalid email address".into()));
    }
    let (local, domain) = (local.to_string(), domain.to_string());
    Ok((email, local, domain))
}

pub async fn create_mailbox(
    State(state): State<AppState>,
    Json(req): Json<CreateMailbox>,
) -> AgentResult<Json<CreateMailboxResp>> {
    let (email, _local, domain) = mailbox_parts(&req.email)?;
    let password = req.password.unwrap_or_else(random_password);
    let hashed = doveadm_hash(&password).await?;

    let mut conn = mail_conn(&state).await?;
    let domain_id: u32 = conn
        .exec_first("SELECT id FROM virtual_domains WHERE name = ?", (&domain,))
        .await?
        .ok_or_else(|| AgentError::NotFound)?;

    // Plain INSERT: creating an address that already exists must fail, not
    // silently replace the owner's password and quota (it used to, via
    // ON DUPLICATE KEY UPDATE).
    match conn
        .exec_drop(
            "INSERT INTO virtual_users (domain_id, email, password, quota_mb) VALUES (?, ?, ?, ?)",
            (domain_id, &email, &hashed, req.quota_mb),
        )
        .await
    {
        Ok(()) => {}
        Err(mysql_async::Error::Server(mysql_async::ServerError { code: 1062, .. })) => {
            return Err(AgentError::Conflict(format!(
                "{email} already exists. Use Edit to change its password or quota."
            )));
        }
        Err(e) => return Err(e.into()),
    }

    Ok(Json(CreateMailboxResp {
        email,
        initial_password: password,
    }))
}

pub async fn update_mailbox(
    State(state): State<AppState>,
    Path(email): Path<String>,
    Json(req): Json<UpdateMailbox>,
) -> AgentResult<Json<OkResponse>> {
    let mut conn = mail_conn(&state).await?;
    if let Some(password) = req.password {
        let hashed = doveadm_hash(&password).await?;
        conn.exec_drop(
            "UPDATE virtual_users SET password = ? WHERE email = ?",
            (hashed, &email),
        )
        .await?;
    }
    if let Some(quota_mb) = req.quota_mb {
        conn.exec_drop(
            "UPDATE virtual_users SET quota_mb = ? WHERE email = ?",
            (quota_mb, &email),
        )
        .await?;
    }
    Ok(Json(OkResponse::default()))
}

pub async fn mailbox_usage(
    Path(email): Path<String>,
) -> AgentResult<Json<MailboxUsageResp>> {
    let (_email, local, domain) = mailbox_parts(&email)?;
    let path = format!("/var/vmail/{domain}/{local}");
    if !std::path::Path::new(&path).exists() {
        return Ok(Json(MailboxUsageResp {
            usage_bytes: 0,
            usage_mb: 0,
        }));
    }
    // `du -sb` prints "<bytes>\t<path>". The leading integer is the maildir
    // size in bytes, including dovecot index/control files alongside the
    // actual messages (which is the right thing to enforce against quota).
    let output = exec::run("du", ["-sb", path.as_str()]).await?;
    let usage_bytes: u64 = output
        .stdout
        .split_whitespace()
        .next()
        .and_then(|t| t.parse().ok())
        .unwrap_or(0);
    // Ceil to the next whole MB so the "+ 1 MB" floor in the UI can never
    // round down past actual usage.
    let usage_mb = (usage_bytes + 1_048_575) / 1_048_576;
    Ok(Json(MailboxUsageResp {
        usage_bytes,
        usage_mb,
    }))
}

#[derive(Serialize)]
pub struct DeleteMailboxResp {
    pub ok: bool,
    /// Files > Trash entry now holding the mailbox's maildir, restorable for
    /// 30 days. None when the mailbox had no mail on disk.
    pub maildir_trash_id: Option<String>,
    /// Forwarders removed with the mailbox: the ones on its address and the
    /// ones that delivered to it.
    pub forwarders_removed: u64,
}

/// Removes the forwarders that name a deleted mailbox, as source or as
/// destination. They used to stay: mail to the deleted address was still
/// forwarded, and a mailbox created later under the same address would have
/// received what other addresses still forwarded to the old one.
async fn remove_forwarders_of(state: &AppState, email: &str) -> AgentResult<u64> {
    let removed: Result<u64, mysql_async::Error> = async {
        let mut conn = state.mariadb.pool().get_conn().await?;
        conn.query_drop(format!("USE `{MAIL_DB}`")).await?;
        conn.exec_drop(
            "DELETE FROM virtual_aliases WHERE source = ? OR destination = ?",
            (email, email),
        )
        .await?;
        Ok(conn.affected_rows())
    }
    .await;
    removed.map_err(|e| {
        AgentError::Other(anyhow::anyhow!(
            "The mailbox {email} was deleted, but its forwarders could not be removed: {e}.              Remove the forwarders that name {email} under Forwarders on the Mailboxes page."
        ))
    })
}

pub async fn delete_mailbox(
    State(state): State<AppState>,
    Path(email): Path<String>,
) -> AgentResult<Json<DeleteMailboxResp>> {
    const DELETE_ROW: &str = "DELETE FROM virtual_users WHERE email = ?";
    let mut conn = mail_conn(&state).await?;
    let parts = mailbox_parts(&email);
    let row_email = parts.as_ref().map(|(e, _, _)| e.clone()).unwrap_or_else(|_| email.clone());

    // The row is only the login. The mail itself stays on disk, and a mailbox
    // created later under the same address would inherit it: move it to the
    // trash. An address that fails validation never gets a path built from it.
    let Ok((_, local, domain)) = parts else {
        conn.exec_drop(DELETE_ROW, (&row_email,)).await?;
        tracing::warn!(email = %email, "mailbox deleted; address not path-safe, maildir left alone");
        let forwarders_removed = remove_forwarders_of(&state, &row_email).await?;
        return Ok(Json(DeleteMailboxResp { ok: true, maildir_trash_id: None, forwarders_removed }));
    };
    let maildir = PathBuf::from(format!("/var/vmail/{domain}/{local}"));
    if !is_real_dir(&maildir).await {
        conn.exec_drop(DELETE_ROW, (&row_email,)).await?;
        let forwarders_removed = remove_forwarders_of(&state, &row_email).await?;
        return Ok(Json(DeleteMailboxResp { ok: true, maildir_trash_id: None, forwarders_removed }));
    }

    // The mail first, the login after. The trash can refuse the maildir (on
    // a mail volume of its own it has to be copied, and may not fit next to
    // the trash), and with the row deleted first the mailbox was then gone
    // while its mail stayed where the next mailbox under this address finds it.
    let trash_id = crate::handlers::files::trash_system_path(&maildir)
        .await
        .map_err(|e| mail_not_trashed(&row_email, &maildir, e))?;
    let mut deleted = conn.exec_drop(DELETE_ROW, (&row_email,)).await;
    if deleted.is_err() {
        // The connection may have gone stale while a large maildir was copied.
        if let Ok(mut fresh) = mail_conn(&state).await {
            deleted = fresh.exec_drop(DELETE_ROW, (&row_email,)).await;
        }
    }
    if let Err(e) = deleted {
        return Err(AgentError::Other(anyhow::anyhow!(
            "The mail of {row_email} is in the trash now (Files > Trash, entry {trash_id}), but the mailbox itself could not be deleted: {e}. \
             It still exists and can still log in, without its mail. Delete it again to finish, or restore its mail from Files > Trash."
        )));
    }
    // Mail that arrived between the two steps made Dovecot create the folder
    // again. Nothing more can arrive now.
    if is_real_dir(&maildir).await {
        match crate::handlers::files::trash_system_path(&maildir).await {
            Ok(id) => tracing::info!(email = %row_email, trash_id = %id, "mail delivered while the mailbox was being deleted moved to trash"),
            Err(e) => tracing::warn!(email = %row_email, error = %e, "mail delivered while the mailbox was being deleted is still at {}", maildir.display()),
        }
    }
    tracing::info!(email = %row_email, trash_id = %trash_id, "mailbox deleted; maildir moved to trash");
    let forwarders_removed = remove_forwarders_of(&state, &row_email).await?;
    Ok(Json(DeleteMailboxResp {
        ok: true,
        maildir_trash_id: Some(trash_id),
        forwarders_removed,
    }))
}

async fn is_real_dir(path: &std::path::Path) -> bool {
    tokio::fs::symlink_metadata(path)
        .await
        .map(|m| m.file_type().is_dir())
        .unwrap_or(false)
}

/// The maildir did not go to the trash, so nothing was deleted: say so. The
/// trash's own refusal is a sentence for the operator and keeps its status.
fn mail_not_trashed(email: &str, maildir: &std::path::Path, e: AgentError) -> AgentError {
    match e {
        AgentError::Conflict(why) => {
            let why = why.trim();
            let stop = if why.ends_with(['.', '!', '?']) { "" } else { "." };
            AgentError::Conflict(format!("{why}{stop} The mailbox {email} was not deleted."))
        }
        e => AgentError::Other(anyhow::anyhow!(
            "The mail of {email} at {} could not be moved to the trash, so the mailbox was not deleted: {e}",
            maildir.display()
        )),
    }
}

async fn doveadm_hash(password: &str) -> AgentResult<String> {
    // Produces a SHA512-CRYPT hash in the same format that
    // `doveadm pw -s SHA512-CRYPT` emits, e.g. `{SHA512-CRYPT}$6$rounds=5000$salt$hash`.
    // Done in-process so the agent doesn't require Dovecot to be installed
    // just to create a mailbox row.
    let password = password.to_string();
    tokio::task::spawn_blocking(move || {
        let params = sha_crypt::Sha512Params::new(5000)
            .map_err(|e| AgentError::Other(anyhow::anyhow!("sha512 params: {e:?}")))?;
        let hash = sha_crypt::sha512_simple(&password, &params)
            .map_err(|e| AgentError::Other(anyhow::anyhow!("sha512 hash: {e:?}")))?;
        Ok(format!("{{SHA512-CRYPT}}{hash}"))
    })
    .await
    .map_err(|e| AgentError::Other(anyhow::anyhow!("join error: {e}")))?
}

fn random_password() -> String {
    use base64::Engine;
    let mut bytes = [0u8; 18];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::{is_mail_helper_vhost, mail_helper_vhost_body, mail_not_trashed, mailbox_parts};
    use crate::error::AgentError;

    #[test]
    fn a_maildir_the_trash_did_not_take_leaves_the_mailbox_and_says_so() {
        let maildir = std::path::Path::new("/var/vmail/example.com/jane");
        // The trash's own refusal keeps its words and its status (409).
        let refused = AgentError::Conflict(
            "jane is 12 GB and the disk the trash is on has 3 GB free".into(),
        );
        match mail_not_trashed("jane@example.com", maildir, refused) {
            AgentError::Conflict(msg) => assert_eq!(
                msg,
                "jane is 12 GB and the disk the trash is on has 3 GB free. The mailbox jane@example.com was not deleted."
            ),
            other => panic!("expected a conflict, got {other}"),
        }
        match mail_not_trashed("jane@example.com", maildir, AgentError::Conflict("No room. ".into())) {
            AgentError::Conflict(msg) => {
                assert_eq!(msg, "No room. The mailbox jane@example.com was not deleted.")
            }
            other => panic!("expected a conflict, got {other}"),
        }
        // Anything else that went wrong with the move.
        let failed = AgentError::Io(std::io::Error::other("No space left on device"));
        let msg = mail_not_trashed("jane@example.com", maildir, failed).to_string();
        assert!(msg.contains("/var/vmail/example.com/jane could not be moved to the trash"), "{msg}");
        assert!(msg.contains("so the mailbox was not deleted"), "{msg}");
        assert!(msg.contains("No space left on device"), "{msg}");
    }

    #[test]
    fn only_the_acme_helper_is_recognised_as_the_helper() {
        assert!(is_mail_helper_vhost(Some(mail_helper_vhost_body("example.com").as_bytes())));
        // An app deployed on mail.<tenant> has the same file name.
        assert!(!is_mail_helper_vhost(Some(b"# vps-app:webmail\nserver {\n}\n")));
        assert!(!is_mail_helper_vhost(Some(b"server {\n    server_name mail.example.com;\n}\n")));
        assert!(!is_mail_helper_vhost(None));
    }

    #[test]
    fn mailbox_parts_lowercases_real_addresses() {
        let (email, local, domain) = mailbox_parts(" Jane.Doe@59example.com ").unwrap();
        assert_eq!(email, "jane.doe@59example.com");
        assert_eq!(local, "jane.doe");
        assert_eq!(domain, "59example.com");
        assert!(mailbox_parts("info+news@example.org").is_ok());
        assert!(mailbox_parts("customer_support-2@mail.example.co.uk").is_ok());
    }

    #[test]
    fn mailbox_parts_refuses_anything_path_like() {
        for bad in [
            "../x@a.com", "a/b@a.com", "..@a.com", ".a@a.com", "a.@a.com", "a..b@a.com",
            "a@../etc", "a@a.com/..", "a@b", "noat", "@a.com", "a@", "a b@a.com",
        ] {
            assert!(mailbox_parts(bad).is_err(), "{bad} should be refused");
        }
    }
}
