use crate::error::{AgentError, AgentResult};
use crate::exec;
use crate::ipc::OkResponse;
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    Json,
};
use mysql_async::prelude::*;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Serialize)]
pub struct ZoneRow {
    pub id: u32,
    pub name: String,
    pub r#type: String,
    pub records: u32,
}

#[derive(Deserialize)]
pub struct CreateZone {
    pub name: String,
    #[serde(default)]
    pub seed_ip: Option<String>,
}

#[derive(Serialize)]
pub struct RecordRow {
    pub id: u32,
    pub name: String,
    pub r#type: String,
    pub content: String,
    pub ttl: u32,
    pub prio: u32,
    pub disabled: bool,
}

#[derive(Deserialize)]
pub struct CreateRecord {
    pub name: String,
    pub r#type: String,
    pub content: String,
    #[serde(default = "default_ttl")]
    pub ttl: u32,
    #[serde(default)]
    pub prio: u32,
}

fn default_ttl() -> u32 {
    3600
}

#[derive(Deserialize)]
pub struct UpdateRecord {
    pub content: Option<String>,
    pub ttl: Option<u32>,
    pub disabled: Option<bool>,
    pub prio: Option<u32>,
    /// New record name. Accepts the same forms as `CreateRecord.name`:
    ///   - "@" or empty → the zone apex
    ///   - already ends in `<zone>` → used verbatim
    ///   - a label → `<label>.<zone>` is stored
    /// The zone is looked up from the record's existing `domain_id`, so the
    /// caller doesn't need to supply it (and can't accidentally move a
    /// record across zones from this endpoint).
    pub name: Option<String>,
    /// New record type (e.g. switch A → CNAME). Validated against the same
    /// allow-list as `CreateRecord.type`. The content field is independent;
    /// callers that change A → CNAME should also send `content` so the cell
    /// value isn't a stale IP string.
    pub r#type: Option<String>,
}

const PDNS_DB: &str = "pdns";
const DKIM_DIR: &str = "/var/lib/rspamd/dkim";
const DKIM_SELECTOR: &str = "default";

static PDNS_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Build the canonical SPF TXT content for a zone given the resolved server
/// IPs. Always hard-fail (`-all`) — operators relax to `~all` by editing the
/// record manually if they need to. The `mx` mechanism stays so receivers
/// still accept mail from whatever MX is published; the explicit `ip4:` and
/// `ip6:` mechanisms anchor the policy to the panel's actual addresses so
/// reputation services and forwarders can attribute the traffic correctly.
pub(crate) fn format_spf(v4: Option<&str>, v6: Option<&str>) -> String {
    let mut parts = String::from("v=spf1 mx");
    if let Some(ip) = v4 {
        parts.push_str(" ip4:");
        parts.push_str(ip);
    }
    if let Some(ip) = v6 {
        parts.push_str(" ip6:");
        parts.push_str(ip);
    }
    parts.push_str(" -all");
    format!("\"{parts}\"")
}

const PDNS_SCHEMA: &str = include_str!("pdns_schema.sql");

/// Reads the `dns.ns_pair` setting from `panel.db`. If set as
/// `ns1.host,ns2.host`, returns the parsed pair (lowercased, no trailing dot).
/// If unset, malformed, or empty, returns None — callers fall back to the
/// per-zone default (`ns1.<zone>`, `ns2.<zone>`).
pub(crate) fn resolve_ns_pair(panel_db: &crate::db::PanelDb) -> Option<(String, String)> {
    let raw: String = panel_db
        .with(|conn| {
            conn.query_row(
                "SELECT value FROM settings WHERE key = 'dns.ns_pair'",
                [],
                |row| row.get::<_, String>(0),
            )
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(String::new()),
                other => Err(other),
            })
        })
        .ok()?;
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let parts: Vec<&str> = raw.split(',').map(|s| s.trim()).collect();
    if parts.len() != 2 || parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    Some((
        parts[0].trim_end_matches('.').to_lowercase(),
        parts[1].trim_end_matches('.').to_lowercase(),
    ))
}

/// Returns the NS pair to use for a given zone — the configured shared pair
/// if set, otherwise the per-zone default.
fn ns_pair_for(zone: &str, panel_db: &crate::db::PanelDb) -> (String, String) {
    resolve_ns_pair(panel_db).unwrap_or_else(|| (format!("ns1.{zone}"), format!("ns2.{zone}")))
}

/// True if the NS hostname lives inside this zone (and therefore needs an
/// in-zone A record). False for an external NS host like
/// `ns1.example.net` when reconciling `example.com`.
fn ns_in_zone(ns_host: &str, zone: &str) -> bool {
    ns_host == zone || ns_host.ends_with(&format!(".{zone}"))
}

async fn pdns_conn(state: &AppState) -> AgentResult<mysql_async::Conn> {
    let mut conn = state.mariadb.pool().get_conn().await?;
    match conn.query_drop(format!("USE `{PDNS_DB}`")).await {
        Ok(()) => Ok(conn),
        Err(e) if is_unknown_database(&e) => {
            ensure_pdns_schema(state).await?;
            let mut conn = state.mariadb.pool().get_conn().await?;
            conn.query_drop(format!("USE `{PDNS_DB}`")).await?;
            Ok(conn)
        }
        Err(e) => Err(e.into()),
    }
}

fn is_unknown_database(e: &mysql_async::Error) -> bool {
    matches!(
        e,
        mysql_async::Error::Server(mysql_async::ServerError { code: 1049, .. })
    )
}

async fn ensure_pdns_schema(state: &AppState) -> AgentResult<()> {
    if PDNS_INITIALIZED.load(Ordering::Acquire) {
        return Ok(());
    }
    let mut conn = state.mariadb.pool().get_conn().await?;
    conn.query_drop(format!(
        "CREATE DATABASE IF NOT EXISTS `{PDNS_DB}` \
         CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci"
    ))
    .await?;
    conn.query_drop(format!("USE `{PDNS_DB}`")).await?;
    for stmt in PDNS_SCHEMA.split(";\n") {
        let trimmed = stmt.trim();
        if trimmed.is_empty() {
            continue;
        }
        conn.query_drop(trimmed).await?;
    }
    PDNS_INITIALIZED.store(true, Ordering::Release);
    tracing::info!("pdns schema bootstrapped");
    Ok(())
}

