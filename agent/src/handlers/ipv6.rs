//! IPv6: the server's address and the default for new domains
//! (`GET /ipv6/status`, `PUT /ipv6/settings`; panel.db `ipv6.address` and
//! `ipv6.new_domains`), and the switch of each domain
//! (`GET|PUT /dns/zones/{name}/ipv6`).
//!
//! server_ip.rs has the rule for the address in effect
//! (`Ipv6Facts::in_effect`), dns.rs what on and off mean for a zone's records
//! and where the choice is stored. The checks here only report: a warning or
//! a failure never stops the operator from switching IPv6 on.

use crate::error::{AgentError, AgentResult};
use crate::handlers::dns::{self, Ipv6Record, OwnAddrs};
use crate::handlers::mail_check::{self, Dig, Lookup, MailCheckItem, Resolver, Status};
use crate::server_ip::{self, Ipv6Facts, Ipv6Source};
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    Json,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

/// One change to the IPv6 settings or a domain's switch at a time.
static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const ADDRESS_KEY: &str = "ipv6.address";
const NEW_DOMAINS_KEY: &str = "ipv6.new_domains";
/// Set once the step at start (`migrate_at_startup`) has run.
const MIGRATED_KEY: &str = "ipv6.migrated";
/// All checks of one domain together.
const CHECKS_TIMEOUT: Duration = Duration::from_secs(20);
/// nginx -T, and one query to PowerDNS.
const TOOL_TIMEOUT: Duration = Duration::from_secs(8);
const PDNS_CONF: &str = "/etc/powerdns/pdns.conf";

#[derive(Debug, Serialize, PartialEq)]
pub struct Ipv6Candidate {
    pub address: String,
    pub device: String,
    pub note: String,
}

#[derive(Debug, Serialize)]
pub struct Ipv6Status {
    /// The server's IPv6 address in effect; None when there is none.
    pub address: Option<String>,
    pub source: Ipv6Source,
    /// ipv6.address as stored ("" = automatic).
    pub setting: String,
    pub candidates: Vec<Ipv6Candidate>,
    pub new_domains: &'static str,
    pub domains_on: usize,
    pub domains_total: usize,
    pub notes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct DomainIpv6 {
    pub domain: String,
    pub enabled: bool,
    pub address: Option<String>,
    /// False only when there is no server address at all.
    pub can_enable: bool,
    pub records: Vec<Ipv6Record>,
    pub checks: Vec<MailCheckItem>,
    pub notes: Vec<String>,
}

#[derive(Deserialize)]
pub struct PutSettings {
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub new_domains: Option<String>,
}

#[derive(Deserialize)]
pub struct PutZone {
    pub enabled: bool,
}

fn item(id: &str, label: &str, status: Status, detail: impl Into<String>) -> MailCheckItem {
    MailCheckItem {
        id: id.to_string(),
        label: label.to_string(),
        status,
        detail: detail.into(),
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

fn read_setting(panel_db: &crate::db::PanelDb, key: &str) -> Option<String> {
    panel_db
        .with(|conn| {
            conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                row.get::<_, String>(0)
            })
        })
        .ok()
}

/// Write settings in one transaction: Some(value) sets it, None deletes it.
/// The table is the panel's; it is created here the same way when the
/// panel has not done so yet.
fn write_settings(panel_db: &crate::db::PanelDb, values: &[(&str, Option<&str>)]) -> AgentResult<()> {
    panel_db.with(|conn| {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )?;
        let tx = conn.unchecked_transaction()?;
        for (key, value) in values {
            match value {
                Some(v) => tx.execute(
                    "INSERT INTO settings (key, value) VALUES (?1, ?2) \
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    rusqlite::params![key, v],
                )?,
                None => tx.execute("DELETE FROM settings WHERE key = ?1", [key])?,
            };
        }
        tx.commit()
    })
}

/// The address a PUT asks for: "" for automatic, else one of this
/// server's addresses, normalised.
fn wanted_address(raw: &str, on_interfaces: &[String]) -> AgentResult<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(String::new());
    }
    let Some(addr) = server_ip::parse_ipv6(raw) else {
        return Err(AgentError::Invalid(format!("{raw} is not an IPv6 address.")));
    };
    if on_interfaces.contains(&addr) {
        return Ok(addr);
    }
    Err(AgentError::Invalid(match on_interfaces {
        [] => format!("{addr} is not an address of this server, and this server has no global IPv6 address."),
        list => format!(
            "{addr} is not an address of this server. Choose one of: {}.",
            list.join(", ")
        ),
    }))
}

fn wanted_new_domains(raw: Option<&str>) -> AgentResult<Option<bool>> {
    match raw.map(str::trim) {
        None => Ok(None),
        Some("on") => Ok(Some(true)),
        Some("off") => Ok(Some(false)),
        Some(_) => Err(AgentError::Invalid("new_domains must be on or off.".into())),
    }
}

// ---------------------------------------------------------------------------
// GET /ipv6/status
// ---------------------------------------------------------------------------

/// The note next to an address the operator can choose.
fn candidate_note(addr: &str, facts: &Ipv6Facts) -> String {
    let mut notes: Vec<&str> = Vec::new();
    if facts.published.as_deref() == Some(addr) {
        notes.push("the panel hostname's AAAA record points here");
    }
    if server_ip::is_ula(addr) {
        notes.push("a private address (fc00::/7), not reachable from the internet");
    }
    notes.join("; ")
}

/// What the zones say about IPv6, for the notes.
#[derive(Debug, Default, PartialEq)]
struct ZoneCounts {
    on: Vec<String>,
    total: usize,
    /// Zones that count as off without a stored choice, but have AAAA
    /// records for this server elsewhere than at their apex.
    partial: Vec<String>,
}

fn zone_counts(zones: &BTreeMap<String, dns::ZoneIpv6>) -> ZoneCounts {
    ZoneCounts {
        on: zones.iter().filter(|(_, z)| z.enabled).map(|(n, _)| n.clone()).collect(),
        total: zones.len(),
        partial: zones
            .iter()
            .filter(|(_, z)| !z.enabled && z.stored.is_none() && z.own_aaaa)
            .map(|(n, _)| n.clone())
            .collect(),
    }
}

/// The plain sentences under the address. Only what needs saying: why
/// there is no address, what the operator should know about mail, zones
/// that need a decision.
fn status_notes(facts: &Ipv6Facts, zones: &ZoneCounts) -> Vec<String> {
    let mut notes = Vec::new();
    let (address, source) = facts.in_effect();
    let host = facts.host.as_str();
    let public: Vec<&String> = facts.on_interfaces.iter().filter(|a| !server_ip::is_ula(a)).collect();
    if let Some(chosen) = facts.chosen().filter(|_| address.is_none()) {
        notes.push(format!(
            "The address chosen, {chosen}, is not on this server any more, so no IPv6 address is in effect. Choose another one, or Automatic."
        ));
    }
    let foreign = facts.published.as_deref().filter(|p| !facts.on_interfaces.iter().any(|a| a == p));
    if let (Some(p), true) = (foreign, facts.chosen().is_none()) {
        notes.push(format!(
            "The panel hostname {host} has an AAAA record for {p}, which is not an address of this server, so it is not used."
        ));
    }
    if address.is_none() && facts.chosen().is_none() {
        notes.push(match public.len() {
            0 => "This server has no public IPv6 address. Once your hosting provider gives it one and it is configured on the server, it shows up here.".to_string(),
            _ => "This server has more than one IPv6 address. Choose the one to publish in DNS.".to_string(),
        });
    }
    if let (Some(a), Ipv6Source::Interface) = (&address, source) {
        notes.push(format!(
            "{a} is the only public IPv6 address of this server, so it is used. Outgoing mail does not use it until it is chosen here \
             (or the panel hostname has an AAAA record for it), and then only once its check passes (Settings > Mail)."
        ));
    }
    if address.is_none() && !zones.on.is_empty() {
        notes.push(format!(
            "IPv6 is on for {}, but with no IPv6 address in effect their records are not changed.",
            plural(zones.on.len(), "domain", "domains")
        ));
    }
    if !zones.partial.is_empty() {
        let (it, has) = if zones.partial.len() == 1 { ("it", "has") } else { ("them", "have") };
        notes.push(format!(
            "{} {has} AAAA records for this server but none at the apex, so IPv6 counts as off there. Those records stay as they are \
             until you decide: switch IPv6 on for {it} on the domain's page to complete them, or off to remove them.",
            mail_check::join_names(&zones.partial)
        ));
    }
    notes
}

async fn build_status(state: &AppState) -> Ipv6Status {
    let (facts, devices) = tokio::join!(
        server_ip::ipv6_facts(&state.panel_db, &state.config.panel_host),
        server_ip::ipv6_devices(),
    );
    let (address, source) = facts.in_effect();
    let candidates = devices
        .iter()
        .map(|(device, addr)| Ipv6Candidate {
            address: addr.clone(),
            device: device.clone(),
            note: candidate_note(addr, &facts),
        })
        .collect();
    let (counts, zones_error) = match dns::zone_ipv6_states(state, &facts.own()).await {
        Ok(zones) => (zone_counts(&zones), None),
        Err(e) => (ZoneCounts::default(), Some(e)),
    };
    let mut notes = status_notes(&facts, &counts);
    if let Some(e) = zones_error {
        notes.push(format!("The DNS zones could not be read: {e}."));
    }
    Ipv6Status {
        address,
        source,
        setting: facts.chosen().unwrap_or_default(),
        candidates,
        new_domains: if server_ip::ipv6_new_domains(&state.panel_db) { "on" } else { "off" },
        domains_on: counts.on.len(),
        domains_total: counts.total,
        notes,
    }
}

/// GET /ipv6/status
pub async fn status(State(state): State<AppState>) -> AgentResult<Json<Ipv6Status>> {
    Ok(Json(build_status(&state).await))
}

// ---------------------------------------------------------------------------
// PUT /ipv6/settings
// ---------------------------------------------------------------------------

/// The 409 for an address change that leaves no address while zones have
/// IPv6 on: their records would point at nothing that is this server.
fn refuse_clearing(on: usize) -> AgentError {
    AgentError::Conflict(format!(
        "IPv6 is on for {}, and with this choice there would be no IPv6 address to publish for {}. \
         Choose an address, or switch IPv6 off for {} first (Domains > the domain > IPv6).",
        plural(on, "domain", "domains"),
        if on == 1 { "it" } else { "them" },
        if on == 1 { "it" } else { "them" },
    ))
}

/// PUT /ipv6/settings  { address?: string, new_domains?: "on" | "off" }
///
/// Changing the address moves the records of every zone with IPv6 on to the
/// new address, in one transaction with the setting, and tells outgoing
/// mail (`mail_ip::ensure_at_startup` looks at its address again).
pub async fn put_settings(
    State(state): State<AppState>,
    Json(body): Json<PutSettings>,
) -> AgentResult<Json<Ipv6Status>> {
    let new_domains = wanted_new_domains(body.new_domains.as_deref())?;
    let _guard = LOCK.lock().await;
    let before = server_ip::ipv6_facts(&state.panel_db, &state.config.panel_host).await;
    let address = match body.address.as_deref() {
        Some(raw) => Some(wanted_address(raw, &before.on_interfaces)?),
        None => None,
    };
    let new_domains_value = new_domains.map(|on| if on { "on" } else { "off" });
    let mut values: Vec<(&str, Option<&str>)> = Vec::new();
    if let Some(a) = address.as_deref() {
        values.push((ADDRESS_KEY, Some(a)));
    }
    if let Some(v) = new_domains_value {
        values.push((NEW_DOMAINS_KEY, Some(v)));
    }
    let after = Ipv6Facts {
        setting: address.clone().unwrap_or_else(|| before.setting.clone()),
        ..before.clone()
    };
    let (old, new) = (before.address(), after.address());

    let mut moved = false;
    if old != new {
        let zones = dns::zone_ipv6_states(&state, &before.own()).await?;
        let on: Vec<String> = zones.iter().filter(|(_, z)| z.enabled).map(|(n, _)| n.clone()).collect();
        if new.is_none() && !on.is_empty() {
            return Err(refuse_clearing(on.len()));
        }
        if !on.is_empty() {
            // The old address stays this server's for the move: its records go.
            let facts4 = server_ip::ipv4_facts(&state.config.panel_host).await;
            let mut own: OwnAddrs = dns::own_addrs_from(&after, &facts4);
            for a in before.own() {
                if !own.own_v6.contains(&a) {
                    own.own_v6.push(a);
                }
            }
            let previous: Vec<(&str, Option<String>)> = values
                .iter()
                .map(|(key, _)| (*key, read_setting(&state.panel_db, key)))
                .collect();
            let panel_db = state.panel_db.clone();
            let panel_db_back = state.panel_db.clone();
            let changed = dns::move_zones_ipv6(
                &state,
                &on,
                &own,
                || write_settings(&panel_db, &values),
                move || {
                    let back: Vec<(&str, Option<&str>)> =
                        previous.iter().map(|(k, v)| (*k, v.as_deref())).collect();
                    if let Err(e) = write_settings(&panel_db_back, &back) {
                        tracing::warn!(error = %e, "could not put the IPv6 settings back");
                    }
                },
            )
            .await?;
            tracing::info!(zones = on.len(), changed, ?old, ?new, "IPv6 records moved to the new address");
            moved = true;
        }
    }
    if !moved && !values.is_empty() {
        write_settings(&state.panel_db, &values)?;
    }
    drop(_guard);
    if old != new {
        tokio::spawn(crate::handlers::mail_ip::ensure_at_startup(state.clone()));
    }
    Ok(Json(build_status(&state).await))
}

// ---------------------------------------------------------------------------
// GET /dns/zones/{name}/ipv6
// ---------------------------------------------------------------------------

/// One `server` block of nginx's configuration: its names, and whether one
/// of its listen lines is an IPv6 one.
#[derive(Debug, PartialEq)]
struct NginxServer {
    names: Vec<String>,
    ipv6: bool,
}

/// The server blocks in `nginx -T` output. Each file is printed after a
/// `# configuration file <path>:` line with its braces balanced, so the
/// count starts again there. Quoted strings and comments are left out
/// before braces are counted.
fn nginx_servers(dump: &str) -> Vec<NginxServer> {
    fn strip(line: &str) -> String {
        let mut out = String::new();
        let mut quote: Option<char> = None;
        for c in line.chars() {
            match quote {
                Some(q) if c == q => quote = None,
                Some(_) => {}
                None if c == '"' || c == '\'' => quote = Some(c),
                None if c == '#' => break,
                None => out.push(c),
            }
        }
        out.trim().to_string()
    }
    let mut out = Vec::new();
    let mut depth: i64 = 0;
    let mut current: Option<(i64, NginxServer)> = None;
    for raw in dump.lines() {
        if raw.starts_with("# configuration file ") {
            depth = 0;
            if let Some((_, s)) = current.take() {
                out.push(s);
            }
            continue;
        }
        let line = strip(raw);
        if current.is_none() {
            let rest = line.strip_prefix("server").map(str::trim_start);
            if rest.is_some_and(|r| r.starts_with('{')) {
                current = Some((depth + 1, NginxServer { names: Vec::new(), ipv6: false }));
            }
        }
        if let Some((inner, server)) = current.as_mut() {
            if depth == *inner {
                let mut words = line.trim_end_matches(';').split_whitespace();
                match words.next() {
                    Some("server_name") => server.names.extend(words.map(|w| w.to_ascii_lowercase())),
                    Some("listen") if words.next().is_some_and(|a| a.starts_with('[')) => server.ipv6 = true,
                    _ => {}
                }
            }
        }
        depth += line.matches('{').count() as i64 - line.matches('}').count() as i64;
        if let Some((inner, _)) = &current {
            if depth < *inner {
                if let Some((_, s)) = current.take() {
                    out.push(s);
                }
            }
        }
        depth = depth.max(0);
    }
    if let Some((_, s)) = current {
        out.push(s);
    }
    out
}

/// Does a server_name entry cover this name? Exact, `*.zone` and `.zone`;
/// regular expressions are not followed.
fn server_name_covers(entry: &str, name: &str) -> bool {
    if let Some(suffix) = entry.strip_prefix('*') {
        return suffix.starts_with('.') && name.ends_with(suffix);
    }
    if let Some(base) = entry.strip_prefix('.') {
        return name == base || name.ends_with(entry);
    }
    entry == name
}

/// The web server check: every name of the zone that nginx serves must have
/// an IPv6 listen line in each of its server blocks, or over IPv6 it
/// reaches whichever site nginx answers with by default.
fn web_item(names: &[String], servers: &[NginxServer]) -> MailCheckItem {
    const ID: &str = "web";
    const LABEL: &str = "Web server listens on IPv6";
    let mut served: Vec<&str> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    for name in names {
        let blocks: Vec<&NginxServer> = servers
            .iter()
            .filter(|s| s.names.iter().any(|e| server_name_covers(e, name)))
            .collect();
        if blocks.is_empty() {
            continue;
        }
        served.push(name);
        if !blocks.iter().all(|s| s.ipv6) {
            missing.push(name.clone());
        }
    }
    if served.is_empty() {
        return item(ID, LABEL, Status::Ok, "None of these names is a site in nginx on this server: nothing to check.");
    }
    if missing.is_empty() {
        return item(
            ID,
            LABEL,
            Status::Ok,
            format!("nginx listens on IPv6 for {}.", mail_check::join_names(&served.iter().map(|s| s.to_string()).collect::<Vec<_>>())),
        );
    }
    item(
        ID,
        LABEL,
        Status::Warn,
        format!(
            "nginx does not listen on IPv6 for {}: a server block of {} has no listen [::]:80 (or [::]:443) line. \
             Over IPv6 visitors reach another site of this server instead. Save the site again under Domains to rewrite its configuration.",
            mail_check::join_names(&missing),
            if missing.len() == 1 { "it" } else { "each" },
        ),
    )
}