/// Just the names of managed PowerDNS zones — cheap helper for other handlers
/// that need to check parent-zone existence without paying for the per-zone
/// record-count query that `list_zones` does.
pub(crate) async fn list_zone_names(state: &AppState) -> AgentResult<Vec<String>> {
    let mut conn = pdns_conn(state).await?;
    let rows: Vec<String> = conn
        .query("SELECT name FROM domains ORDER BY name")
        .await?;
    Ok(rows)
}

pub async fn list_zones(State(state): State<AppState>) -> AgentResult<Json<Vec<ZoneRow>>> {
    let mut conn = pdns_conn(&state).await?;
    let rows: Vec<(u32, String, String)> = conn
        .query("SELECT id, name, type FROM domains ORDER BY name")
        .await?;
    let mut zones = Vec::with_capacity(rows.len());
    for (id, name, ty) in rows {
        let count: u32 = conn
            .exec_first("SELECT COUNT(*) FROM records WHERE domain_id = ?", (id,))
            .await?
            .unwrap_or(0);
        zones.push(ZoneRow {
            id,
            name,
            r#type: ty,
            records: count,
        });
    }
    Ok(Json(zones))
}

pub async fn create_zone(
    State(state): State<AppState>,
    Json(req): Json<CreateZone>,
) -> AgentResult<Json<ZoneRow>> {
    create_zone_inner(&state, req).await.map(Json)
}

/// Programmatic counterpart to `create_zone` for in-agent callers (e.g.
/// the panel-host setup task in `system.rs`). Same behavior, no Axum
/// extractors.
pub(crate) async fn create_zone_inner(
    state: &AppState,
    req: CreateZone,
) -> AgentResult<ZoneRow> {
    let name = req.name.trim().trim_end_matches('.').to_lowercase();
    if !is_valid_zone(&name) {
        return Err(AgentError::Invalid("invalid zone name".into()));
    }
    let mut conn = pdns_conn(state).await?;
    conn.exec_drop(
        "INSERT INTO domains (name, type) VALUES (?, 'NATIVE')",
        (&name,),
    )
    .await?;
    let id: u32 = conn
        .exec_first("SELECT id FROM domains WHERE name = ?", (&name,))
        .await?
        .ok_or_else(|| AgentError::Other(anyhow::anyhow!("zone not found after insert")))?;

    let (ns1, ns2) = ns_pair_for(&name, &state.panel_db);
    let soa_content = format!(
        "{ns1} hostmaster.{name} {} 10800 3600 604800 3600",
        chrono::Utc::now().format("%Y%m%d%H")
    );
    insert_record(&mut conn, id, &name, "SOA", &soa_content, 0).await?;
    insert_record(&mut conn, id, &name, "NS", &ns1, 0).await?;
    insert_record(&mut conn, id, &name, "NS", &ns2, 0).await?;

    let mut records = 3u32;
    let server_ip = crate::server_ip::resolve_ipv4(
        req.seed_ip.as_deref(),
        &state.config.panel_host,
    )
    .await;
    let server_ip_v6 = crate::server_ip::resolve_ipv6(
        req.seed_ip.as_deref(),
        &state.config.panel_host,
    )
    .await;

    if let Some(ip) = &server_ip {
        insert_record(&mut conn, id, &name, "A", ip, 0).await?;
        insert_record(&mut conn, id, &format!("www.{name}"), "A", ip, 0).await?;
        insert_record(&mut conn, id, &format!("mail.{name}"), "A", ip, 0).await?;
        records += 3;
        // Only seed A records for the NS hostnames when they live inside
        // this zone (per-zone NS mode). With a shared NS pair (e.g.
        // ns1.example.net), the A records belong in that other
        // zone, not this one.
        if ns_in_zone(&ns1, &name) {
            insert_record(&mut conn, id, &ns1, "A", ip, 0).await?;
            records += 1;
        }
        if ns_in_zone(&ns2, &name) {
            insert_record(&mut conn, id, &ns2, "A", ip, 0).await?;
            records += 1;
        }
        insert_record(
            &mut conn,
            id,
            &name,
            "CAA",
            "0 issue \"letsencrypt.org\"",
            0,
        )
        .await?;
        records += 1;
    } else {
        tracing::warn!(
            zone = %name,
            "no server IPv4 resolved; seeding only SOA + NS records"
        );
    }

    if let Some(ip6) = &server_ip_v6 {
        insert_record(&mut conn, id, &name, "AAAA", ip6, 0).await?;
        insert_record(&mut conn, id, &format!("www.{name}"), "AAAA", ip6, 0).await?;
        insert_record(&mut conn, id, &format!("mail.{name}"), "AAAA", ip6, 0).await?;
        records += 3;
        if ns_in_zone(&ns1, &name) {
            insert_record(&mut conn, id, &ns1, "AAAA", ip6, 0).await?;
            records += 1;
        }
        if ns_in_zone(&ns2, &name) {
            insert_record(&mut conn, id, &ns2, "AAAA", ip6, 0).await?;
            records += 1;
        }
    }

    Ok(ZoneRow {
        id,
        name,
        r#type: "NATIVE".into(),
        records,
    })
}

/// Types of the records already at `name` in the zone, except `exclude_id`.
async fn types_at(
    conn: &mut mysql_async::Conn,
    domain_id: u32,
    name: &str,
    exclude_id: Option<u32>,
) -> AgentResult<Vec<String>> {
    let rows: Vec<(u32, String)> = conn
        .exec(
            "SELECT id, type FROM records WHERE domain_id = ? AND name = ?",
            (domain_id, name),
        )
        .await?;
    Ok(rows
        .into_iter()
        .filter(|(id, _)| Some(*id) != exclude_id)
        .map(|(_, t)| t.to_uppercase())
        .collect())
}

/// Is `name` a CNAME? Automatic writers (SSL preflight A/AAAA, reconcile,
/// mail records) skip such a name instead of adding a record next to it.
pub(crate) async fn cname_at(
    conn: &mut mysql_async::Conn,
    domain_id: u32,
    name: &str,
) -> AgentResult<bool> {
    Ok(types_at(conn, domain_id, name, None)
        .await?
        .iter()
        .any(|t| t == "CNAME"))
}

/// DNS allows a CNAME only as the single record at its name, and never at the
/// zone apex, where the SOA and NS records live. The panel accepted both: a
/// CNAME added next to a domain's apex MX and SPF (2026-09) made
/// resolvers follow it and silently rerouted that domain's mail. Refuses a
/// write that would break the rule, for the record `exclude_id` (an edit)
/// or a new one (None).
async fn check_cname_rules(
    conn: &mut mysql_async::Conn,
    domain_id: u32,
    zone: &str,
    name: &str,
    rtype: &str,
    exclude_id: Option<u32>,
) -> AgentResult<()> {
    let others = types_at(conn, domain_id, name, exclude_id).await?;
    if rtype == "CNAME" {
        if name == zone {
            return Err(AgentError::Invalid(format!(
                "a CNAME cannot be at the zone apex ({zone}): it would hide the zone's SOA, NS, MX and TXT records. \
                 Use A/AAAA records, or a redirect to move a website"
            )));
        }
        if !others.is_empty() {
            return Err(AgentError::Invalid(format!(
                "{name} already has {} record(s); a CNAME must be the only record at its name",
                others.join(", ")
            )));
        }
    } else if others.iter().any(|t| t == "CNAME") {
        return Err(AgentError::Invalid(format!(
            "{name} is a CNAME; remove it before adding a {rtype} record there"
        )));
    }
    Ok(())
}

async fn insert_record(
    conn: &mut mysql_async::Conn,
    domain_id: u32,
    name: &str,
    rtype: &str,
    content: &str,
    prio: u32,
) -> AgentResult<()> {
    conn.exec_drop(
        "INSERT INTO records (domain_id, name, type, content, ttl, prio, disabled)
         VALUES (?, ?, ?, ?, 3600, ?, 0)",
        (domain_id, name, rtype, content, prio),
    )
    .await?;
    Ok(())
}

#[derive(Default, Serialize)]
pub struct ReconcileResp {
    pub added: Vec<String>,
    pub updated: Vec<String>,
    pub unchanged: Vec<String>,
    pub non_standard_extras: Vec<String>,
}

/// Bring an existing PowerDNS zone up to the panel's standard shape.
///
/// Additive only — never deletes user-added records. Records that don't fit
/// the standard template are reported under `non_standard_extras` so the
/// operator can clean them up by hand if they want.
///
/// Targets the same record set `create_zone` seeds, plus the per-mail-domain
/// DKIM/SPF/DMARC/MX rows if a row exists in `mail.virtual_domains` for the
/// zone.
pub async fn reconcile_zone(
    State(state): State<AppState>,
    Path(zone): Path<String>,
) -> AgentResult<Json<ReconcileResp>> {
    let zone = zone.trim_end_matches('.').to_lowercase();
    if !is_valid_zone(&zone) {
        return Err(AgentError::Invalid("invalid zone".into()));
    }
    let mut conn = pdns_conn(&state).await?;
    let domain_id: u32 = conn
        .exec_first("SELECT id FROM domains WHERE name = ?", (&zone,))
        .await?
        .ok_or(AgentError::NotFound)?;

    let (ns1, ns2) = ns_pair_for(&zone, &state.panel_db);
    let server_ip = crate::server_ip::resolve_ipv4(None, &state.config.panel_host).await;
    let server_ip_v6 = crate::server_ip::resolve_ipv6(None, &state.config.panel_host).await;
    let has_mail = mail_domain_exists(&mut conn, &zone).await;
    let dkim_pubkey = if has_mail {
        read_dkim_pubkey(&zone).await.ok()
    } else {
        None
    };

    let mut report = ReconcileResp::default();

    // SOA — single record per zone, identify by type alone. Only consider it
    // changed if the primary NS field differs from target; otherwise leave
    // the existing serial alone so resolvers don't see spurious bumps.
    let soa_target_primary = ns1.as_str();
    let existing_soa: Option<(u32, String)> = conn
        .exec_first(
            "SELECT id, content FROM records WHERE domain_id = ? AND type = 'SOA' LIMIT 1",
            (domain_id,),
        )
        .await?;
    let soa_target = format!(
        "{} hostmaster.{} {} 10800 3600 604800 3600",
        soa_target_primary,
        zone,
        chrono::Utc::now().format("%Y%m%d%H")
    );
    match existing_soa {
        Some((id, content)) => {
            let existing_primary = content
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_end_matches('.')
                .to_lowercase();
            if existing_primary == soa_target_primary {
                report.unchanged.push(format!("SOA {zone}"));
            } else {
                conn.exec_drop(
                    "UPDATE records SET content = ?, ttl = 3600, disabled = 0 WHERE id = ?",
                    (&soa_target, id),
                )
                .await?;
                report.updated.push(format!("SOA {zone}"));
            }
        }
        None => {
            insert_record(&mut conn, domain_id, &zone, "SOA", &soa_target, 0).await?;
            report.added.push(format!("SOA {zone}"));
        }
    }

    // NS — there are two at the apex, identify by content. Insert any that
    // are missing; leave any extras alone (they surface in non_standard_extras).
    let existing_ns_contents: Vec<String> = conn
        .exec(
            "SELECT content FROM records WHERE domain_id = ? AND name = ? AND type = 'NS'",
            (domain_id, &zone),
        )
        .await?;
    let existing_ns_lower: Vec<String> = existing_ns_contents
        .iter()
        .map(|c| c.trim_end_matches('.').to_lowercase())
        .collect();
    for ns_host in [&ns1, &ns2] {
        if existing_ns_lower.iter().any(|c| c == ns_host) {
            report.unchanged.push(format!("NS {zone} → {ns_host}"));
        } else {
            insert_record(&mut conn, domain_id, &zone, "NS", ns_host, 0).await?;
            report.added.push(format!("NS {zone} → {ns_host}"));
        }
    }

    // A / AAAA records — apex, www, mail, and ns1/ns2 if they're in-zone.
    // Compute the target name list once; A and AAAA share it.
    let mut addr_targets: Vec<String> = vec![
        zone.clone(),
        format!("www.{zone}"),
        format!("mail.{zone}"),
    ];
    if ns_in_zone(&ns1, &zone) {
        addr_targets.push(ns1.clone());
    }
    if ns_in_zone(&ns2, &zone) {
        addr_targets.push(ns2.clone());
    }
    if let Some(ip) = &server_ip {
        for name in &addr_targets {
            upsert_single(&mut conn, domain_id, name, "A", ip, 0, &mut report).await?;
        }
    }
    if let Some(ip6) = &server_ip_v6 {
        for name in &addr_targets {
            upsert_single(&mut conn, domain_id, name, "AAAA", ip6, 0, &mut report).await?;
        }
    }

    // CAA at apex.
    upsert_single(
        &mut conn,
        domain_id,
        &zone,
        "CAA",
        "0 issue \"letsencrypt.org\"",
        0,
        &mut report,
    )
    .await?;

    // Mail records (only if this zone has a mail domain configured).
    if has_mail {
        upsert_single(
            &mut conn,
            domain_id,
            &zone,
            "MX",
            &format!("mail.{zone}"),
            10,
            &mut report,
        )
        .await?;
        let spf_target = format_spf(server_ip.as_deref(), server_ip_v6.as_deref());
        upsert_txt_by_prefix(
            &mut conn,
            domain_id,
            &zone,
            "\"v=spf1",
            &spf_target,
            &mut report,
        )
        .await?;
        let dmarc_target = format!(
            "\"v=DMARC1; p=none; rua=mailto:postmaster@{zone}\""
        );
        upsert_txt_by_prefix(
            &mut conn,
            domain_id,
            &format!("_dmarc.{zone}"),
            "\"v=DMARC1",
            &dmarc_target,
            &mut report,
        )
        .await?;
        if let Some(pubkey) = &dkim_pubkey {
            let dkim_target = format!("\"v=DKIM1; k=rsa; p={pubkey}\"");
            upsert_txt_by_prefix(
                &mut conn,
                domain_id,
                &format!("{DKIM_SELECTOR}._domainkey.{zone}"),
                "\"v=DKIM1",
                &dkim_target,
                &mut report,
            )
            .await?;
        }
    }

    // Walk all records and collect anything that doesn't match a known
    // standard slot. Informational — left in the DB untouched.
    let all: Vec<(String, String, String)> = conn
        .exec(
            "SELECT name, type, content FROM records \
             WHERE domain_id = ? AND type IS NOT NULL \
             ORDER BY name, type",
            (domain_id,),
        )
        .await?;
    for (name, rtype, content) in &all {
        if !is_standard_record(name, rtype, content, &zone, &ns1, &ns2, has_mail) {
            let truncated = if content.len() > 80 {
                format!("{}…", &content[..80])
            } else {
                content.clone()
            };
            report
                .non_standard_extras
                .push(format!("{rtype} {name} → {truncated}"));
        }
    }

    Ok(Json(report))
}