async fn web_check(names: &[String]) -> MailCheckItem {
    const ID: &str = "web";
    const LABEL: &str = "Web server listens on IPv6";
    match tokio::time::timeout(TOOL_TIMEOUT, crate::exec::run("nginx", ["-T"])).await {
        Ok(Ok(out)) => web_item(names, &nginx_servers(&out.stdout)),
        Ok(Err(AgentError::Io(e))) if e.kind() == std::io::ErrorKind::NotFound => {
            item(ID, LABEL, Status::Warn, "Not checked: nginx is not installed on this server.")
        }
        Ok(Err(e)) => item(ID, LABEL, Status::Warn, format!("Not checked: nginx's configuration could not be read ({e}).")),
        Err(_) => item(ID, LABEL, Status::Warn, "Not checked: nginx took too long to print its configuration."),
    }
}

/// The addresses in pdns.conf's `local-address` line, None without one.
fn pdns_local_addresses(conf: &str) -> Option<Vec<String>> {
    let line = conf
        .lines()
        .filter_map(|l| l.trim().strip_prefix("local-address"))
        .filter_map(|rest| rest.trim_start().strip_prefix('='))
        .last()?;
    Some(
        line.split(|c: char| c == ',' || c.is_whitespace())
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

/// Does PowerDNS listen on IPv6 at `addr`? `::` is every IPv6 address.
fn pdns_listens_ipv6(local: &[String], addr: &str) -> bool {
    local.iter().any(|a| {
        let bare = a.trim_start_matches('[').split(']').next().unwrap_or(a);
        bare == "::" || server_ip::parse_ipv6(bare).as_deref() == Some(addr)
    })
}

fn nameserver_item(
    zone: &str,
    in_zone_ns: &[String],
    addr: &str,
    conf: Result<&str, &str>,
    answered: Option<bool>,
) -> MailCheckItem {
    const ID: &str = "nameservers";
    const LABEL: &str = "Name servers answer over IPv6";
    if in_zone_ns.is_empty() {
        return item(
            ID,
            LABEL,
            Status::Ok,
            format!("The name servers of {zone} are not names in this zone, so IPv6 on gives them no AAAA record: nothing to check."),
        );
    }
    let ns = mail_check::join_names(in_zone_ns);
    let conf = match conf {
        Ok(conf) => conf,
        Err(why) => {
            return item(ID, LABEL, Status::Warn, format!("Not checked: {PDNS_CONF} could not be read ({why})."));
        }
    };
    let Some(local) = pdns_local_addresses(conf) else {
        // No local-address: PowerDNS listens on every address.
        return item(ID, LABEL, Status::Ok, format!("PowerDNS listens on every address, so {ns} answer over IPv6 too."));
    };
    if !pdns_listens_ipv6(&local, addr) {
        return item(
            ID,
            LABEL,
            Status::Warn,
            format!(
                "PowerDNS listens on IPv4 only (local-address={} in {PDNS_CONF}), but IPv6 on gives {ns} an AAAA record: resolvers that ask over IPv6 get no answer there. \
                 The panel's update adds IPv6 to that line; or add \",::\" to it yourself and restart pdns.",
                local.join(",")
            ),
        );
    }
    match answered {
        Some(false) => item(
            ID,
            LABEL,
            Status::Warn,
            format!("PowerDNS is set to listen on IPv6, but did not answer for {zone} at {addr} just now. Check that pdns is running: systemctl status pdns."),
        ),
        _ => item(ID, LABEL, Status::Ok, format!("PowerDNS answers on IPv6 ({addr}), so {ns} can be asked over IPv6.")),
    }
}

/// Ask PowerDNS for the zone's SOA at the address. None when that cannot be
/// tried (no dig).
async fn pdns_answers(zone: &str, addr: &str) -> Option<bool> {
    let at = format!("@{addr}");
    let run = crate::exec::run("dig", ["+short", "+time=2", "+tries=1", "+norecurse", at.as_str(), zone, "SOA"]);
    match tokio::time::timeout(TOOL_TIMEOUT, run).await {
        Ok(Ok(out)) => Some(!out.stdout.trim().is_empty() && !out.stdout.contains(";;")),
        Ok(Err(AgentError::Io(e))) if e.kind() == std::io::ErrorKind::NotFound => None,
        _ => Some(false),
    }
}

fn public_item(zone: &str, addr: &str, enabled: bool, lookup: &Lookup) -> MailCheckItem {
    const ID: &str = "public";
    const LABEL: &str = "Public DNS";
    match (lookup, enabled) {
        (Lookup::Records(addrs), true) if addrs.iter().any(|a| a == addr) => {
            item(ID, LABEL, Status::Ok, format!("{zone} resolves to {addr} over IPv6 in public DNS."))
        }
        (Lookup::Records(addrs), true) => item(
            ID,
            LABEL,
            Status::Warn,
            format!(
                "Public DNS still gives {zone} the AAAA record {}, not {addr}. Resolvers follow within about an hour; if it stays, {zone} is not served by this server's name servers.",
                mail_check::join_names(addrs)
            ),
        ),
        (Lookup::Empty, true) => item(
            ID,
            LABEL,
            Status::Warn,
            format!("Public DNS has no AAAA record for {zone} yet. After switching IPv6 on that can take up to an hour."),
        ),
        (Lookup::Records(addrs), false) if addrs.iter().any(|a| a == addr) => item(
            ID,
            LABEL,
            Status::Warn,
            format!("Public DNS still gives {zone} the AAAA record {addr}. It goes once resolvers forget it, within about an hour."),
        ),
        (Lookup::Records(addrs), false) => item(
            ID,
            LABEL,
            Status::Ok,
            format!("{zone} has the AAAA record {} in public DNS, which is not this server's.", mail_check::join_names(addrs)),
        ),
        (Lookup::Empty, false) => item(ID, LABEL, Status::Ok, format!("{zone} has no AAAA record in public DNS.")),
        (Lookup::Failed(why), _) => item(ID, LABEL, Status::Warn, format!("Public DNS could not be asked: {why}.")),
    }
}

fn address_item(facts: &Ipv6Facts, devices: &[(String, String)]) -> MailCheckItem {
    const ID: &str = "address";
    const LABEL: &str = "Address on this server";
    let (address, source) = facts.in_effect();
    let Some(a) = address else {
        let detail = match facts.chosen() {
            Some(chosen) => format!(
                "The address chosen, {chosen}, is not on this server any more. {}",
                dns::SET_ADDRESS
            ),
            None if facts.on_interfaces.iter().all(|a| server_ip::is_ula(a)) => {
                "This server has no public IPv6 address.".to_string()
            }
            None => format!("This server has more than one IPv6 address. {}", dns::SET_ADDRESS),
        };
        return item(ID, LABEL, Status::Fail, detail);
    };
    let on = devices
        .iter()
        .find(|(_, addr)| *addr == a)
        .map(|(dev, _)| format!(" ({dev})"))
        .unwrap_or_default();
    let why = match source {
        Ipv6Source::Setting => "the address chosen under Settings > DNS & SSL > IPv6".to_string(),
        Ipv6Source::Hostname => format!("the panel hostname {} has an AAAA record for it", facts.host),
        _ => "the only public IPv6 address of this server".to_string(),
    };
    item(ID, LABEL, Status::Ok, format!("{a} is on this server{on}: {why}."))
}

/// Will IPv6 work for this domain? Bounded in time; a missing tool is a
/// warning, never an error.
async fn zone_checks(
    state: &AppState,
    view: &dns::ZoneIpv6View,
    facts: &Ipv6Facts,
    devices: &[(String, String)],
) -> Vec<MailCheckItem> {
    let mut checks = vec![address_item(facts, devices)];
    let Some(addr) = facts.address() else {
        return checks;
    };
    let names: Vec<String> = view
        .plan
        .records
        .iter()
        .filter(|r| r.rtype == "AAAA")
        .map(|r| r.name.clone())
        .collect();
    let zone = view.zone.as_str();
    let dig = Dig::default();
    let run = async {
        let web = web_check(&names);
        let ns = async {
            if view.in_zone_ns.is_empty() {
                return nameserver_item(zone, &view.in_zone_ns, &addr, Ok(""), None);
            }
            let conf = tokio::fs::read_to_string(PDNS_CONF).await.map_err(|e| e.to_string());
            let answered = match &conf {
                Ok(c) if pdns_local_addresses(c).is_none_or(|l| pdns_listens_ipv6(&l, &addr)) => {
                    pdns_answers(zone, &addr).await
                }
                _ => None,
            };
            nameserver_item(zone, &view.in_zone_ns, &addr, conf.as_deref().map_err(String::as_str), answered)
        };
        let reverse = async {
            if !view.has_mail {
                return Vec::new();
            }
            let helo = crate::handlers::mail_ip::mail_addrs(state).await.helo;
            mail_check::reverse_dns_items(&dig, &addr, &helo).await
        };
        let public = async { public_item(zone, &addr, view.enabled, &dig.lookup(zone, "AAAA").await) };
        tokio::join!(web, ns, reverse, public)
    };
    match tokio::time::timeout(CHECKS_TIMEOUT, run).await {
        Ok((web, ns, reverse, public)) => {
            checks.push(web);
            checks.push(ns);
            checks.extend(reverse);
            checks.push(public);
        }
        Err(_) => checks.push(item(
            "timeout",
            "Checks",
            Status::Warn,
            "The checks took too long. Try again in a moment.",
        )),
    }
    checks
}

/// The sentences under a domain's switch.
fn zone_notes(view: &dns::ZoneIpv6View, facts: &Ipv6Facts, panel_host: &str) -> Vec<String> {
    let mut notes: Vec<String> = view.plan.skipped.clone();
    let zone = view.zone.as_str();
    if facts.address().is_none() {
        notes.push(format!("This server has no IPv6 address to publish. {}", dns::SET_ADDRESS));
    }
    if !view.enabled && view.plan.records.iter().any(|r| r.rtype == "AAAA" && r.present) {
        notes.push(format!(
            "IPv6 counts as off for {zone} because its apex has no AAAA record for this server, but other names have one. \
             They stay as they are until you decide: switch IPv6 on to complete them, or off to remove them."
        ));
    }
    let host = panel_host.trim_end_matches('.').to_ascii_lowercase();
    if host == zone || host.ends_with(&format!(".{zone}")) {
        notes.push(format!(
            "{host}, the panel's hostname, is in this zone. Outgoing mail uses IPv6 only while that name has an AAAA record for this server (Settings > Mail)."
        ));
    }
    if view.has_mail {
        notes.push(if view.enabled {
            format!("{zone} is a mail domain: its SPF record lets this server send its mail over IPv6.")
        } else {
            format!("{zone} is a mail domain: while IPv6 is off, its mail goes out over IPv4 only.")
        });
    }
    notes
}

async fn domain_ipv6(state: &AppState, zone: &str) -> AgentResult<DomainIpv6> {
    let (facts, facts4, devices) = tokio::join!(
        server_ip::ipv6_facts(&state.panel_db, &state.config.panel_host),
        server_ip::ipv4_facts(&state.config.panel_host),
        server_ip::ipv6_devices(),
    );
    let own = dns::own_addrs_from(&facts, &facts4);
    let view = dns::zone_ipv6_view(state, zone, &own).await?;
    let checks = zone_checks(state, &view, &facts, &devices).await;
    let panel_host = server_ip::panel_host_now(&state.config.panel_host).await;
    Ok(DomainIpv6 {
        notes: zone_notes(&view, &facts, &panel_host),
        domain: view.zone,
        enabled: view.enabled,
        address: own.v6.clone(),
        can_enable: own.v6.is_some(),
        records: view.plan.records,
        checks,
    })
}

fn zone_name(raw: &str) -> String {
    raw.trim().trim_end_matches('.').to_lowercase()
}

/// GET /dns/zones/{name}/ipv6
pub async fn get_zone(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> AgentResult<Json<DomainIpv6>> {
    domain_ipv6(&state, &zone_name(&name)).await.map(Json)
}

/// PUT /dns/zones/{name}/ipv6  { enabled: bool }
///
/// The zone's records change and the choice is stored in one transaction
/// (dns::set_zone_ipv6). For a mail domain the per-domain mail routing is
/// built again right away (mail_ip.rs): switched off, the domain is held to
/// IPv4 at once; switched on, it is let go as soon as public DNS shows its
/// new SPF record, looked at now and again after the record's TTL.
pub async fn put_zone(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<PutZone>,
) -> AgentResult<Json<DomainIpv6>> {
    let zone = zone_name(&name);
    let guard = LOCK.lock().await;
    let own = dns::own_addrs(&state).await;
    let view = dns::zone_ipv6_view(&state, &zone, &own).await?;
    if body.enabled && own.v6.is_none() {
        return Err(AgentError::Conflict(format!(
            "This server has no IPv6 address to publish, so IPv6 cannot be switched on for {zone}. {}",
            dns::SET_ADDRESS
        )));
    }
    let plan = dns::set_zone_ipv6(&state, &zone, body.enabled, &own).await?;
    tracing::info!(
        zone = %zone,
        enabled = body.enabled,
        added = plan.add.len(),
        updated = plan.update.len(),
        removed = plan.remove.len(),
        spf = plan.spf.len(),
        "IPv6 switched for the zone"
    );
    drop(guard);
    if view.has_mail {
        let state = state.clone();
        let on = body.enabled;
        tokio::spawn(async move {
            crate::handlers::mail_ip::ensure_at_startup(state.clone()).await;
            if on {
                // The SPF record's TTL is an hour: public resolvers show
                // the new one by then.
                tokio::time::sleep(Duration::from_secs(3700)).await;
                crate::handlers::mail_ip::ensure_at_startup(state).await;
            }
        });
    }
    domain_ipv6(&state, &zone).await.map(Json)
}

// ---------------------------------------------------------------------------
// The step at start
// ---------------------------------------------------------------------------

/// The decision of the one-time step at start. `published`: the address the
/// old rule gives (the panel hostname's AAAA, on an interface).
/// `aaaa_published`: a zone has an AAAA for it; `aaaa_own`: a zone has an
/// AAAA for any address on the interfaces (None: MariaDB not reachable).
/// Some(true): store new_domains = on. Some(false): nothing to store.
/// None: not known yet, try again.
fn migration_decision(
    published: Option<&str>,
    aaaa_published: Option<bool>,
    aaaa_own: Option<bool>,
    last_try: bool,
) -> Option<bool> {
    match published {
        Some(_) => aaaa_published,
        None => match aaaa_own {
            // Zones name this server, but the hostname's AAAA was not found:
            // the lookup may have failed at boot. Ask again a few times.
            Some(true) if !last_try => None,
            Some(_) => Some(false),
            None => None,
        },
    }
}

/// Runs at every agent start until it has decided once: a server that
/// publishes IPv6 today (the panel hostname's AAAA is on an interface and a
/// zone has an AAAA for it) gets `ipv6.new_domains = on`, so that new
/// domains keep getting IPv6 as they did. Nothing else changes: no zone is
/// rewritten, and a zone without a stored choice keeps what it has
/// (dns::ipv6_enabled). An operator's own choice of new_domains is never
/// replaced. Recorded in `ipv6.migrated`; nothing is created when MariaDB or
/// the pdns database are not there yet.
pub async fn migrate_at_startup(state: AppState) {
    if read_setting(&state.panel_db, MIGRATED_KEY).is_some() {
        return;
    }
    const TRIES: usize = 6;
    let mut decided = None;
    for attempt in 0..TRIES {
        let facts = server_ip::ipv6_facts(&state.panel_db, &state.config.panel_host).await;
        let published = facts.usable();
        let aaaa_published = match &published {
            Some(a) => dns::any_aaaa_for(&state, std::slice::from_ref(a)).await,
            None => None,
        };
        let aaaa_own = match &published {
            Some(_) => None,
            None => dns::any_aaaa_for(&state, &facts.on_interfaces).await,
        };
        decided = migration_decision(published.as_deref(), aaaa_published, aaaa_own, attempt + 1 == TRIES);
        if decided.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
    let Some(on) = decided else {
        tracing::warn!("IPv6 default for new domains not decided: MariaDB not reachable; trying again at the next start");
        return;
    };
    let stamp = chrono::Utc::now().to_rfc3339();
    let written = state.panel_db.with(|conn| {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )?;
        let tx = conn.unchecked_transaction()?;
        if on {
            tx.execute(
                "INSERT OR IGNORE INTO settings (key, value) VALUES (?1, 'on')",
                [NEW_DOMAINS_KEY],
            )?;
        }
        tx.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            rusqlite::params![MIGRATED_KEY, stamp],
        )?;
        tx.commit()
    });
    match written {
        Ok(()) if on => tracing::info!("this server publishes IPv6: new domains get IPv6 by default, as before"),
        Ok(()) => {}
        Err(e) => tracing::warn!(error = %e, "could not record the IPv6 default for new domains"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(published: Option<&str>, on: &[&str], setting: &str) -> Ipv6Facts {
        Ipv6Facts {
            host: "panel.example.com".into(),
            published: published.map(str::to_string),
            on_interfaces: on.iter().map(|s| s.to_string()).collect(),
            setting: setting.into(),
        }
    }

    #[test]
    fn the_step_at_start_keeps_ipv6_for_new_domains_only_where_it_is_published_today() {
        let v6 = Some("2001:db8:1::1");
        // The panel hostname's AAAA is on an interface and a zone has it.
        assert_eq!(migration_decision(v6, Some(true), None, false), Some(true));
        // Published, but no zone has an AAAA for it: nothing to keep.
        assert_eq!(migration_decision(v6, Some(false), None, false), Some(false));
        // MariaDB not reachable: ask again (and at the next start).
        assert_eq!(migration_decision(v6, None, None, false), None);
        assert_eq!(migration_decision(v6, None, None, true), None);
        assert_eq!(migration_decision(None, None, None, true), None);
        // No published address, and no zone names this server: decided.
        assert_eq!(migration_decision(None, None, Some(false), false), Some(false));
        // Zones name this server but the lookup found nothing: it may have
        // failed at boot, so it is tried again before deciding.
        assert_eq!(migration_decision(None, None, Some(true), false), None);
        assert_eq!(migration_decision(None, None, Some(true), true), Some(false));
    }

    #[test]
    fn an_address_must_be_one_of_this_servers() {
        let on = vec!["2001:db8:1::1".to_string(), "2001:db8:1::2".to_string()];
        assert_eq!(wanted_address("", &on).unwrap(), "");
        assert_eq!(wanted_address("  ", &on).unwrap(), "");
        assert_eq!(wanted_address("2001:DB8:1:0::2", &on).unwrap(), "2001:db8:1::2");
        assert_eq!(wanted_address("[2001:db8:1::1]", &on).unwrap(), "2001:db8:1::1");
        let err = wanted_address("2001:db8:9::9", &on).unwrap_err().to_string();
        assert!(err.contains("2001:db8:9::9 is not an address of this server. Choose one of: 2001:db8:1::1, 2001:db8:1::2."), "{err}");
        assert!(matches!(wanted_address("2001:db8:9::9", &on), Err(AgentError::Invalid(_))));
        assert!(wanted_address("198.51.100.127", &on).unwrap_err().to_string().contains("is not an IPv6 address"));
        assert!(wanted_address("2001:db8:9::9", &[]).unwrap_err().to_string().contains("no global IPv6 address"));
        assert_eq!(wanted_new_domains(Some("on")).unwrap(), Some(true));
        assert_eq!(wanted_new_domains(Some(" off ")).unwrap(), Some(false));
        assert_eq!(wanted_new_domains(None).unwrap(), None);
        assert!(wanted_new_domains(Some("yes")).is_err());
        let refused = refuse_clearing(3).to_string();
        assert!(refused.contains("IPv6 is on for 3 domains"), "{refused}");
    }

    #[test]
    fn the_status_notes_say_why_there_is_no_address_and_what_mail_does() {
        let none = ZoneCounts::default();
        // Several addresses, nothing chosen, no AAAA for the panel hostname.
        let n = status_notes(&facts(None, &["2001:db8:1::1", "2001:db8:1::2"], ""), &none);
        assert_eq!(n, vec!["This server has more than one IPv6 address. Choose the one to publish in DNS."]);
        // None at all.
        let n = status_notes(&facts(None, &[], ""), &none);
        assert!(n[0].starts_with("This server has no public IPv6 address."), "{n:?}");
        // The panel picked the only one: mail does not use it by itself.
        let n = status_notes(&facts(None, &["2001:db8:1::1"], ""), &none);
        assert_eq!(n.len(), 1, "{n:?}");
        assert!(n[0].contains("Outgoing mail does not use it until it is chosen here"), "{n:?}");
        // The hostname's AAAA is: nothing to say.
        assert!(status_notes(&facts(Some("2001:db8:1::1"), &["2001:db8:1::1"], ""), &none).is_empty());
        // A chosen address that has left; the hostname behind a proxy.
        let n = status_notes(&facts(None, &["2001:db8:1::1"], "2001:db8:9::9"), &none);
        assert!(n[0].contains("2001:db8:9::9, is not on this server any more"), "{n:?}");
        let n = status_notes(&facts(Some("2606:4700::1"), &["2001:db8:1::1", "2001:db8:1::2"], ""), &none);
        assert!(n[0].contains("2606:4700::1, which is not an address of this server"), "{n:?}");
        // Zones on with no address; zones with IPv6 records but off.
        let zones = ZoneCounts { on: vec!["a.example".into(), "b.example".into()], total: 3, partial: vec!["c.example".into()] };
        let n = status_notes(&facts(None, &[], ""), &zones);
        assert!(n.iter().any(|s| s.contains("IPv6 is on for 2 domains, but with no IPv6 address in effect")), "{n:?}");
        assert!(n.iter().any(|s| s.starts_with("c.example has AAAA records for this server but none at the apex")), "{n:?}");
    }

    #[test]
    fn zones_are_counted_and_the_ones_that_need_a_decision_named() {
        let z = |enabled, stored, own_aaaa| dns::ZoneIpv6 { enabled, stored, own_aaaa };
        let mut zones = BTreeMap::new();
        zones.insert("on.example".to_string(), z(true, None, true));
        zones.insert("stored.example".to_string(), z(true, Some(true), true));
        zones.insert("off.example".to_string(), z(false, Some(false), false));
        zones.insert("partial.example".to_string(), z(false, None, true));
        zones.insert("plain.example".to_string(), z(false, None, false));
        let c = zone_counts(&zones);
        assert_eq!(c.on, vec!["on.example", "stored.example"]);
        assert_eq!(c.total, 5);
        assert_eq!(c.partial, vec!["partial.example"]);
    }

    #[test]
    fn candidates_say_which_one_the_panel_hostname_names_and_which_are_private() {
        let f = facts(Some("2001:db8:1::1"), &["2001:db8:1::1", "fd00::5"], "");
        assert_eq!(candidate_note("2001:db8:1::1", &f), "the panel hostname's AAAA record points here");
        assert!(candidate_note("fd00::5", &f).contains("private address"));
        assert_eq!(candidate_note("2001:db8:1::2", &f), "");
    }

    #[test]
    fn nginx_server_blocks_are_read_with_their_names_and_ipv6_listeners() {
        let dump = "\
# configuration file /etc/nginx/nginx.conf:
http {
    upstream app { server 127.0.0.1:3000; }
    include /etc/nginx/sites-enabled/*;
}
# configuration file /etc/nginx/sites-enabled/site-shop.example.com:
server {
    listen 80;
    listen [::]:80;
    server_name shop.example.com www.shop.example.com;
    location / { return 200 \"{ok}\"; } # a } in a comment
}
server {
    listen 443 ssl http2;
    listen [::]:443 ssl http2;
    server_name shop.example.com;
}
# configuration file /etc/nginx/sites-enabled/old.example.com:
server {
    listen 80;
    server_name old.example.com;
}
# configuration file /etc/nginx/sites-enabled/wild:
server {
    listen [::]:80;
    server_name *.apps.example.com;
";
        let servers = nginx_servers(dump);
        assert_eq!(
            servers,
            vec![
                NginxServer { names: vec!["shop.example.com".into(), "www.shop.example.com".into()], ipv6: true },
                NginxServer { names: vec!["shop.example.com".into()], ipv6: true },
                NginxServer { names: vec!["old.example.com".into()], ipv6: false },
                NginxServer { names: vec!["*.apps.example.com".into()], ipv6: true },
            ]
        );
        assert!(server_name_covers("*.apps.example.com", "x.apps.example.com"));
        assert!(!server_name_covers("*.apps.example.com", "apps.example.com"));
        assert!(server_name_covers(".example.com", "example.com") && server_name_covers(".example.com", "a.example.com"));
        assert!(!server_name_covers("example.com", "www.example.com"));

        let names = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(web_item(&names(&["shop.example.com", "x.apps.example.com", "mail.example.com"]), &servers).status, Status::Ok);
        let it = web_item(&names(&["shop.example.com", "old.example.com"]), &servers);
        assert_eq!(it.status, Status::Warn);
        assert!(it.detail.starts_with("nginx does not listen on IPv6 for old.example.com"), "{}", it.detail);
        let it = web_item(&names(&["example.com"]), &servers);
        assert_eq!(it.status, Status::Ok);
        assert!(it.detail.contains("nothing to check"));
    }

    #[test]
    fn powerdns_answers_over_ipv6_when_its_local_address_has_it() {
        let a = "2001:db8:1::1";
        let ns = vec!["ns1.example.com".to_string(), "ns2.example.com".to_string()];
        let conf_v4 = "launch=gmysql\nlocal-address=198.51.100.127,127.0.0.1\nlocal-port=53\n";
        let conf_v6 = "local-address=198.51.100.127,127.0.0.1,::\n";
        assert_eq!(pdns_local_addresses(conf_v4).unwrap(), vec!["198.51.100.127", "127.0.0.1"]);
        assert!(!pdns_listens_ipv6(&pdns_local_addresses(conf_v4).unwrap(), a));
        assert!(pdns_listens_ipv6(&pdns_local_addresses(conf_v6).unwrap(), a));
        assert!(pdns_listens_ipv6(&["[2001:db8:1::1]:53".to_string()], a));
        assert!(!pdns_listens_ipv6(&["198.51.100.127:5300".to_string()], a));
        assert_eq!(pdns_local_addresses("launch=gmysql\n"), None);

        let it = nameserver_item("example.com", &ns, a, Ok(conf_v4), None);
        assert_eq!(it.status, Status::Warn);
        assert!(it.detail.contains("PowerDNS listens on IPv4 only (local-address=198.51.100.127,127.0.0.1"), "{}", it.detail);
        assert_eq!(nameserver_item("example.com", &ns, a, Ok(conf_v6), Some(true)).status, Status::Ok);
        // No dig to ask with: the configuration is enough.
        assert_eq!(nameserver_item("example.com", &ns, a, Ok(conf_v6), None).status, Status::Ok);
        assert_eq!(nameserver_item("example.com", &ns, a, Ok(conf_v6), Some(false)).status, Status::Warn);
        assert_eq!(nameserver_item("example.com", &ns, a, Err("No such file or directory"), None).status, Status::Warn);
        // Name servers elsewhere: nothing of theirs changes.
        assert_eq!(nameserver_item("example.com", &[], a, Err("x"), None).status, Status::Ok);
    }

    #[test]
    fn public_dns_is_told_apart_for_on_and_off() {
        let a = "2001:db8:1::1";
        let rec = |list: &[&str]| Lookup::Records(list.iter().map(|s| s.to_string()).collect());
        assert_eq!(public_item("example.com", a, true, &rec(&[a])).status, Status::Ok);
        assert_eq!(public_item("example.com", a, true, &Lookup::Empty).status, Status::Warn);
        assert_eq!(public_item("example.com", a, true, &rec(&["2001:db8:ffff::1"])).status, Status::Warn);
        assert_eq!(public_item("example.com", a, false, &rec(&[a])).status, Status::Warn);
        assert_eq!(public_item("example.com", a, false, &Lookup::Empty).status, Status::Ok);
        assert_eq!(public_item("example.com", a, false, &rec(&["2001:db8:ffff::1"])).status, Status::Ok);
        assert_eq!(public_item("example.com", a, true, &Lookup::Failed("no dig".into())).status, Status::Warn);
    }

    #[test]
    fn the_address_item_says_where_the_address_comes_from() {
        let devices = vec![("eth0".to_string(), "2001:db8:1::1".to_string())];
        let it = address_item(&facts(Some("2001:db8:1::1"), &["2001:db8:1::1"], ""), &devices);
        assert_eq!(it.status, Status::Ok);
        assert_eq!(it.detail, "2001:db8:1::1 is on this server (eth0): the panel hostname panel.example.com has an AAAA record for it.");
        let it = address_item(&facts(None, &["2001:db8:1::1", "2001:db8:1::2"], "2001:db8:1::1"), &devices);
        assert!(it.detail.ends_with("the address chosen under Settings > DNS & SSL > IPv6."), "{}", it.detail);
        let it = address_item(&facts(None, &["2001:db8:1::1", "2001:db8:1::2"], ""), &devices);
        assert_eq!(it.status, Status::Fail);
        assert!(it.detail.contains("Choose this server's IPv6 address under Settings > DNS & SSL > IPv6."), "{}", it.detail);
        assert_eq!(address_item(&facts(None, &[], ""), &[]).detail, "This server has no public IPv6 address.");
    }

    #[test]
    fn the_json_is_the_contracts() {
        let s = Ipv6Status {
            address: Some("2001:db8:1::1".into()),
            source: Ipv6Source::Hostname,
            setting: String::new(),
            candidates: vec![Ipv6Candidate { address: "2001:db8:1::1".into(), device: "eth0".into(), note: String::new() }],
            new_domains: "off",
            domains_on: 1,
            domains_total: 2,
            notes: vec![],
        };
        assert_eq!(
            serde_json::to_value(&s).unwrap(),
            serde_json::json!({
                "address": "2001:db8:1::1", "source": "hostname", "setting": "",
                "candidates": [{ "address": "2001:db8:1::1", "device": "eth0", "note": "" }],
                "new_domains": "off", "domains_on": 1, "domains_total": 2, "notes": [],
            })
        );
        let d = DomainIpv6 {
            domain: "example.com".into(),
            enabled: true,
            address: None,
            can_enable: false,
            records: vec![Ipv6Record { name: "example.com".into(), rtype: "AAAA", content: "2001:db8:1::1".into(), present: true }],
            checks: vec![item("address", "Address on this server", Status::Fail, "x")],
            notes: vec![],
        };
        assert_eq!(
            serde_json::to_value(&d).unwrap(),
            serde_json::json!({
                "domain": "example.com", "enabled": true, "address": null, "can_enable": false,
                "records": [{ "name": "example.com", "type": "AAAA", "content": "2001:db8:1::1", "present": true }],
                "checks": [{ "id": "address", "label": "Address on this server", "status": "fail", "detail": "x" }],
                "notes": [],
            })
        );
        assert_eq!(serde_json::to_value(Ipv6Source::None).unwrap(), serde_json::json!("none"));
        assert_eq!(serde_json::to_value(Ipv6Source::Interface).unwrap(), serde_json::json!("interface"));
    }

    // --- against a real MariaDB --------------------------------------------

    const A6: &str = "2001:db8:1::1";
    const B6: &str = "2001:db8:1::2";
    const F6: &str = "2001:db8:ffff::1";
    const V4: &str = "198.51.100.127";

    fn real_state(dir: &std::path::Path, url: &str, panel_host: &str, db: &str) -> AppState {
        let db = dir.join(db);
        let config = crate::config::Config {
            socket_path: dir.join("agent.sock"),
            agent_token: "t".repeat(40),
            panel_db_path: db.clone(),
            mariadb_url: url.into(),
            restic_password: String::new(),
            restic_repository: String::new(),
            panel_host: panel_host.into(),
            acme_email: "ops@example.com".into(),
            acme_directory: String::new(),
            vps_panel_gid: 0,
            vps_panel_uid: None,
            apps_uid: 0,
            apps_gid: 0,
        };
        let panel_db = crate::db::PanelDb::open(&db).unwrap();
        panel_db
            .with(|c| c.execute_batch("CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);"))
            .unwrap();
        AppState {
            config: std::sync::Arc::new(config),
            tasks: crate::ipc::tasks::TaskTracker::new(panel_db.clone()),
            panel_db,
            mariadb: crate::db::MariaDb::connect(url).unwrap(),
        }
    }

    /// (name, content) of the zone's records of one type, sorted.
    async fn rows(state: &AppState, zone: &str, rtype: &str) -> Vec<(String, String)> {
        use mysql_async::prelude::*;
        let mut c = state.mariadb.pool().get_conn().await.unwrap();
        let mut out: Vec<(String, String)> = c
            .exec(
                "SELECT r.name, r.content FROM pdns.records r JOIN pdns.domains d ON d.id = r.domain_id \
                 WHERE d.name = ? AND r.type = ?",
                (zone, rtype),
            )
            .await
            .unwrap();
        out.sort();
        out
    }

    async fn sql(state: &AppState, q: &str) {
        use mysql_async::prelude::*;
        let mut c = state.mariadb.pool().get_conn().await.unwrap();
        c.query_drop(q).await.unwrap();
    }

    async fn add(state: &AppState, zone: &str, name: &str, rtype: &str, content: &str) {
        use mysql_async::prelude::*;
        let mut c = state.mariadb.pool().get_conn().await.unwrap();
        c.exec_drop(
            "INSERT INTO pdns.records (domain_id, name, type, content, ttl, prio, disabled) \
             SELECT id, ?, ?, ?, 3600, 0, 0 FROM pdns.domains WHERE name = ?",
            (name, rtype, content, zone),
        )
        .await
        .unwrap();
    }

    fn named(list: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = list.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        v.sort();
        v
    }

    async fn create(state: &AppState, name: &str, ipv6: Option<bool>) -> dns::ZoneRow {
        dns::create_zone_inner(
            state,
            dns::CreateZone { name: name.into(), seed_ip: None, ipv6 },
        )
        .await
        .unwrap()
    }

    /// The zone functions and the endpoints against a real MariaDB with the
    /// pdns schema, on a scratch server, never the system's one. `ip` must be
    /// a stand-in that prints eth0 with 198.51.100.127, 2001:db8:1::1 and
    /// 2001:db8:1::2, and MAIL_CONFIG a copy of /etc/postfix (the switch of a
    /// mail domain rebuilds the mail routing):
    ///
    ///   IPV6_TEST_MARIADB=mysql://root@127.0.0.1:33099/ IPV6_TEST_DIR=<scratch dir> \
    ///   PATH=<dir with the fake ip>:$PATH MAIL_CONFIG=<copy of /etc/postfix> \
    ///   cargo test --manifest-path agent/Cargo.toml -- --ignored real_mariadb
    #[test]
    #[ignore = "needs a scratch MariaDB and a stand-in for ip (see above)"]
    fn real_mariadb_zones_follow_their_ipv6_switch() {
        // One long async fn: in a debug build its frame is larger than a
        // test thread's stack.
        std::thread::Builder::new()
            .stack_size(64 << 20)
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(real_mariadb_run())
            })
            .unwrap()
            .join()
            .unwrap();
    }

    async fn real_mariadb_run() {
        let url = std::env::var("IPV6_TEST_MARIADB").expect("IPV6_TEST_MARIADB");
        let dir = std::path::PathBuf::from(std::env::var("IPV6_TEST_DIR").expect("IPV6_TEST_DIR"));
        assert!(!url.contains("mysqld.sock") && !url.contains(":3306"), "a scratch server only, not {url}");
        assert!(std::env::var("MAIL_CONFIG").is_ok_and(|d| !d.starts_with("/etc")), "MAIL_CONFIG must name a copy");
        for f in ["panel.db", "panel.db-wal", "panel.db-shm", "published.db", "published.db-wal", "published.db-shm", "chosen.db", "chosen.db-wal", "chosen.db-shm"] {
            let _ = std::fs::remove_file(dir.join(f));
        }
        let state = real_state(&dir, &url, V4, "panel.db");
        sql(&state, "DROP DATABASE IF EXISTS pdns").await;
        sql(&state, "DROP DATABASE IF EXISTS mail").await;

        // The step at start on a server without a pdns database: decided
        // (nothing published), recorded, and nothing created.
        migrate_at_startup(state.clone()).await;
        assert!(read_setting(&state.panel_db, MIGRATED_KEY).is_some());
        assert_eq!(read_setting(&state.panel_db, NEW_DOMAINS_KEY), None);
        {
            use mysql_async::prelude::*;
            let mut c = state.mariadb.pool().get_conn().await.unwrap();
            let dbs: Vec<String> = c.query("SHOW DATABASES LIKE 'pdns'").await.unwrap();
            assert!(dbs.is_empty(), "{dbs:?}");
        }
        // A mail domain for mail.test (the table install.sh makes).
        sql(&state, "CREATE DATABASE mail").await;
        sql(&state, "CREATE TABLE mail.virtual_domains (id INT AUTO_INCREMENT PRIMARY KEY, name VARCHAR(255) NOT NULL UNIQUE) Engine=InnoDB").await;
        sql(&state, "INSERT INTO mail.virtual_domains (name) VALUES ('mail.test')").await;

        // Two addresses and no AAAA for the panel hostname: none in effect.
        let Json(s) = status(State(state.clone())).await.unwrap();
        assert_eq!((s.address.as_deref(), s.source), (None, Ipv6Source::None));
        assert_eq!(s.candidates.iter().map(|c| c.address.as_str()).collect::<Vec<_>>(), vec![A6, B6]);
        assert_eq!(s.candidates[0].device, "eth0");
        assert!(s.notes.iter().any(|n| n.contains("more than one IPv6 address")), "{:?}", s.notes);
        assert_eq!((s.new_domains, s.domains_total), ("off", 0));
        // Switching a zone on now would publish nothing: create one, then 409.
        create(&state, "early.test", Some(true)).await;
        let early = put_zone(State(state.clone()), Path("early.test".into()), Json(PutZone { enabled: true })).await;
        assert!(matches!(&early, Err(AgentError::Conflict(m)) if m.contains("Settings > DNS & SSL > IPv6")), "{:?}", early.err());
        assert!(rows(&state, "early.test", "AAAA").await.is_empty());
        // An address that is not this server's is refused.
        let refused = put_settings(State(state.clone()), Json(PutSettings { address: Some(F6.into()), new_domains: None })).await;
        assert!(matches!(&refused, Err(AgentError::Invalid(m)) if m.contains("Choose one of")), "{:?}", refused.err());
        let Json(s) = put_settings(State(state.clone()), Json(PutSettings { address: Some(A6.into()), new_domains: None }))
            .await
            .unwrap();
        assert_eq!((s.address.as_deref(), s.source, s.setting.as_str()), (Some(A6), Ipv6Source::Setting, A6));

        // New domains: off by default; on when asked; on by default once set.
        let z = create(&state, "off.test", None).await;
        assert!(!z.ipv6);
        assert!(rows(&state, "off.test", "AAAA").await.is_empty());
        let z = create(&state, "on.test", Some(true)).await;
        assert!(z.ipv6);
        assert_eq!(
            rows(&state, "on.test", "AAAA").await,
            named(&[("on.test", A6), ("www.on.test", A6), ("mail.on.test", A6), ("ns1.on.test", A6), ("ns2.on.test", A6)])
        );
        let _ = put_settings(State(state.clone()), Json(PutSettings { address: None, new_domains: Some("on".into()) }))
            .await
            .unwrap();
        assert!(create(&state, "default.test", None).await.ipv6);
        assert!(!create(&state, "mail.test", Some(false)).await.ipv6);

        // A hand-edited SPF record, a site, and a www that points elsewhere
        // over IPv6 in off.test; switched on.
        let hand = "\"v=spf1 mx include:_spf.google.com ip6:2001:db8:aaaa::/48 ~all\"";
        add(&state, "off.test", "off.test", "TXT", hand).await;
        add(&state, "off.test", "off.test", "TXT", "\"google-site-verification=abc\"").await;
        add(&state, "off.test", "shop.off.test", "A", V4).await;
        add(&state, "off.test", "blog.off.test", "A", "203.0.113.50").await;
        add(&state, "off.test", "www.off.test", "AAAA", F6).await;
        let Json(d) = get_zone(State(state.clone()), Path("off.test".into())).await.unwrap();
        assert!(!d.enabled && d.can_enable);
        assert_eq!(d.address.as_deref(), Some(A6));
        assert!(d.records.iter().all(|r| !r.present));
        assert!(d.records.iter().any(|r| r.rtype == "TXT" && r.content.contains("ip6:2001:db8:1::1 ~all")));
        assert!(d.notes.iter().any(|n| n.starts_with(&format!("www.off.test has an AAAA record for {F6}"))), "{:?}", d.notes);
        let ids: Vec<&str> = d.checks.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids[0], "address", "{ids:?}");
        assert!(ids.contains(&"web") && ids.contains(&"nameservers") && ids.contains(&"public"), "{ids:?}");
        assert!(!ids.contains(&"ptr"), "not a mail domain: {ids:?}");
        let Json(d) = put_zone(State(state.clone()), Path("off.test".into()), Json(PutZone { enabled: true })).await.unwrap();
        assert!(d.enabled);
        assert_eq!(
            rows(&state, "off.test", "AAAA").await,
            named(&[
                ("off.test", A6),
                ("mail.off.test", A6),
                ("ns1.off.test", A6),
                ("ns2.off.test", A6),
                ("shop.off.test", A6),
                ("www.off.test", F6),
            ])
        );
        let spf_on = "\"v=spf1 mx include:_spf.google.com ip6:2001:db8:aaaa::/48 ip6:2001:db8:1::1 ~all\"";
        assert_eq!(
            rows(&state, "off.test", "TXT").await,
            named(&[("off.test", spf_on), ("off.test", "\"google-site-verification=abc\"")])
        );
        // Reconcile with IPv6 on: nothing more to add, nothing removed.
        let Json(r) = dns::reconcile_zone(State(state.clone()), Path("off.test".into())).await.unwrap();
        assert!(!r.added.iter().any(|a| a.starts_with("AAAA")) && r.removed.is_empty(), "{:?} {:?}", r.added, r.removed);
        assert!(r.unchanged.contains(&"AAAA shop.off.test".to_string()));
        assert_eq!(rows(&state, "off.test", "AAAA").await.len(), 6);
        // An AAAA the operator deletes in a zone with IPv6 on comes back
        // with Reconcile: on means it is there.
        sql(&state, "DELETE FROM pdns.records WHERE name = 'shop.off.test' AND type = 'AAAA'").await;
        let Json(r) = dns::reconcile_zone(State(state.clone()), Path("off.test".into())).await.unwrap();
        assert_eq!(r.added, vec!["AAAA shop.off.test".to_string()]);

        // Off again: this server's AAAA records go, the foreign one and the
        // operator's SPF record stay exactly as they were.
        let Json(d) = put_zone(State(state.clone()), Path("off.test".into()), Json(PutZone { enabled: false })).await.unwrap();
        assert!(!d.enabled);
        assert_eq!(rows(&state, "off.test", "AAAA").await, named(&[("www.off.test", F6)]));
        assert_eq!(
            rows(&state, "off.test", "TXT").await,
            named(&[("off.test", hand), ("off.test", "\"google-site-verification=abc\"")])
        );
        // Reconcile with IPv6 off adds none back.
        let Json(r) = dns::reconcile_zone(State(state.clone()), Path("off.test".into())).await.unwrap();
        assert!(!r.added.iter().any(|a| a.starts_with("AAAA")) && r.removed.is_empty(), "{:?}", r.added);
        assert_eq!(rows(&state, "off.test", "AAAA").await, named(&[("www.off.test", F6)]));
        // An AAAA for this server added by hand in a zone with IPv6 off is
        // what Reconcile removes.
        add(&state, "off.test", "shop.off.test", "AAAA", A6).await;
        let Json(r) = dns::reconcile_zone(State(state.clone()), Path("off.test".into())).await.unwrap();
        assert_eq!(r.removed, vec![format!("AAAA shop.off.test → {A6}")]);

        // A mail domain: Reconcile writes its SPF record whole, without ip6
        // while off, with it once on; the switch edits only the term.
        let Json(_) = dns::reconcile_zone(State(state.clone()), Path("mail.test".into())).await.unwrap();
        let off_spf = dns::format_spf(Some(V4), None);
        assert_eq!(rows(&state, "mail.test", "TXT").await.iter().find(|(_, c)| c.contains("v=spf1")).unwrap().1, off_spf);
        let Json(d) = put_zone(State(state.clone()), Path("mail.test".into()), Json(PutZone { enabled: true })).await.unwrap();
        assert!(d.checks.iter().any(|c| c.id == "ptr"), "a mail domain gets the reverse DNS check");
        let on_spf = dns::format_spf(Some(V4), Some(A6));
        assert_eq!(rows(&state, "mail.test", "TXT").await.iter().find(|(_, c)| c.contains("v=spf1")).unwrap().1, on_spf);
        let Json(r) = dns::reconcile_zone(State(state.clone()), Path("mail.test".into())).await.unwrap();
        assert!(r.unchanged.iter().any(|u| u.starts_with("TXT mail.test")), "{:?}", r.unchanged);

        // Certificate preflight: a new name in a zone with IPv6 on gets its
        // AAAA, in one with IPv6 off only its A.
        dns::ensure_subdomain_records(&state, "blog.on.test").await.unwrap();
        assert!(rows(&state, "on.test", "AAAA").await.contains(&("blog.on.test".to_string(), A6.to_string())));
        dns::ensure_subdomain_records(&state, "news.off.test").await.unwrap();
        assert!(rows(&state, "off.test", "A").await.contains(&("news.off.test".to_string(), V4.to_string())));
        assert!(!rows(&state, "off.test", "AAAA").await.iter().any(|(n, _)| n == "news.off.test"));

        // A site's A record, as the panel creates it: with IPv6 on the AAAA
        // comes with it, with IPv6 off it does not; an A elsewhere brings none.
        let a = |name: &str, content: &str| dns::CreateRecord {
            name: name.into(),
            r#type: "A".into(),
            content: content.into(),
            ttl: 3600,
            prio: 0,
        };
        dns::create_record_inner(&state, "on.test".into(), a("app", V4)).await.unwrap();
        dns::create_record_inner(&state, "on.test".into(), a("ext", "203.0.113.50")).await.unwrap();
        dns::create_record_inner(&state, "off.test".into(), a("app", V4)).await.unwrap();
        let on_aaaa = rows(&state, "on.test", "AAAA").await;
        assert!(on_aaaa.contains(&("app.on.test".to_string(), A6.to_string())), "{on_aaaa:?}");
        assert!(!on_aaaa.iter().any(|(n, _)| n == "ext.on.test"));
        assert!(!rows(&state, "off.test", "AAAA").await.iter().any(|(n, _)| n == "app.off.test"));

        // The panel hostname task.
        let own = vec![A6.to_string(), B6.to_string()];
        let log = dns::point_name_here(&state, "on.test", "panel.on.test", V4, &own).await.unwrap();
        assert!(log.iter().any(|l| l == &format!("added AAAA panel.on.test -> {A6}: IPv6 is on for on.test")), "{log:?}");
        add(&state, "off.test", "panel.off.test", "AAAA", A6).await;
        let log = dns::point_name_here(&state, "off.test", "panel.off.test", V4, &own).await.unwrap();
        assert!(log.iter().any(|l| l.starts_with(&format!("removed AAAA panel.off.test -> {A6}: IPv6 is off for off.test"))), "{log:?}");
        assert!(!rows(&state, "off.test", "AAAA").await.iter().any(|(n, _)| n == "panel.off.test"));

        // A mail zone from before the choice existed, written while IPv6 was
        // published but never reconciled: AAAA for mail and a site, ip6 in
        // its SPF record, none at the apex. It counts as off, and keeps all
        // of it through Reconcile and the panel hostname task.
        {
            use mysql_async::prelude::*;
            let mut c = state.mariadb.pool().get_conn().await.unwrap();
            c.query_drop("INSERT INTO pdns.domains (name, type) VALUES ('partial.test', 'NATIVE')").await.unwrap();
            c.query_drop("INSERT INTO mail.virtual_domains (name) VALUES ('partial.test')").await.unwrap();
        }
        for (name, rtype, content) in [
            ("partial.test", "A", V4),
            ("mail.partial.test", "A", V4),
            ("mail.partial.test", "AAAA", A6),
            ("shop.partial.test", "A", V4),
            ("shop.partial.test", "AAAA", A6),
            ("panel.partial.test", "A", V4),
            ("panel.partial.test", "AAAA", A6),
            ("partial.test", "TXT", "\"v=spf1 mx ip4:198.51.100.127 ip6:2001:db8:1::1 -all\""),
        ] {
            add(&state, "partial.test", name, rtype, content).await;
        }
        let Json(r) = dns::reconcile_zone(State(state.clone()), Path("partial.test".into())).await.unwrap();
        assert!(r.removed.is_empty() && !r.added.iter().any(|a| a.starts_with("AAAA")), "{:?} {:?}", r.removed, r.added);
        let partial_aaaa = named(&[("mail.partial.test", A6), ("panel.partial.test", A6), ("shop.partial.test", A6)]);
        assert_eq!(rows(&state, "partial.test", "AAAA").await, partial_aaaa);
        let spf = rows(&state, "partial.test", "TXT").await.into_iter().find(|(_, c)| c.contains("v=spf1")).unwrap().1;
        assert_eq!(spf, dns::format_spf(Some(V4), Some(A6)));
        let log = dns::point_name_here(&state, "partial.test", "panel.partial.test", V4, &own).await.unwrap();
        assert!(!log.iter().any(|l| l.contains("removed")), "{log:?}");
        assert_eq!(rows(&state, "partial.test", "AAAA").await, partial_aaaa);
        let Json(d) = get_zone(State(state.clone()), Path("partial.test".into())).await.unwrap();
        assert!(!d.enabled);
        assert!(d.notes.iter().any(|n| n.contains("They stay as they are until you decide")), "{:?}", d.notes);
        let Json(s) = status(State(state.clone())).await.unwrap();
        assert!(s.notes.iter().any(|n| n.starts_with("partial.test has AAAA records for this server but none at the apex")), "{:?}", s.notes);
        // Outgoing mail does not hold it to IPv4 for that: no choice is stored.
        assert!(!dns::zones_stored_off(&state).await.unwrap().contains("partial.test"));

        // The list says which zones have IPv6 on; one from before the choice
        // existed counts by its apex.
        {
            use mysql_async::prelude::*;
            let mut c = state.mariadb.pool().get_conn().await.unwrap();
            c.query_drop("INSERT INTO pdns.domains (name, type) VALUES ('legacy.test', 'NATIVE'), ('plain.test', 'NATIVE')").await.unwrap();
        }
        add(&state, "legacy.test", "legacy.test", "AAAA", A6).await;
        add(&state, "legacy.test", "legacy.test", "TXT", "\"v=spf1 mx ip6:2001:db8:1::1 -all\"").await;
        add(&state, "plain.test", "plain.test", "A", V4).await;
        let Json(list) = dns::list_zones(State(state.clone())).await.unwrap();
        let on: Vec<&str> = list.iter().filter(|z| z.ipv6).map(|z| z.name.as_str()).collect();
        assert_eq!(on, vec!["default.test", "legacy.test", "mail.test", "on.test"]);

        // A new address moves the records of every zone with IPv6 on, in
        // one go; zones with IPv6 off are not touched.
        let off_before = rows(&state, "off.test", "AAAA").await;
        let Json(s) = put_settings(State(state.clone()), Json(PutSettings { address: Some(B6.into()), new_domains: None }))
            .await
            .unwrap();
        assert_eq!((s.address.as_deref(), s.domains_on, s.domains_total), (Some(B6), 4, 8));
        for zone in ["on.test", "default.test", "mail.test", "legacy.test"] {
            let aaaa = rows(&state, zone, "AAAA").await;
            assert!(!aaaa.is_empty() && aaaa.iter().all(|(_, c)| c == B6), "{zone}: {aaaa:?}");
        }
        assert_eq!(rows(&state, "mail.test", "TXT").await.iter().find(|(_, c)| c.contains("v=spf1")).unwrap().1, dns::format_spf(Some(V4), Some(B6)));
        assert_eq!(rows(&state, "legacy.test", "TXT").await, named(&[("legacy.test", "\"v=spf1 mx ip6:2001:db8:1::2 -all\"")]));
        assert_eq!(rows(&state, "off.test", "AAAA").await, off_before);
        // Back to automatic: with two addresses that is none, refused while
        // zones have IPv6 on.
        let cleared = put_settings(State(state.clone()), Json(PutSettings { address: Some(String::new()), new_domains: None })).await;
        assert!(matches!(&cleared, Err(AgentError::Conflict(m)) if m.contains("IPv6 is on for 4 domains")), "{:?}", cleared.err());
        assert_eq!(read_setting(&state.panel_db, ADDRESS_KEY).as_deref(), Some(B6));

        // Deleting a zone takes its choice with it.
        let _ = dns::delete_zone(State(state.clone()), Path("on.test".into())).await.unwrap();
        {
            use mysql_async::prelude::*;
            let mut c = state.mariadb.pool().get_conn().await.unwrap();
            let left: Vec<u32> = c
                .query("SELECT m.id FROM pdns.domainmetadata m LEFT JOIN pdns.domains d ON d.id = m.domain_id WHERE d.id IS NULL")
                .await
                .unwrap();
            assert!(left.is_empty(), "{left:?}");
        }

        // The step at start on a server that publishes IPv6 today: the panel
        // hostname's AAAA (here an address literal) is on an interface and
        // a zone has an AAAA for it. Once, and never over the operator's own choice.
        let published = real_state(&dir, &url, B6, "published.db");
        migrate_at_startup(published.clone()).await;
        assert_eq!(read_setting(&published.panel_db, NEW_DOMAINS_KEY).as_deref(), Some("on"));
        let stamp = read_setting(&published.panel_db, MIGRATED_KEY).unwrap();
        write_settings(&published.panel_db, &[(NEW_DOMAINS_KEY, Some("off"))]).unwrap();
        migrate_at_startup(published.clone()).await;
        assert_eq!(read_setting(&published.panel_db, NEW_DOMAINS_KEY).as_deref(), Some("off"));
        assert_eq!(read_setting(&published.panel_db, MIGRATED_KEY).unwrap(), stamp);
        let chosen = real_state(&dir, &url, B6, "chosen.db");
        write_settings(&chosen.panel_db, &[(NEW_DOMAINS_KEY, Some("off"))]).unwrap();
        migrate_at_startup(chosen.clone()).await;
        assert_eq!(read_setting(&chosen.panel_db, NEW_DOMAINS_KEY).as_deref(), Some("off"));
        assert!(read_setting(&chosen.panel_db, MIGRATED_KEY).is_some());
    }
}