async fn mail_domain_exists(conn: &mut mysql_async::Conn, zone: &str) -> bool {
    // The `mail` database may not exist yet on a fresh install — treat any
    // error (unknown DB, no row) as "no mail domain". We're still inside the
    // pdns DB context after pdns_conn(), so the cross-database query needs
    // the explicit `mail.` prefix.
    let res: Result<Option<u32>, _> = conn
        .exec_first(
            "SELECT id FROM `mail`.`virtual_domains` WHERE name = ?",
            (zone,),
        )
        .await;
    matches!(res, Ok(Some(_)))
}

async fn read_dkim_pubkey(zone: &str) -> AgentResult<String> {
    let path = std::path::PathBuf::from(format!("{DKIM_DIR}/{zone}.{DKIM_SELECTOR}.key"));
    if !path.exists() {
        return Err(AgentError::Other(anyhow::anyhow!(
            "no DKIM key at {}",
            path.display()
        )));
    }
    let out = exec::run("openssl", ["rsa", "-in", path.to_str().unwrap(), "-pubout"]).await?;
    let b64 = out
        .stdout
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect::<Vec<_>>()
        .join("");
    if b64.is_empty() {
        return Err(AgentError::Other(anyhow::anyhow!("empty DKIM pubkey")));
    }
    Ok(b64)
}

/// Upsert a record where (name, type) is unique in the zone — SOA-like, but
/// generalized to A / CAA / MX. Tracks the outcome on the supplied report.
async fn upsert_single(
    conn: &mut mysql_async::Conn,
    domain_id: u32,
    name: &str,
    rtype: &str,
    content: &str,
    prio: u32,
    report: &mut ReconcileResp,
) -> AgentResult<()> {
    if rtype != "CNAME" && cname_at(conn, domain_id, name).await? {
        report
            .non_standard_extras
            .push(format!("{rtype} {name} not written: {name} is a CNAME"));
        return Ok(());
    }
    let existing: Option<(u32, String, u32)> = conn
        .exec_first(
            "SELECT id, content, prio FROM records \
             WHERE domain_id = ? AND name = ? AND type = ? \
             LIMIT 1",
            (domain_id, name, rtype),
        )
        .await?;
    let label = format!("{rtype} {name}");
    match existing {
        Some((id, existing_content, existing_prio)) => {
            if existing_content == content && existing_prio == prio {
                report.unchanged.push(label);
            } else {
                conn.exec_drop(
                    "UPDATE records SET content = ?, prio = ?, ttl = 3600, disabled = 0 \
                     WHERE id = ?",
                    (content, prio, id),
                )
                .await?;
                report.updated.push(label);
            }
        }
        None => {
            insert_record(conn, domain_id, name, rtype, content, prio).await?;
            report.added.push(label);
        }
    }
    Ok(())
}

/// Upsert a TXT record identified by its content prefix (e.g. `"v=spf1`,
/// `"v=DMARC1`, `"v=DKIM1`). Lets a zone keep other TXT records on the same
/// name (M365 verification tokens, third-party verification, etc.) without
/// the reconcile clobbering them.
async fn upsert_txt_by_prefix(
    conn: &mut mysql_async::Conn,
    domain_id: u32,
    name: &str,
    prefix: &str,
    target_content: &str,
    report: &mut ReconcileResp,
) -> AgentResult<()> {
    if cname_at(conn, domain_id, name).await? {
        report
            .non_standard_extras
            .push(format!("TXT {name} not written: {name} is a CNAME"));
        return Ok(());
    }
    let candidates: Vec<(u32, String)> = conn
        .exec(
            "SELECT id, content FROM records \
             WHERE domain_id = ? AND name = ? AND type = 'TXT'",
            (domain_id, name),
        )
        .await?;
    let matched = candidates.iter().find(|(_, c)| c.starts_with(prefix));
    let label = format!("TXT {name} ({})", prefix.trim_start_matches('"'));
    match matched {
        Some((id, content)) => {
            if content == target_content {
                report.unchanged.push(label);
            } else {
                conn.exec_drop(
                    "UPDATE records SET content = ?, ttl = 3600, disabled = 0 \
                     WHERE id = ?",
                    (target_content, *id),
                )
                .await?;
                report.updated.push(label);
            }
        }
        None => {
            insert_record(conn, domain_id, name, "TXT", target_content, 0).await?;
            report.added.push(label);
        }
    }
    Ok(())
}

fn is_standard_record(
    name: &str,
    rtype: &str,
    content: &str,
    zone: &str,
    ns1: &str,
    ns2: &str,
    has_mail: bool,
) -> bool {
    let apex = zone;
    let www = format!("www.{zone}");
    let mail = format!("mail.{zone}");
    let dmarc = format!("_dmarc.{zone}");
    let dkim = format!("{DKIM_SELECTOR}._domainkey.{zone}");
    match rtype {
        "SOA" => name == apex,
        "NS" => {
            if name != apex {
                return false;
            }
            let c = content.trim_end_matches('.').to_lowercase();
            c == ns1 || c == ns2
        }
        "A" | "AAAA" => {
            name == apex || name == www || name == mail || name == ns1 || name == ns2
        }
        "CAA" => name == apex && content.contains("letsencrypt.org"),
        "MX" => has_mail && name == apex,
        "TXT" if has_mail => {
            (name == apex && content.starts_with("\"v=spf1"))
                || (name == dmarc && content.starts_with("\"v=DMARC1"))
                || (name == dkim && content.starts_with("\"v=DKIM1"))
        }
        _ => false,
    }
}

pub async fn delete_zone(
    State(state): State<AppState>,
    Path(zone): Path<String>,
) -> AgentResult<Json<OkResponse>> {
    let zone = zone.trim_end_matches('.').to_lowercase();
    let mut conn = pdns_conn(&state).await?;
    conn.exec_drop("DELETE FROM records WHERE domain_id IN (SELECT id FROM domains WHERE name = ?)", (&zone,)).await?;
    conn.exec_drop("DELETE FROM domains WHERE name = ?", (&zone,)).await?;
    Ok(Json(OkResponse::default()))
}

pub async fn sign_zone(Path(zone): Path<String>) -> AgentResult<Json<OkResponse>> {
    let zone = zone.trim_end_matches('.').to_lowercase();
    if !is_valid_zone(&zone) {
        return Err(AgentError::Invalid("invalid zone".into()));
    }
    exec::run("pdnsutil", ["secure-zone", &zone]).await?;
    exec::run("pdnsutil", ["rectify-zone", &zone]).await?;
    Ok(Json(OkResponse::default()))
}

pub async fn list_records(
    State(state): State<AppState>,
    Path(zone): Path<String>,
) -> AgentResult<Json<Vec<RecordRow>>> {
    let zone = zone.trim_end_matches('.').to_lowercase();
    let mut conn = pdns_conn(&state).await?;
    // `pdnsutil rectify-zone` (run after `load-zone`) inserts "empty
    // non-terminal" rows with NULL type+content so PowerDNS can answer NXDOMAIN
    // vs NODATA correctly. These aren't user-managed records; skip them in the
    // SQL filter, and decode type/content as Option<String> so any future NULL
    // doesn't crash the whole list endpoint (which is what caused the panel's
    // DNS records page to render empty before this fix).
    //
    // Update 2026-05-21 (second iteration): zones imported via
    // `pdnsutil load-zone` from BIND files can also leave NULL in ttl, prio,
    // or disabled on real (type IS NOT NULL) records. The mysql_async row
    // decoder still panics on NULL for non-Option columns, so decode all
    // nullable PowerDNS columns as Option<_> and fall back to sane defaults
    // (panel insert convention: ttl=3600, prio=0, disabled=0).
    let rows: Vec<(
        u32,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<u32>,
        Option<u32>,
        Option<i8>,
    )> = conn
        .exec(
            "SELECT r.id, r.name, r.type, r.content, r.ttl, r.prio, r.disabled
             FROM records r JOIN domains d ON d.id = r.domain_id
             WHERE d.name = ? AND r.type IS NOT NULL
             ORDER BY r.name, r.type",
            (&zone,),
        )
        .await?;
    Ok(Json(
        rows.into_iter()
            .filter_map(|(id, name, ty, content, ttl, prio, disabled)| {
                Some(RecordRow {
                    id,
                    name: name.unwrap_or_default(),
                    r#type: ty?,
                    content: content.unwrap_or_default(),
                    ttl: ttl.unwrap_or(3600),
                    prio: prio.unwrap_or(0),
                    disabled: disabled.unwrap_or(0) != 0,
                })
            })
            .collect(),
    ))
}

pub async fn create_record(
    State(state): State<AppState>,
    Path(zone): Path<String>,
    Json(req): Json<CreateRecord>,
) -> AgentResult<Json<RecordRow>> {
    create_record_inner(&state, zone, req).await.map(Json)
}

/// Programmatic counterpart to `create_record` for in-agent callers.
pub(crate) async fn create_record_inner(
    state: &AppState,
    zone: String,
    req: CreateRecord,
) -> AgentResult<RecordRow> {
    let zone = zone.trim_end_matches('.').to_lowercase();
    let mut conn = pdns_conn(state).await?;
    let domain_id: u32 = conn
        .exec_first("SELECT id FROM domains WHERE name = ?", (&zone,))
        .await?
        .ok_or(AgentError::NotFound)?;
    // Dot boundary: "fooexample.com" is a label inside example.com, not the
    // zone itself.
    let raw = req.name.trim().trim_end_matches('.').to_lowercase();
    let name = if raw == "@" || raw.is_empty() {
        zone.clone()
    } else if raw == zone || raw.ends_with(&format!(".{zone}")) {
        raw
    } else {
        format!("{raw}.{zone}")
    };
    let rtype = req.r#type.to_uppercase();
    if !is_valid_rtype(&rtype) {
        return Err(AgentError::Invalid("unsupported record type".into()));
    }
    check_cname_rules(&mut conn, domain_id, &zone, &name, &rtype, None).await?;
    conn.exec_drop(
        "INSERT INTO records (domain_id, name, type, content, ttl, prio, disabled) VALUES (?, ?, ?, ?, ?, ?, 0)",
        (domain_id, &name, &rtype, &req.content, req.ttl, req.prio),
    )
    .await?;
    let id: u32 = conn
        .exec_first("SELECT LAST_INSERT_ID()", ())
        .await?
        .unwrap_or(0);
    Ok(RecordRow {
        id,
        name,
        r#type: rtype,
        content: req.content,
        ttl: req.ttl,
        prio: req.prio,
        disabled: false,
    })
}

pub async fn update_record(
    State(state): State<AppState>,
    Path(id): Path<u32>,
    Json(req): Json<UpdateRecord>,
) -> AgentResult<Json<OkResponse>> {
    let mut conn = pdns_conn(&state).await?;
    // Validate everything before writing anything: a rejected field must not
    // leave the other fields half-applied.
    let current: Option<(String, String, u32, String)> = conn
        .exec_first(
            "SELECT r.type, r.name, r.domain_id, d.name
               FROM records r
               JOIN domains d ON r.domain_id = d.id
              WHERE r.id = ?",
            (id,),
        )
        .await?;
    let (current_type, current_name, domain_id, zone) = current.ok_or(AgentError::NotFound)?;
    let current_type = current_type.to_uppercase();
    let zone = zone.to_lowercase().trim_end_matches('.').to_string();

    // A zone without its SOA stops resolving, so an SOA keeps its type and its
    // place at the apex, and nothing else becomes a second SOA. The edit form
    // once had no SOA option and silently saved an SOA as an A record.
    let new_type = match req.r#type.as_deref().map(|t| t.trim().to_uppercase()) {
        Some(t) if t != current_type => {
            if !is_valid_rtype(&t) {
                return Err(AgentError::Invalid("unsupported record type".into()));
            }
            if current_type == "SOA" || t == "SOA" {
                return Err(AgentError::Invalid(
                    "an SOA record cannot change type, and no other record can become one".into(),
                ));
            }
            Some(t)
        }
        _ => None,
    };
    // The zone comes from THIS record, so the name is normalised against it
    // ("mail" → "mail.example.com", "@" → "example.com") and a mistyped name
    // can't move the record into a different zone.
    let new_name = req.name.as_deref().map(|raw| {
        let trimmed = raw.trim().trim_end_matches('.').to_lowercase();
        if trimmed == "@" || trimmed.is_empty() {
            zone.clone()
        } else if trimmed == zone || trimmed.ends_with(&format!(".{zone}")) {
            trimmed
        } else {
            format!("{trimmed}.{zone}")
        }
    });
    if current_type == "SOA" && new_name.as_deref().is_some_and(|n| n != zone) {
        return Err(AgentError::Invalid(
            "the SOA record must stay at the zone apex".into(),
        ));
    }

    if new_type.is_some() || new_name.is_some() {
        let final_name = new_name.clone().unwrap_or_else(|| current_name.to_lowercase());
        let final_type = new_type.clone().unwrap_or_else(|| current_type.clone());
        check_cname_rules(&mut conn, domain_id, &zone, &final_name, &final_type, Some(id)).await?;
    }

    let mut tx = conn.start_transaction(mysql_async::TxOpts::default()).await?;
    if let Some(content) = req.content {
        tx.exec_drop("UPDATE records SET content = ? WHERE id = ?", (content, id))
            .await?;
    }
    if let Some(ttl) = req.ttl {
        tx.exec_drop("UPDATE records SET ttl = ? WHERE id = ?", (ttl, id))
            .await?;
    }
    if let Some(disabled) = req.disabled {
        tx.exec_drop(
            "UPDATE records SET disabled = ? WHERE id = ?",
            (i8::from(disabled), id),
        )
        .await?;
    }
    if let Some(prio) = req.prio {
        tx.exec_drop("UPDATE records SET prio = ? WHERE id = ?", (prio, id))
            .await?;
    }
    if let Some(name) = new_name {
        tx.exec_drop("UPDATE records SET name = ? WHERE id = ?", (name, id))
            .await?;
    }
    if let Some(rtype) = new_type {
        tx.exec_drop("UPDATE records SET type = ? WHERE id = ?", (rtype, id))
            .await?;
    }
    tx.commit().await?;
    Ok(Json(OkResponse::default()))
}

pub async fn delete_record(
    State(state): State<AppState>,
    Path(id): Path<u32>,
) -> AgentResult<Json<OkResponse>> {
    let mut conn = pdns_conn(&state).await?;
    conn.exec_drop("DELETE FROM records WHERE id = ?", (id,))
        .await?;
    Ok(Json(OkResponse::default()))
}

fn is_valid_zone(s: &str) -> bool {
    let re = regex::Regex::new(r"^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)+$").unwrap();
    re.is_match(s)
}

fn is_valid_rtype(s: &str) -> bool {
    matches!(
        s,
        "A" | "AAAA" | "CNAME" | "MX" | "TXT" | "NS" | "SOA" | "SRV" | "CAA" | "PTR" | "TLSA"
    )
}

// ---------------------------------------------------------------------------
// ACME pre-flight helpers
//
// These let SSL issuance flows (sites.rs, ssl.rs) do the right thing before
// calling Let's Encrypt:
//   1. If the parent zone is in the panel's PowerDNS, auto-upsert the A/AAAA
//      record for the subdomain so the operator doesn't have to.
//   2. Verify public DNS for the name actually points at this server. If not,
//      fail fast with a clear message instead of letting ACME return its
//      generic NXDOMAIN error after a 60s wait.
// ---------------------------------------------------------------------------

/// Find the longest panel-managed zone that's a suffix of `domain`.
/// E.g. for `foo.bar.example.com` returns `example.com` if that's the only
/// matching zone, or `bar.example.com` if both exist (longest match wins).
pub(crate) async fn find_enclosing_zone(
    state: &AppState,
    domain: &str,
) -> Option<(String, u32)> {
    let mut conn = pdns_conn(state).await.ok()?;
    let mut name = domain.trim_end_matches('.').to_lowercase();
    loop {
        let id: Option<u32> = conn
            .exec_first("SELECT id FROM domains WHERE name = ?", (&name,))
            .await
            .ok()
            .flatten();
        if let Some(id) = id {
            return Some((name, id));
        }
        match name.find('.') {
            Some(i) if i + 1 < name.len() => name = name[i + 1..].to_string(),
            _ => return None,
        }
    }
}

/// Make `fqdn` (in `zone`) answer with this server only: one A record with
/// `ipv4`, no CNAME, and no AAAA except this server's own `ipv6s`. For the
/// panel's hostname, which the operator explicitly wants here. The old path
/// only added an A, so an existing A to another server stayed next to it
/// (two answers, part of the visits going elsewhere) and a CNAME or an old
/// AAAA kept pointing away. Returns what changed, for the task log.
pub(crate) async fn point_name_here(
    state: &AppState,
    zone: &str,
    fqdn: &str,
    ipv4: &str,
    ipv6s: &[String],
) -> AgentResult<Vec<String>> {
    let zone = zone.trim_end_matches('.').to_lowercase();
    let fqdn = fqdn.trim_end_matches('.').to_lowercase();
    let mut conn = pdns_conn(state).await?;
    let zone_id: u32 = conn
        .exec_first("SELECT id FROM domains WHERE name = ?", (&zone,))
        .await?
        .ok_or(AgentError::NotFound)?;
    let rows: Vec<(u32, String, String)> = conn
        .exec(
            "SELECT id, type, content FROM records \
             WHERE domain_id = ? AND name = ? AND type IN ('A', 'AAAA', 'CNAME')",
            (zone_id, &fqdn),
        )
        .await?;
    let mut log = Vec::new();
    let mut has_a = false;
    for (id, rtype, content) in rows {
        let keep = match rtype.as_str() {
            "A" => content == ipv4,
            "AAAA" => ipv6s.iter().any(|v| v == &content),
            _ => false,
        };
        if keep {
            has_a |= rtype == "A";
            continue;
        }
        conn.exec_drop("DELETE FROM records WHERE id = ?", (id,)).await?;
        log.push(format!("removed {rtype} {fqdn} -> {content}: the panel's name must point here"));
    }
    if has_a {
        log.push(format!("A {fqdn} -> {ipv4} already present"));
    } else {
        insert_record(&mut conn, zone_id, &fqdn, "A", ipv4, 0).await?;
        log.push(format!("added A {fqdn} -> {ipv4}"));
    }
    Ok(log)
}

/// Upsert a record by (name, type). Smaller variant of `upsert_single` for
/// callers that don't want to track changes in a ReconcileResp.
async fn upsert_record_basic(
    conn: &mut mysql_async::Conn,
    domain_id: u32,
    name: &str,
    rtype: &str,
    content: &str,
) -> AgentResult<()> {
    if rtype != "CNAME" && cname_at(conn, domain_id, name).await? {
        tracing::warn!(name, rtype, "not adding a record next to a CNAME");
        return Ok(());
    }
    let existing: Option<u32> = conn
        .exec_first(
            "SELECT id FROM records WHERE domain_id = ? AND name = ? AND type = ? LIMIT 1",
            (domain_id, name, rtype),
        )
        .await?;
    match existing {
        Some(id) => {
            conn.exec_drop(
                "UPDATE records SET content = ?, ttl = 3600, disabled = 0 WHERE id = ?",
                (content, id),
            )
            .await?;
        }
        None => {
            insert_record(conn, domain_id, name, rtype, content, 0).await?;
        }
    }
    Ok(())
}

/// If `domain`'s enclosing zone lives in the panel's PowerDNS DB, upsert
/// A/AAAA records pointing at this server. Returns `Ok(true)` when records
/// were written, `Ok(false)` when the zone is external (caller-managed DNS).
/// What `ensure_subdomain_records` did for a name.
pub(crate) enum SubdomainRecords {
    Written,
    /// No panel-managed zone encloses the name.
    ExternalZone,
    /// The name is a managed zone's apex (create_zone / reconcile own it).
    ZoneApex,
    NoServerIp,
    /// An A record already sends the name to other address(es).
    PointsElsewhere(String),
    IsCname,
}

pub(crate) async fn ensure_subdomain_records(
    state: &AppState,
    domain: &str,
) -> AgentResult<SubdomainRecords> {
    let domain_norm = domain.trim_end_matches('.').to_lowercase();
    let Some((zone_name, zone_id)) = find_enclosing_zone(state, &domain_norm).await else {
        return Ok(SubdomainRecords::ExternalZone);
    };
    if domain_norm == zone_name {
        return Ok(SubdomainRecords::ZoneApex);
    }
    let v4 = crate::server_ip::resolve_ipv4(None, &state.config.panel_host).await;
    let v6 = crate::server_ip::resolve_ipv6(None, &state.config.panel_host).await;
    if v4.is_none() && v6.is_none() {
        return Ok(SubdomainRecords::NoServerIp);
    }
    let mut conn = pdns_conn(state).await?;
    if cname_at(&mut conn, zone_id, &domain_norm).await? {
        return Ok(SubdomainRecords::IsCname);
    }
    // A name the operator pointed at another host stays that way: issuing a
    // cert here used to rewrite its A record to this server.
    let existing_a: Vec<String> = conn
        .exec(
            "SELECT content FROM records WHERE domain_id = ? AND name = ? AND type = 'A' AND disabled = 0",
            (zone_id, &domain_norm),
        )
        .await?;
    if !existing_a.is_empty() && !existing_a.iter().any(|ip| Some(ip) == v4.as_ref()) {
        return Ok(SubdomainRecords::PointsElsewhere(existing_a.join(", ")));
    }
    if let Some(ip) = &v4 {
        upsert_record_basic(&mut conn, zone_id, &domain_norm, "A", ip).await?;
    }
    if let Some(ip6) = &v6 {
        upsert_record_basic(&mut conn, zone_id, &domain_norm, "AAAA", ip6).await?;
    }
    Ok(SubdomainRecords::Written)
}

/// Run `dig +short @1.1.1.1 <domain> <rtype>` and return the parsed answers.
/// Skips the local resolver to avoid stale NXDOMAIN cache after the user
/// just added a record at their DNS provider.
///
/// When the name is a CNAME (e.g. `www -> apex`), `dig +short A` returns the
/// CNAME target first, then the final A. Filter to the requested IP family so
/// the error log shows clean IPs only — otherwise the operator sees nonsense
/// like `A=["example.com", "203.0.113.10"]` and can't tell why preflight
/// failed.
async fn dig_public(domain: &str, rtype: &str) -> Vec<String> {
    // Try 1.1.1.1 first; if it returns nothing, fall back to 8.8.8.8 and 9.9.9.9
    // in turn before giving up. Each attempt gets +tries=2 +time=4 so a single
    // dropped UDP packet doesn't sink the whole preflight (the old +tries=1
    // +time=3 caused false negatives that flipped the row to "failed" and made
    // operators chase ghost DNS problems). Also helps when one resolver still
    // has a stale NXDOMAIN cached from before the zone existed.
    for resolver in ["@1.1.1.1", "@8.8.8.8", "@9.9.9.9"] {
        let res = crate::exec::run(
            "dig",
            ["+short", "+time=4", "+tries=2", resolver, domain, rtype],
        )
        .await;
        let Ok(out) = res else { continue };
        let want_v4 = rtype.eq_ignore_ascii_case("A");
        let parsed: Vec<String> = out
            .stdout
            .lines()
            .filter_map(|l| {
                let s = l.trim().trim_end_matches('.');
                if s.is_empty() {
                    return None;
                }
                match s.parse::<std::net::IpAddr>() {
                    Ok(std::net::IpAddr::V4(_)) if want_v4 => Some(s.to_string()),
                    Ok(std::net::IpAddr::V6(_)) if !want_v4 => Some(s.to_string()),
                    _ => None,
                }
            })
            .collect();
        if !parsed.is_empty() {
            return parsed;
        }
    }
    Vec::new()
}

/// Verify public DNS for `domain` resolves to one of this server's IPs.
/// Returns a human-readable error suitable for surfacing in the task log
/// if the lookup fails or the records point elsewhere.
pub(crate) async fn verify_public_dns(
    state: &AppState,
    domain: &str,
) -> Result<(), String> {
    let expected_v4 = crate::server_ip::resolve_ipv4(None, &state.config.panel_host).await;
    let expected_v6 = crate::server_ip::resolve_ipv6(None, &state.config.panel_host).await;
    let v4_resolved = dig_public(domain, "A").await;
    let v6_resolved = dig_public(domain, "AAAA").await;

    if v4_resolved.is_empty() && v6_resolved.is_empty() {
        return Err(format!(
            "no public A or AAAA record for {domain} (queried 1.1.1.1). \
             Add an A record at the authoritative DNS provider for the parent \
             zone, pointing to {}, then retry SSL.",
            expected_v4
                .as_deref()
                .or(expected_v6.as_deref())
                .unwrap_or(&state.config.panel_host)
        ));
    }

    let v4_match = expected_v4
        .as_ref()
        .map_or(false, |w| v4_resolved.iter().any(|got| got == w));
    let v6_match = expected_v6
        .as_ref()
        .map_or(false, |w| v6_resolved.iter().any(|got| got == w));

    if !v4_match && !v6_match {
        return Err(format!(
            "public DNS for {domain} resolves to A={v4_resolved:?} AAAA={v6_resolved:?} \
             but this server is A={expected_v4:?} AAAA={expected_v6:?}. \
             Update the record at your DNS provider before retrying."
        ));
    }
    Ok(())
}

/// Symmetric to `ensure_subdomain_records`. When a site is deleted and the
/// parent zone is panel-managed, drop the auto-created A/AAAA so the public
/// record disappears too — otherwise nslookup keeps returning a stale IP for
/// a hostname that no longer routes anywhere on this VPS.
///
/// Never deletes the apex (some other site/feature may still need it) and
/// never touches records when the parent zone is external. Returns the
/// number of rows removed (0 when nothing matched, which is a no-op success).
///
/// MX-target guard: if any MX record in the same zone has this hostname as
/// its content (e.g. zone `foo.com` has `MX 10 mail.foo.com` and the caller
/// is trying to clean up `mail.foo.com`), the deletion is skipped entirely.
/// Without this, deleting a site that happens to share the name of an active
/// mail tenant's MX target (the `mail.foo.com → port 8082` Roundcube experiment
/// pattern) would silently wipe the A record the MX relies on, breaking mail
/// delivery for an unrelated feature. PowerDNS stores MX content as the bare
/// target hostname (priority lives in the `prio` column), so direct equality
/// is the right check.
pub(crate) async fn cleanup_subdomain_records(
    state: &AppState,
    domain: &str,
) -> AgentResult<u64> {
    let domain_norm = domain.trim_end_matches('.').to_lowercase();
    let Some((zone_name, zone_id)) = find_enclosing_zone(state, &domain_norm).await else {
        return Ok(0);
    };
    if domain_norm == zone_name {
        return Ok(0);
    }
    let mut conn = pdns_conn(state).await?;
    let mx_target: Option<u32> = conn
        .exec_first(
            "SELECT id FROM records \
             WHERE domain_id = ? AND type = 'MX' AND content = ? LIMIT 1",
            (zone_id, &domain_norm),
        )
        .await?;
    if mx_target.is_some() {
        tracing::info!(
            domain = %domain_norm,
            zone = %zone_name,
            "skipping A/AAAA cleanup: name is still an active MX target in this zone"
        );
        return Ok(0);
    }
    conn.exec_drop(
        "DELETE FROM records \
         WHERE domain_id = ? AND name = ? AND type IN ('A', 'AAAA')",
        (zone_id, &domain_norm),
    )
    .await?;
    Ok(conn.affected_rows())
}

/// One-call orchestrator for SSL issuance paths: auto-create records when we
/// own DNS, then verify public DNS regardless. Logs progress to the task.
pub(crate) async fn preflight_acme_dns(
    state: &AppState,
    domain: &str,
    task: &crate::ipc::tasks::TaskHandle,
) -> Result<(), String> {
    match ensure_subdomain_records(state, domain).await {
        Ok(SubdomainRecords::Written) => task.log(format!(
            "preflight: A/AAAA for {domain} point at this server in the panel-managed zone"
        )),
        Ok(SubdomainRecords::ExternalZone) => task.log(format!(
            "preflight: {domain} is not in a panel-managed zone; its DNS is managed elsewhere"
        )),
        Ok(SubdomainRecords::ZoneApex) => task.log(format!(
            "preflight: {domain} is the apex of a panel-managed zone; its records come from the zone itself"
        )),
        Ok(SubdomainRecords::NoServerIp) => task.log(format!(
            "preflight: could not work out this server's IP, so no record was written for {domain}"
        )),
        Ok(SubdomainRecords::PointsElsewhere(ips)) => task.log(format!(
            "preflight: {domain} already points at {ips}, not at this server; left unchanged. \
             Point it here on the zone page if this server should serve it"
        )),
        Ok(SubdomainRecords::IsCname) => task.log(format!(
            "preflight: {domain} is a CNAME; no A/AAAA record was added next to it"
        )),
        Err(e) => task.log(format!(
            "preflight: warning, could not write {domain} record to PowerDNS: {e}"
        )),
    }
    verify_public_dns(state, domain).await
}
