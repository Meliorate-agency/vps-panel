//! Which IP protocol outgoing mail uses: a server-wide default and a choice
//! per mail domain (`/mail/ip-settings`, `/mail/domains/{name}/ip-preference`).
//!
//! Postfix itself holds the server-wide state, so a GET reports what Postfix
//! does and nothing else can drift from it:
//!   - `smtp_address_preference` (main.cf): the protocol tried first. The
//!     other one is the fallback.
//!   - `smtp_bind_address6` (main.cf): set means Postfix may send over IPv6,
//!     and only from this address: the one AAAA and SPF records publish
//!     (`server_ip::published_ipv6`). With `smtp_bind_address_enforce` a
//!     connection that cannot use it fails softly and Postfix goes on to
//!     IPv4, instead of sending from an address nothing vouches for.
//!   - `-o inet_protocols=ipv4` on the `smtp` and `relay` services
//!     (master.cf): Postfix sends over IPv4 only. Receiving is not touched:
//!     smtpd keeps main.cf's `inet_protocols`.
//!   - `sender_dependent_default_transport_maps` (main.cf): an `inline:`
//!     table `@domain=smtp-ipv4:` / `@domain=smtp-ipv6:` for the domains with
//!     a choice of their own. Those two master.cf services are `smtp` with
//!     their own `smtp_address_preference`. The table lives in main.cf
//!     itself: no file or database that can be missing, stale or down, any
//!     of which makes Postfix defer all mail.
//! The per-domain choice is stored in `mail.virtual_domains.ip_preference`;
//! the table is rebuilt from it on every change and at agent start, so a
//! domain that is gone drops out by itself (and its entry is harmless until
//! then: the services it names stay).
//!
//! IPv6 is switched on for sending only when this server has an address to
//! send from that the panel hostname publishes, and either the live check
//! (`mail_check::ipv6_check`: reverse DNS, HELO name, SPF) passes or the
//! operator chose IPv6 with `force`. Without that, a destination that is slow
//! or greylists over IPv4 made Postfix fall back to an IPv6 address with no
//! reverse DNS and no SPF entry, and Gmail and others answer that with a
//! permanent 550: the mail bounced instead of being retried.

use crate::error::{AgentError, AgentResult};
use crate::exec;
use crate::fs_atomic;
use crate::handlers::mail::mail_conn;
use crate::handlers::mail_check;
use crate::server_ip::{self, Ipv6Facts};
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    Json,
};
use mysql_async::prelude::*;
use serde::Serialize;
use std::collections::BTreeMap;

const MAIN_CF: &str = "/etc/postfix/main.cf";
const MASTER_CF: &str = "/etc/postfix/master.cf";
const SENDER_MAPS: &str = "sender_dependent_default_transport_maps";

/// One change to Postfix at a time.
static APPLY_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Ipv4,
    Ipv6,
}

impl Protocol {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "ipv4" => Some(Protocol::Ipv4),
            "ipv6" => Some(Protocol::Ipv6),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Protocol::Ipv4 => "ipv4",
            Protocol::Ipv6 => "ipv6",
        }
    }

    /// The master.cf service that tries this protocol first.
    fn service(self) -> &'static str {
        match self {
            Protocol::Ipv4 => "smtp-ipv4",
            Protocol::Ipv6 => "smtp-ipv6",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Preference {
    Default,
    Ipv4,
    Ipv6,
}

impl Preference {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "default" => Some(Preference::Default),
            "ipv4" => Some(Preference::Ipv4),
            "ipv6" => Some(Preference::Ipv6),
            _ => None,
        }
    }

    /// The stored value; anything unexpected follows the server default.
    pub(crate) fn from_db(s: &str) -> Self {
        Self::parse(s.trim()).unwrap_or(Preference::Default)
    }

    fn as_str(self) -> &'static str {
        match self {
            Preference::Default => "default",
            Preference::Ipv4 => "ipv4",
            Preference::Ipv6 => "ipv6",
        }
    }

    fn protocol(self) -> Option<Protocol> {
        match self {
            Preference::Default => None,
            Preference::Ipv4 => Some(Protocol::Ipv4),
            Preference::Ipv6 => Some(Protocol::Ipv6),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MailIpDomain {
    pub name: String,
    /// What is stored for the domain.
    pub preference: Preference,
    /// What Postfix does for it right now.
    pub effective: Protocol,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MailIpSettings {
    pub default_protocol: Protocol,
    pub server_ipv4: Option<String>,
    /// The IPv6 address mail can leave from; None when there is none.
    pub server_ipv6: Option<String>,
    pub domains: Vec<MailIpDomain>,
    pub notes: Vec<String>,
    /// Postfix sends over IPv6 at the moment (as first choice or fallback).
    pub ipv6_active: bool,
}

// ---------------------------------------------------------------------------
// Reading Postfix
// ---------------------------------------------------------------------------

/// One master.cf service, as `postconf -M` prints it.
#[derive(Debug, Clone, PartialEq)]
struct Service {
    name: String,
    kind: String,
    chroot: String,
    command: String,
    /// The `-o name=value` settings.
    overrides: Vec<(String, String)>,
}

impl Service {
    fn setting(&self, name: &str) -> Option<&str> {
        self.overrides
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
struct Postfix {
    /// smtp_address_preference: ipv4, ipv6 or any.
    preference: String,
    inet_protocols: String,
    bind6: Option<String>,
    /// None on a Postfix that has no smtp_bind_address_enforce (before 3.7).
    bind_enforce: Option<String>,
    sender_maps: String,
    relayhost: String,
    myhostname: String,
    services: Vec<Service>,
}

/// Which protocols Postfix sends over.
#[derive(Debug, Clone, PartialEq)]
enum Sending {
    Ipv4Only,
    /// Both, IPv6 from this address only.
    Pinned(String),
    /// Both, IPv6 from whatever address the kernel picks: how every server
    /// was before this module existed.
    Unpinned,
}

fn allows_ipv6(inet_protocols: &str) -> bool {
    inet_protocols
        .split(|c: char| c == ',' || c.is_whitespace())
        .any(|p| matches!(p.trim().to_ascii_lowercase().as_str(), "all" | "ipv6"))
}

impl Postfix {
    fn service(&self, name: &str) -> Option<&Service> {
        self.services
            .iter()
            .find(|s| s.name == name && s.kind == "unix")
    }

    fn sending(&self) -> Sending {
        let smtp_ipv4_only = self
            .service("smtp")
            .and_then(|s| s.setting("inet_protocols"))
            .is_some_and(|v| !allows_ipv6(v));
        if smtp_ipv4_only || !allows_ipv6(&self.inet_protocols) {
            return Sending::Ipv4Only;
        }
        match &self.bind6 {
            Some(addr) => Sending::Pinned(addr.clone()),
            None => Sending::Unpinned,
        }
    }

    /// `any` (no fixed order) has no place in the API; it reads as the
    /// panel's default and a note says what Postfix is really set to.
    fn default_protocol(&self) -> Protocol {
        if self.preference == "ipv6" {
            Protocol::Ipv6
        } else {
            Protocol::Ipv4
        }
    }
}

/// `name = value` lines of `postconf` output.
fn parse_postconf(output: &str) -> BTreeMap<String, String> {
    output
        .lines()
        .filter_map(|l| {
            let (k, v) = l.split_once('=')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

/// `postconf -M` prints one service per line:
/// `smtp unix - - y - - smtp -o syslog_name=postfix/$service_name`.
fn parse_master(output: &str) -> Vec<Service> {
    let mut services = Vec::new();
    for line in output.lines() {
        let t: Vec<&str> = line.split_whitespace().collect();
        if t.len() < 8 || t[0].starts_with('#') {
            continue;
        }
        let mut overrides = Vec::new();
        let mut i = 8;
        while i < t.len() {
            let mut setting: Option<String> = None;
            if t[i] == "-o" {
                i += 1;
                if t.get(i) == Some(&"{") {
                    // `-o { name = value with spaces }`
                    let mut parts = Vec::new();
                    i += 1;
                    while i < t.len() && t[i] != "}" {
                        parts.push(t[i]);
                        i += 1;
                    }
                    setting = Some(parts.join(" "));
                } else {
                    setting = t.get(i).map(|s| s.to_string());
                }
            } else if let Some(rest) = t[i].strip_prefix("-o") {
                setting = Some(rest.to_string());
            }
            if let Some((k, v)) = setting.as_deref().and_then(|s| s.split_once('=')) {
                overrides.push((k.trim().to_string(), v.trim().to_string()));
            }
            i += 1;
        }
        services.push(Service {
            name: t[0].to_string(),
            kind: t[1].to_string(),
            chroot: t[4].to_string(),
            command: t[7].to_string(),
            overrides,
        });
    }
    services
}

fn postfix_from(main: &str, myhostname: &str, bind_enforce: Option<&str>, master: &str) -> Postfix {
    let main = parse_postconf(main);
    let get = |name: &str| main.get(name).cloned().unwrap_or_default();
    Postfix {
        preference: get("smtp_address_preference").to_ascii_lowercase(),
        inet_protocols: get("inet_protocols"),
        bind6: server_ip::parse_ipv6(&get("smtp_bind_address6")),
        bind_enforce: bind_enforce
            .map(|v| v.trim().to_ascii_lowercase())
            .filter(|v| !v.is_empty()),
        sender_maps: get(SENDER_MAPS),
        relayhost: get("relayhost"),
        myhostname: myhostname.trim().to_ascii_lowercase(),
        services: parse_master(master),
    }
}

/// Run postconf. A server without Postfix gets a plain sentence, not
/// "No such file or directory".
async fn postconf(args: &[&str]) -> AgentResult<String> {
    match exec::run("postconf", args.iter().copied()).await {
        Ok(out) => Ok(out.stdout),
        Err(AgentError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Err(AgentError::Other(
            anyhow::anyhow!("Postfix is not installed on this server"),
        )),
        Err(e) => Err(e),
    }
}

async fn read_postfix() -> AgentResult<Postfix> {
    let main = postconf(&[
        "smtp_address_preference",
        "inet_protocols",
        "smtp_bind_address6",
        SENDER_MAPS,
        "relayhost",
    ])
    .await?;
    let myhostname = postconf(&["-xh", "myhostname"]).await.unwrap_or_default();
    // Unknown to a Postfix before 3.7: then it is left alone.
    let enforce = postconf(&["-h", "smtp_bind_address_enforce"]).await.ok();
    let master = postconf(&["-M"]).await?;
    Ok(postfix_from(&main, &myhostname, enforce.as_deref(), &master))
}

// ---------------------------------------------------------------------------
// The per-domain table
// ---------------------------------------------------------------------------

/// The tables in a main.cf list value. An `inline:{...}` table keeps the
/// commas and spaces inside its braces.
fn split_tables(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0u32;
    for c in value.chars() {
        match c {
            '{' => {
                depth += 1;
                cur.push(c);
            }
            '}' => {
                depth = depth.saturating_sub(1);
                cur.push(c);
            }
            c if depth == 0 && (c == ',' || c.is_whitespace()) => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `inline:{@a.example=smtp-ipv6:,@b.example=smtp-ipv4:}`; None when no
/// domain has a choice of its own. Postfix looks a sender up as `@domain`.
fn build_table(entries: &BTreeMap<String, Protocol>) -> Option<String> {
    if entries.is_empty() {
        return None;
    }
    let body = entries
        .iter()
        .map(|(domain, p)| format!("@{domain}={}:", p.service()))
        .collect::<Vec<_>>()
        .join(",");
    Some(format!("inline:{{{body}}}"))
}

/// The entries of a table this module wrote; None for anyone else's table.
fn parse_table(table: &str) -> Option<BTreeMap<String, Protocol>> {
    let body = table.strip_prefix("inline:{")?.strip_suffix('}')?;
    let mut out = BTreeMap::new();
    for pair in body
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|p| !p.is_empty())
    {
        let (key, value) = pair.split_once('=')?;
        let proto = [Protocol::Ipv4, Protocol::Ipv6]
            .into_iter()
            .find(|p| value.strip_suffix(':') == Some(p.service()))?;
        out.insert(key.strip_prefix('@')?.to_string(), proto);
    }
    (!out.is_empty()).then_some(out)
}

/// What sender_dependent_default_transport_maps should hold: the tables
/// someone else put there, in their order, then ours.
fn sender_maps_value(current: &str, entries: &BTreeMap<String, Protocol>) -> String {
    let mut tables: Vec<String> = split_tables(current)
        .into_iter()
        .filter(|t| parse_table(t).is_none())
        .collect();
    tables.extend(build_table(entries));
    tables.join(", ")
}

/// The domains Postfix routes by our table right now.
fn applied_entries(sender_maps: &str) -> BTreeMap<String, Protocol> {
    split_tables(sender_maps)
        .iter()
        .filter_map(|t| parse_table(t))
        .flatten()
        .collect()
}

/// The domains with a choice of their own. A name that is not a plain
/// hostname never goes into main.cf.
fn explicit_entries(prefs: &[(String, Preference)]) -> BTreeMap<String, Protocol> {
    prefs
        .iter()
        .filter(|(name, _)| crate::validate::is_safe_subdomain(name))
        .filter_map(|(name, pref)| Some((name.clone(), pref.protocol()?)))
        .collect()
}

// ---------------------------------------------------------------------------
// Changing Postfix
// ---------------------------------------------------------------------------

/// How Postfix should use IPv6 for sending.
#[derive(Debug, Clone, PartialEq)]
enum Ipv6Use {
    /// Not at all: IPv4 only.
    Off,
    /// As first choice or as fallback, from this address only.
    From(String),
    /// Left as it is. For a server that has no IPv4 address and no IPv6
    /// address fit to pin: limited to IPv4 it could not send any mail.
    Untouched,
}

/// What Postfix should be set to.
#[derive(Debug, Clone, PartialEq)]
struct Desired {
    /// None leaves smtp_address_preference as it is.
    preference: Option<Protocol>,
    ipv6: Ipv6Use,
    /// Domains with a choice of their own. They are only routed while IPv6
    /// is in use: without it every domain sends over IPv4 anyway.
    entries: BTreeMap<String, Protocol>,
}

/// The edits that bring Postfix to a `Desired`, in an order that never leaves
/// it half-way: main.cf must not name a service master.cf does not have yet,
/// and IPv6 must not be open before it is pinned to its address.
#[derive(Debug, Default, PartialEq)]
struct Plan {
    /// main.cf settings (`name = value`) that are safe before master.cf.
    early: Vec<String>,
    /// postconf argument lists that edit master.cf.
    master: Vec<Vec<String>>,
    /// main.cf settings that need the master.cf changes loaded first.
    late: Vec<String>,
}

impl Plan {
    fn is_empty(&self) -> bool {
        self.early.is_empty() && self.master.is_empty() && self.late.is_empty()
    }
}

fn plan(now: &Postfix, want: &Desired) -> Plan {
    let mut p = Plan::default();
    if let Some(pref) = want.preference {
        if now.preference != pref.as_str() {
            p.early
                .push(format!("smtp_address_preference = {}", pref.as_str()));
        }
    }
    let entries = match want.ipv6 {
        Ipv6Use::From(_) => want.entries.clone(),
        _ => BTreeMap::new(),
    };
    let maps = sender_maps_value(&now.sender_maps, &entries);
    let maps_changed = maps != split_tables(&now.sender_maps).join(", ");
    let smtp_clients = ["smtp", "relay"]
        .into_iter()
        .filter_map(|name| now.service(name))
        .filter(|s| s.command == "smtp");

    match &want.ipv6 {
        Ipv6Use::From(addr) => {
            if now.bind6.as_deref() != Some(addr.as_str()) {
                p.early.push(format!("smtp_bind_address6 = {addr}"));
            }
            if now.bind_enforce.as_deref().is_some_and(|v| v != "yes") {
                p.early.push("smtp_bind_address_enforce = yes".to_string());
            }
            // A new service runs chrooted exactly when the stock one does.
            let chroot = now
                .service("smtp")
                .map(|s| s.chroot.clone())
                .unwrap_or_else(|| "n".to_string());
            for proto in [Protocol::Ipv4, Protocol::Ipv6] {
                if !entries.values().any(|p| *p == proto) {
                    continue;
                }
                let name = proto.service();
                let as_wanted = now.service(name).is_some_and(|s| {
                    s.command == "smtp"
                        && s.setting("smtp_address_preference") == Some(proto.as_str())
                        && s.setting("inet_protocols").is_none()
                });
                if !as_wanted {
                    p.master.push(vec![
                        "-M".to_string(),
                        format!(
                            "{name}/unix={name} unix - - {chroot} - - smtp -o smtp_address_preference={}",
                            proto.as_str()
                        ),
                    ]);
                }
            }
            for s in smtp_clients {
                if s.setting("inet_protocols") == Some("ipv4") {
                    p.master.push(vec![
                        "-P".to_string(),
                        "-X".to_string(),
                        format!("{}/unix/inet_protocols", s.name),
                    ]);
                }
            }
            if maps_changed {
                let line = format!("{SENDER_MAPS} = {maps}");
                // Taking entries away is safe at once; new ones may name a
                // service master.cf only gets in this run.
                if entries.is_empty() {
                    p.early.push(line);
                } else {
                    p.late.push(line);
                }
            }
        }
        Ipv6Use::Untouched => {
            if maps_changed {
                p.early.push(format!("{SENDER_MAPS} = {maps}"));
            }
        }
        Ipv6Use::Off => {
            if maps_changed {
                p.early.push(format!("{SENDER_MAPS} = {maps}"));
            }
            for s in smtp_clients {
                if s.setting("inet_protocols") != Some("ipv4") {
                    p.master.push(vec![
                        "-P".to_string(),
                        format!("{}/unix/inet_protocols=ipv4", s.name),
                    ]);
                }
            }
            // Only once Postfix no longer sends over IPv6: unpinned, it would
            // use any address for a moment.
            if now.bind6.is_some() {
                p.late.push("smtp_bind_address6 =".to_string());
            }
        }
    }
    p
}

async fn reload_postfix() {
    // Fails when Postfix is stopped; the files are right for its next start.
    if let Err(e) = exec::run("systemctl", ["reload", "postfix"]).await {
        tracing::warn!(error = %e, "postfix reload failed");
    }
}

async fn edit_main_cf(settings: &[String]) -> AgentResult<()> {
    let mut args = vec!["-e"];
    args.extend(settings.iter().map(String::as_str));
    postconf(&args).await.map(|_| ())
}

async fn run_plan(plan: &Plan) -> AgentResult<()> {
    if !plan.early.is_empty() {
        edit_main_cf(&plan.early).await?;
    }
    for args in &plan.master {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        postconf(&args).await?;
    }
    if !plan.early.is_empty() || !plan.master.is_empty() {
        exec::run("postfix", ["check"]).await?;
        reload_postfix().await;
    }
    if !plan.late.is_empty() {
        edit_main_cf(&plan.late).await?;
        exec::run("postfix", ["check"]).await?;
        reload_postfix().await;
    }
    Ok(())
}

/// Bring Postfix to `want`. Every step is checked with `postfix check`, and
/// Postfix is read back at the end; when a step fails or Postfix did not end
/// up as asked, main.cf and master.cf are put back as they were. Returns
/// whether anything had to change.
async fn converge(now: &Postfix, want: &Desired) -> AgentResult<bool> {
    let steps = plan(now, want);
    if steps.is_empty() {
        return Ok(false);
    }
    // Before anything is written: does this Postfix read the table the way
    // it is meant? A wrong answer here would misroute or defer mail.
    if matches!(want.ipv6, Ipv6Use::From(_)) {
        if let (Some(table), Some((domain, proto))) =
            (build_table(&want.entries), want.entries.iter().next())
        {
            let key = format!("@{domain}");
            let got = exec::run("postmap", ["-q", key.as_str(), table.as_str()]).await?;
            if got.stdout.trim() != format!("{}:", proto.service()) {
                return Err(AgentError::Other(anyhow::anyhow!(
                    "Postfix does not read the per-domain table as written; nothing was changed"
                )));
            }
        }
    }
    let main = tokio::fs::read(MAIN_CF).await?;
    let master = tokio::fs::read(MASTER_CF).await?;
    let result = async {
        run_plan(&steps).await?;
        let after = read_postfix().await?;
        if !plan(&after, want).is_empty() {
            return Err(AgentError::Other(anyhow::anyhow!(
                "Postfix did not take the new settings"
            )));
        }
        Ok(())
    }
    .await;
    if let Err(e) = &result {
        tracing::warn!(error = %e, "postfix change failed; putting main.cf and master.cf back");
        let _ = fs_atomic::write(std::path::Path::new(MAIN_CF), &main, 0o644).await;
        let _ = fs_atomic::write(std::path::Path::new(MASTER_CF), &master, 0o644).await;
        reload_postfix().await;
    }
    result.map(|()| true)
}

// ---------------------------------------------------------------------------
// Deciding
// ---------------------------------------------------------------------------

/// The IPv6 address mail may leave from: the one the panel hostname
/// publishes, when it is on an interface here. When the lookup gives nothing
/// (resolver trouble, or the AAAA was taken away) the address Postfix is
/// pinned to stays for as long as it is still on an interface: a failed
/// lookup at boot must not switch IPv6 off.
fn mail_ipv6(postfix: Option<&Postfix>, facts: &Ipv6Facts) -> Option<String> {
    if postfix.is_some_and(|p| !allows_ipv6(&p.inet_protocols)) {
        return None;
    }
    if facts.published.is_some() {
        return facts.usable();
    }
    match postfix.map(Postfix::sending) {
        Some(Sending::Pinned(addr)) if facts.on_interfaces.contains(&addr) => Some(addr),
        _ => None,
    }
}

/// Why there is no IPv6 address to send from, and what to do about it.
fn ipv6_missing_reason(postfix: Option<&Postfix>, facts: &Ipv6Facts, helo: &str) -> String {
    if let Some(p) = postfix.filter(|p| !allows_ipv6(&p.inet_protocols)) {
        return format!(
            "Postfix is limited to IPv4 (inet_protocols = {} in main.cf), so it neither sends nor receives over IPv6. \
             To change that: postconf -e 'inet_protocols = all' && systemctl restart postfix.",
            p.inet_protocols
        );
    }
    let host = facts.host.as_str();
    let named = host.contains('.') && host.parse::<std::net::IpAddr>().is_err();
    // The first address that is not a private (fc00::/7) one.
    let own = facts
        .on_interfaces
        .iter()
        .find(|a| !a.starts_with("fc") && !a.starts_with("fd"));
    match (&facts.published, own) {
        (Some(p), _) => format!(
            "The panel hostname {host} has an AAAA record for {p}, which is not an address of this server."
        ),
        (None, Some(a)) if named => format!(
            "This server has the IPv6 address {a}, but the panel hostname {host} has no AAAA record for it, so nothing is published for IPv6. \
             To use IPv6: add the record {host} AAAA {a}, set the reverse DNS of {a} to {helo} at your hosting provider, then run Reconcile on each domain."
        ),
        (None, Some(a)) => format!(
            "This server has the IPv6 address {a}, but no panel hostname is set, so nothing is published for IPv6. \
             Set a panel hostname that has an AAAA record for {a} (Settings > Panel hostname)."
        ),
        (None, None) => "This server has no public IPv6 address.".to_string(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Trigger {
    /// The agent started: nobody asked for a change.
    Startup,
    /// The operator saved a choice.
    Save { force: bool },
}

/// Does the live check have to run before `use_ipv6` can answer?
/// `candidate`: there is an address to send from. `kept`: Postfix already
/// sends over IPv6 from that address. `wants_ipv6`: IPv6 is the server
/// default or some domain's choice.
fn needs_check(candidate: bool, kept: bool, wants_ipv6: bool, trigger: Trigger) -> bool {
    candidate
        && match trigger {
            Trigger::Startup => !kept,
            Trigger::Save { force } => !(wants_ipv6 && (force || kept)),
        }
}

/// May Postfix send over IPv6?
///
/// Never without an address. At agent start, what was on stays on, and what
/// was off comes on only when the check passes. On a save, a passing check
/// switches it on; without one it stays on only while IPv6 is somebody's
/// explicit choice (accepted earlier, after a passing check or with force),
/// and goes off when IPv6 would merely be the fallback.
fn use_ipv6(candidate: bool, kept: bool, wants_ipv6: bool, trigger: Trigger, ready: bool) -> bool {
    candidate
        && match trigger {
            Trigger::Startup => kept || ready,
            Trigger::Save { force } => ready || (wants_ipv6 && (force || kept)),
        }
}

// ---------------------------------------------------------------------------
// Putting it together
// ---------------------------------------------------------------------------

/// The names and addresses the mail checks work with. Available without
/// Postfix too (the HELO name is then the panel hostname).
pub(crate) struct MailAddrs {
    /// The name Postfix introduces itself with.
    pub helo: String,
    pub ipv4: Option<String>,
    /// The IPv6 address mail can leave from.
    pub ipv6: Option<String>,
    pub facts: Ipv6Facts,
    /// Why `ipv6` is None, as a sentence.
    pub ipv6_missing: Option<String>,
    /// The server has an IPv4 address on an interface.
    pub has_ipv4: bool,
}

fn helo_name(postfix: Option<&Postfix>, facts: &Ipv6Facts) -> String {
    match postfix.map(|p| p.myhostname.as_str()).filter(|h| !h.is_empty()) {
        Some(h) => h.to_string(),
        None => facts.host.clone(),
    }
}

async fn server_ipv4(state: &AppState) -> Option<String> {
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        server_ip::resolve_ipv4(None, &state.config.panel_host),
    )
    .await
    .unwrap_or(None)
}

fn addrs_from(
    postfix: Option<&Postfix>,
    facts: Ipv6Facts,
    ipv4: Option<String>,
    has_ipv4: bool,
) -> MailAddrs {
    let helo = helo_name(postfix, &facts);
    let ipv6 = mail_ipv6(postfix, &facts);
    let ipv6_missing = match ipv6 {
        Some(_) => None,
        None => Some(ipv6_missing_reason(postfix, &facts, &helo)),
    };
    MailAddrs {
        helo,
        ipv4,
        ipv6,
        facts,
        ipv6_missing,
        has_ipv4,
    }
}

async fn gather_addrs(state: &AppState, postfix: Option<&Postfix>) -> MailAddrs {
    let facts = server_ip::ipv6_facts(&state.config.panel_host).await;
    let has_ipv4 = !server_ip::ipv4s_on_interfaces().await.is_empty();
    addrs_from(postfix, facts, server_ipv4(state).await, has_ipv4)
}

pub(crate) async fn mail_addrs(state: &AppState) -> MailAddrs {
    let postfix = read_postfix().await.ok();
    gather_addrs(state, postfix.as_ref()).await
}

async fn domain_prefs(state: &AppState) -> AgentResult<Vec<(String, Preference)>> {
    let mut conn = mail_conn(state).await?;
    let rows: Vec<(String, String)> = conn
        .query("SELECT name, ip_preference FROM virtual_domains ORDER BY name")
        .await?;
    Ok(rows
        .into_iter()
        .map(|(name, pref)| (name, Preference::from_db(&pref)))
        .collect())
}

/// Work out what Postfix should be set to and bring it there. `default` is
/// a new server-wide choice, `prefs` every domain's choice as it will be,
/// `known_ready` the result of a live check the caller already ran.
async fn settle(
    state: &AppState,
    postfix: &Postfix,
    addrs: &MailAddrs,
    default: Option<Protocol>,
    prefs: &[(String, Preference)],
    trigger: Trigger,
    known_ready: Option<bool>,
) -> AgentResult<bool> {
    let default_after = default.unwrap_or_else(|| postfix.default_protocol());
    let wants_ipv6 = default_after == Protocol::Ipv6
        || prefs.iter().any(|(_, p)| *p == Preference::Ipv6);
    let candidate = addrs.ipv6.is_some();
    let kept = matches!(
        (postfix.sending(), &addrs.ipv6),
        (Sending::Pinned(pinned), Some(addr)) if pinned == *addr
    );
    let ready = match known_ready {
        Some(ready) => ready,
        None if needs_check(candidate, kept, wants_ipv6, trigger) => {
            mail_check::ipv6_check(state, addrs).await.ready
        }
        None => false,
    };
    let ipv6 = match &addrs.ipv6 {
        Some(addr) if use_ipv6(candidate, kept, wants_ipv6, trigger, ready) => {
            Ipv6Use::From(addr.clone())
        }
        _ if !addrs.has_ipv4 => Ipv6Use::Untouched,
        _ => Ipv6Use::Off,
    };
    let want = Desired {
        preference: default,
        ipv6,
        entries: explicit_entries(prefs),
    };
    converge(postfix, &want).await
}

/// Refuse IPv6 as a choice while it is not fit to send from.
async fn require_ipv6_ready(state: &AppState, addrs: &MailAddrs) -> AgentResult<()> {
    let check = mail_check::ipv6_check(state, addrs).await;
    if check.ready {
        Ok(())
    } else {
        Err(AgentError::Conflict(mail_check::not_ready_message(&check)))
    }
}

fn notes(postfix: &Postfix, addrs: &MailAddrs, prefs: &[(String, Preference)]) -> Vec<String> {
    let mut notes = Vec::new();
    let default = postfix.default_protocol();
    let wants_ipv6 =
        default == Protocol::Ipv6 || prefs.iter().any(|(_, p)| *p == Preference::Ipv6);
    let v4 = addrs.ipv4.as_deref().unwrap_or("its IPv4 address");

    match (&addrs.ipv6, postfix.sending()) {
        (Some(a), Sending::Pinned(pinned)) if pinned == *a => {
            notes.push(match postfix.preference.as_str() {
                "ipv6" => format!("Postfix tries IPv6 first, from {a}, and falls back to IPv4 ({v4})."),
                "ipv4" => format!("Postfix tries IPv4 first, from {v4}, and falls back to IPv6 ({a})."),
                _ => format!("Postfix sends over IPv4 ({v4}) and IPv6 ({a})."),
            });
            if addrs.facts.published.is_none() {
                notes.push(format!(
                    "The panel hostname {} no longer has an AAAA record for {a}. Run the check before receivers start refusing mail sent over IPv6.",
                    addrs.facts.host
                ));
            }
            let applied = applied_entries(&postfix.sender_maps);
            let stale: Vec<String> = explicit_entries(prefs)
                .into_iter()
                .filter(|(name, proto)| applied.get(name) != Some(proto))
                .map(|(name, _)| name)
                .collect();
            if !stale.is_empty() {
                notes.push(format!(
                    "Postfix does not route {} as chosen yet. Save the choice again.",
                    stale.join(", ")
                ));
            }
        }
        (Some(a), Sending::Ipv4Only) => {
            notes.push(format!(
                "Outgoing mail uses IPv4 only. IPv6 ({a}) is switched on once its check passes: run the check, fix what it reports, then save this setting again."
            ));
            if wants_ipv6 {
                notes.push("Until then, mail set to IPv6 goes out over IPv4.".to_string());
            }
        }
        (Some(a), _) => notes.push(format!(
            "Postfix is not set up for the IPv6 address {a} yet. Save this setting to apply it."
        )),
        (None, sending) => {
            let reason = addrs.ipv6_missing.clone().unwrap_or_default();
            notes.push(match sending {
                Sending::Ipv4Only => format!("Outgoing mail uses IPv4 only. {reason}"),
                _ if !addrs.has_ipv4 => format!(
                    "{reason} It has no IPv4 address either, so Postfix is left to send over IPv6 as it is."
                ),
                _ => format!("{reason} Postfix can still send over IPv6 from an address nothing vouches for; save this setting to stop that."),
            });
            if wants_ipv6 {
                notes.push(
                    "IPv6 is chosen, but mail goes out over IPv4 until this server can send over IPv6."
                        .to_string(),
                );
            }
        }
    }
    if !matches!(postfix.preference.as_str(), "ipv4" | "ipv6") {
        notes.push(format!(
            "Postfix is set by hand to try IPv4 and IPv6 in no fixed order (smtp_address_preference = {}). Saving a choice here replaces that.",
            if postfix.preference.is_empty() { "any" } else { postfix.preference.as_str() }
        ));
    }
    if !postfix.relayhost.is_empty() {
        notes.push(format!(
            "Outgoing mail is handed to {} (relayhost in Postfix). This setting only decides how this server connects to it.",
            postfix.relayhost
        ));
    }
    notes
}

fn build_settings(postfix: &Postfix, addrs: &MailAddrs, prefs: &[(String, Preference)]) -> MailIpSettings {
    let default_protocol = postfix.default_protocol();
    let sending = postfix.sending();
    let applied = applied_entries(&postfix.sender_maps);
    let mut domains: Vec<MailIpDomain> = prefs
        .iter()
        .map(|(name, preference)| MailIpDomain {
            name: name.clone(),
            preference: *preference,
            effective: match sending {
                Sending::Ipv4Only => Protocol::Ipv4,
                _ => applied.get(name).copied().unwrap_or(default_protocol),
            },
        })
        .collect();
    domains.sort_by(|a, b| a.name.cmp(&b.name));
    MailIpSettings {
        default_protocol,
        server_ipv4: addrs.ipv4.clone(),
        server_ipv6: addrs.ipv6.clone(),
        domains,
        notes: notes(postfix, addrs, prefs),
        ipv6_active: sending != Sending::Ipv4Only,
    }
}

async fn current_settings(state: &AppState) -> AgentResult<MailIpSettings> {
    let postfix = read_postfix().await?;
    let prefs = domain_prefs(state).await?;
    let addrs = gather_addrs(state, Some(&postfix)).await;
    Ok(build_settings(&postfix, &addrs, &prefs))
}

/// GET /mail/ip-settings
pub async fn get_settings(State(state): State<AppState>) -> AgentResult<Json<MailIpSettings>> {
    current_settings(&state).await.map(Json)
}

fn force_flag(body: &serde_json::Value) -> bool {
    body.get("force").and_then(|v| v.as_bool()).unwrap_or(false)
}

/// PUT /mail/ip-settings  { default_protocol: "ipv4" | "ipv6", force?: bool }
pub async fn put_settings(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> AgentResult<Json<MailIpSettings>> {
    let protocol = body
        .get("default_protocol")
        .and_then(|v| v.as_str())
        .and_then(Protocol::parse)
        .ok_or_else(|| AgentError::Invalid("default_protocol must be ipv4 or ipv6".into()))?;
    let force = force_flag(&body);

    let _guard = APPLY_LOCK.lock().await;
    let postfix = read_postfix().await?;
    let prefs = domain_prefs(&state).await?;
    let addrs = gather_addrs(&state, Some(&postfix)).await;
    let known_ready = if protocol == Protocol::Ipv6 && !force {
        require_ipv6_ready(&state, &addrs).await?;
        Some(true)
    } else {
        None
    };
    settle(&state, &postfix, &addrs, Some(protocol), &prefs, Trigger::Save { force }, known_ready).await?;
    current_settings(&state).await.map(Json)
}

/// PUT /mail/domains/{name}/ip-preference  { preference: "default" | "ipv4" | "ipv6", force?: bool }
pub async fn put_domain_preference(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> AgentResult<Json<MailIpDomain>> {
    let name = name.trim().trim_end_matches('.').to_lowercase();
    let preference = body
        .get("preference")
        .and_then(|v| v.as_str())
        .and_then(Preference::parse)
        .ok_or_else(|| AgentError::Invalid("preference must be default, ipv4 or ipv6".into()))?;
    let force = force_flag(&body);

    let _guard = APPLY_LOCK.lock().await;
    let mut conn = mail_conn(&state).await?;
    let previous: Option<String> = conn
        .exec_first("SELECT ip_preference FROM virtual_domains WHERE name = ?", (&name,))
        .await?;
    let Some(previous) = previous else {
        return Err(AgentError::NotFound);
    };
    // Nothing is stored on a server where Postfix cannot be asked.
    let postfix = read_postfix().await?;
    let mut prefs = domain_prefs(&state).await?;
    let addrs = gather_addrs(&state, Some(&postfix)).await;
    let known_ready = if preference == Preference::Ipv6 && !force {
        require_ipv6_ready(&state, &addrs).await?;
        Some(true)
    } else {
        None
    };
    for (domain, pref) in prefs.iter_mut() {
        if *domain == name {
            *pref = preference;
        }
    }
    conn.exec_drop(
        "UPDATE virtual_domains SET ip_preference = ? WHERE name = ?",
        (preference.as_str(), &name),
    )
    .await?;
    if let Err(e) = settle(&state, &postfix, &addrs, None, &prefs, Trigger::Save { force }, known_ready).await {
        // Postfix kept its old routing, so the stored choice goes back too.
        let _ = conn
            .exec_drop(
                "UPDATE virtual_domains SET ip_preference = ? WHERE name = ?",
                (&previous, &name),
            )
            .await;
        return Err(e);
    }
    drop(conn);
    current_settings(&state)
        .await?
        .domains
        .into_iter()
        .find(|d| d.name == name)
        .map(Json)
        .ok_or(AgentError::NotFound)
}

/// Runs at every agent start, so every server gets here with its next
/// update, without install.sh. Brings Postfix in line with the rules at the
/// top of this file: a server whose Postfix could fall back to an IPv6
/// address nothing vouches for is limited to IPv4, one that is fit for IPv6
/// is pinned to its address, and the per-domain table is rebuilt from the
/// database. The operator's choices are not touched. No-op when Postfix is
/// already as it should be, or is not installed.
pub async fn ensure_at_startup(state: AppState) {
    if read_postfix().await.is_err() {
        return;
    }
    // MariaDB may still be starting.
    let mut prefs: Option<Vec<(String, Preference)>> = None;
    for attempt in 0..6 {
        match domain_prefs(&state).await {
            Ok(p) => {
                prefs = Some(p);
                break;
            }
            Err(e) if attempt == 5 => {
                tracing::warn!(error = %e, "mail domains not readable; per-domain mail routing left as it is");
            }
            Err(_) => tokio::time::sleep(std::time::Duration::from_secs(10)).await,
        }
    }

    let _guard = APPLY_LOCK.lock().await;
    let Ok(postfix) = read_postfix().await else {
        return;
    };
    // Without the database the table Postfix has now stands in for it.
    let prefs = prefs.unwrap_or_else(|| {
        applied_entries(&postfix.sender_maps)
            .into_iter()
            .map(|(name, proto)| {
                let pref = match proto {
                    Protocol::Ipv4 => Preference::Ipv4,
                    Protocol::Ipv6 => Preference::Ipv6,
                };
                (name, pref)
            })
            .collect()
    });
    let addrs = gather_addrs(&state, Some(&postfix)).await;
    match settle(&state, &postfix, &addrs, None, &prefs, Trigger::Startup, None).await {
        Ok(false) => {}
        Ok(true) => match read_postfix().await.map(|p| p.sending()) {
            Ok(Sending::Pinned(addr)) => {
                tracing::info!(%addr, "postfix: outgoing mail may use IPv6, from this address only")
            }
            _ => tracing::info!("postfix: outgoing mail limited to IPv4"),
        },
        Err(e) => tracing::warn!(error = %e, "could not bring postfix in line with the mail IP settings"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const V4: &str = "198.51.100.127";
    const V6: &str = "2001:db8:1::1";

    /// master.cf as Ubuntu ships it, plus the installer's submission services.
    const MASTER: &str = "\
smtp       inet  n       -       y       -       -       smtpd
pickup     unix  n       -       y       60      1       pickup
qmgr       unix  n       -       n       300     1       qmgr
smtp       unix  -       -       y       -       -       smtp
relay      unix  -       -       y       -       -       smtp -o syslog_name=postfix/$service_name
lmtp       unix  -       -       y       -       -       lmtp
maildrop   unix  -       n       n       -       -       pipe flags=DRXhu user=vmail argv=/usr/bin/maildrop -d ${recipient}
submission inet  n       -       y       -       -       smtpd -o syslog_name=postfix/submission -o smtpd_tls_security_level=encrypt -o { smtpd_relay_restrictions = permit_sasl_authenticated, reject }
";

    fn main_cf(preference: &str, bind6: &str, maps: &str) -> String {
        format!(
            "smtp_address_preference = {preference}\ninet_protocols = all\nsmtp_bind_address6 = {bind6}\n\
             sender_dependent_default_transport_maps = {maps}\nrelayhost = \n"
        )
    }

    /// A server as install.sh left it before this module: both protocols,
    /// IPv4 first, IPv6 from any address.
    fn legacy() -> Postfix {
        postfix_from(&main_cf("ipv4", "", ""), "panel.example.com\n", Some("no\n"), MASTER)
    }

    /// Apply a plan the way postconf would, for the transitions below.
    fn after(now: &Postfix, plan: &Plan) -> Postfix {
        let mut p = now.clone();
        for line in plan.early.iter().chain(plan.late.iter()) {
            let (k, v) = line.split_once('=').unwrap();
            let (k, v) = (k.trim(), v.trim());
            match k {
                "smtp_address_preference" => p.preference = v.to_string(),
                "smtp_bind_address6" => p.bind6 = server_ip::parse_ipv6(v),
                "smtp_bind_address_enforce" => p.bind_enforce = Some(v.to_string()),
                SENDER_MAPS => p.sender_maps = v.to_string(),
                other => panic!("unexpected main.cf setting {other}"),
            }
        }
        for args in &plan.master {
            match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
                ["-M", entry] => {
                    let (_, line) = entry.split_once('=').unwrap();
                    let new = parse_master(line).remove(0);
                    p.services.retain(|s| !(s.name == new.name && s.kind == new.kind));
                    p.services.push(new);
                }
                ["-P", "-X", what] => {
                    let mut it = what.split('/');
                    let (name, param) = (it.next().unwrap(), it.nth(1).unwrap());
                    let s = p.services.iter_mut().find(|s| s.name == name && s.kind == "unix").unwrap();
                    s.overrides.retain(|(k, _)| k != param);
                }
                ["-P", what] => {
                    let (path, value) = what.split_once('=').unwrap();
                    let mut it = path.split('/');
                    let (name, param) = (it.next().unwrap(), it.nth(1).unwrap());
                    let s = p.services.iter_mut().find(|s| s.name == name && s.kind == "unix").unwrap();
                    s.overrides.retain(|(k, _)| k != param);
                    s.overrides.push((param.to_string(), value.to_string()));
                }
                other => panic!("unexpected postconf call {other:?}"),
            }
        }
        p
    }

    fn entries(list: &[(&str, Protocol)]) -> BTreeMap<String, Protocol> {
        list.iter().map(|(d, p)| (d.to_string(), *p)).collect()
    }

    fn facts(published: Option<&str>, on: &[&str]) -> Ipv6Facts {
        Ipv6Facts {
            host: "panel.example.com".into(),
            published: published.map(str::to_string),
            on_interfaces: on.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn postconf_and_master_cf_are_read() {
        let p = postfix_from(
            &main_cf("ipv6", "[2001:DB8:1::1]", "inline:{@example.com=smtp-ipv4:}"),
            "Panel.Example.com\n",
            None,
            MASTER,
        );
        assert_eq!(p.preference, "ipv6");
        assert_eq!(p.default_protocol(), Protocol::Ipv6);
        assert_eq!(p.bind6.as_deref(), Some(V6));
        assert_eq!(p.bind_enforce, None);
        assert_eq!(p.myhostname, "panel.example.com");
        assert_eq!(p.relayhost, "");
        // The value keeps its own '=' signs.
        assert_eq!(p.sender_maps, "inline:{@example.com=smtp-ipv4:}");
        assert_eq!(p.sending(), Sending::Pinned(V6.into()));

        let smtp = p.service("smtp").unwrap();
        assert_eq!((smtp.kind.as_str(), smtp.chroot.as_str(), smtp.command.as_str()), ("unix", "y", "smtp"));
        assert!(smtp.overrides.is_empty());
        assert_eq!(p.service("relay").unwrap().setting("syslog_name"), Some("postfix/$service_name"));
        let submission = p.services.iter().find(|s| s.name == "submission").unwrap();
        assert_eq!(submission.setting("smtpd_tls_security_level"), Some("encrypt"));
        assert_eq!(
            submission.setting("smtpd_relay_restrictions"),
            Some("permit_sasl_authenticated, reject")
        );
        assert!(p.service("smtp-ipv6").is_none());
    }

    #[test]
    fn what_postfix_sends_over_follows_from_its_settings() {
        assert_eq!(legacy().sending(), Sending::Unpinned);
        assert_eq!(legacy().default_protocol(), Protocol::Ipv4);
        let mut p = legacy();
        p.bind6 = Some(V6.into());
        assert_eq!(p.sending(), Sending::Pinned(V6.into()));
        p.inet_protocols = "ipv4".into();
        assert_eq!(p.sending(), Sending::Ipv4Only);
        let mut p = legacy();
        p.services.iter_mut().find(|s| s.name == "smtp" && s.kind == "unix").unwrap()
            .overrides.push(("inet_protocols".into(), "ipv4".into()));
        assert_eq!(p.sending(), Sending::Ipv4Only);
        // `any` is not a choice the API has: it reads as the default.
        let mut p = legacy();
        p.preference = "any".into();
        assert_eq!(p.default_protocol(), Protocol::Ipv4);
        assert!(allows_ipv6("all") && allows_ipv6("ipv4, ipv6") && !allows_ipv6("ipv4") && !allows_ipv6(""));
    }

    #[test]
    fn the_per_domain_table_round_trips_and_leaves_other_tables_alone() {
        let e = entries(&[("b.example", Protocol::Ipv4), ("a.example", Protocol::Ipv6)]);
        let table = build_table(&e).unwrap();
        assert_eq!(table, "inline:{@a.example=smtp-ipv6:,@b.example=smtp-ipv4:}");
        assert_eq!(parse_table(&table), Some(e.clone()));
        assert_eq!(build_table(&BTreeMap::new()), None);

        // Somebody else's tables are not ours to read or drop.
        assert_eq!(parse_table("hash:/etc/postfix/sender_transport"), None);
        assert_eq!(parse_table("inline:{@x.example=relay:[smtp.example.net]:587}"), None);
        assert_eq!(parse_table("inline:{ { @x.example = smtp-ipv6: } }"), None);

        let theirs = "hash:/etc/postfix/sender_transport, inline:{ {@x.example = relay:[mx.example.net]} }";
        assert_eq!(
            split_tables(theirs),
            vec!["hash:/etc/postfix/sender_transport", "inline:{ {@x.example = relay:[mx.example.net]} }"]
        );
        let merged = sender_maps_value(theirs, &e);
        assert_eq!(merged, format!("{theirs}, {table}"));
        assert_eq!(applied_entries(&merged), e);
        // Replacing ours keeps theirs; removing ours leaves only theirs.
        let one = entries(&[("a.example", Protocol::Ipv4)]);
        assert_eq!(
            sender_maps_value(&merged, &one),
            format!("{theirs}, inline:{{@a.example=smtp-ipv4:}}")
        );
        assert_eq!(sender_maps_value(&merged, &BTreeMap::new()), theirs);
        assert_eq!(sender_maps_value("", &BTreeMap::new()), "");
    }

    #[test]
    fn only_plain_domain_names_reach_main_cf() {
        let prefs = vec![
            ("example.com".to_string(), Preference::Ipv6),
            ("follows.example".to_string(), Preference::Default),
            ("bad name}, x".to_string(), Preference::Ipv6),
            ("UPPER.example".to_string(), Preference::Ipv4),
        ];
        assert_eq!(explicit_entries(&prefs), entries(&[("example.com", Protocol::Ipv6)]));
        assert_eq!(Preference::from_db(" ipv6 "), Preference::Ipv6);
        assert_eq!(Preference::from_db("nonsense"), Preference::Default);
    }

    #[test]
    fn a_server_without_a_published_ipv6_is_limited_to_ipv4() {
        // What the agent does at its first start on an existing install.
        let now = legacy();
        let want = Desired { preference: None, ipv6: Ipv6Use::Off, entries: BTreeMap::new() };
        let steps = plan(&now, &want);
        assert_eq!(
            steps,
            Plan {
                early: vec![],
                master: vec![
                    vec!["-P".to_string(), "smtp/unix/inet_protocols=ipv4".to_string()],
                    vec!["-P".to_string(), "relay/unix/inet_protocols=ipv4".to_string()],
                ],
                late: vec![],
            }
        );
        let then = after(&now, &steps);
        assert_eq!(then.sending(), Sending::Ipv4Only);
        // The operator's protocol choice and smtpd's protocols are untouched.
        assert_eq!(then.preference, "ipv4");
        assert_eq!(then.inet_protocols, "all");
        // And the next start changes nothing.
        assert!(plan(&then, &want).is_empty());
    }

    #[test]
    fn switching_ipv6_on_pins_it_before_it_opens() {
        let off = after(&legacy(), &plan(&legacy(), &Desired { preference: None, ipv6: Ipv6Use::Off, entries: BTreeMap::new() }));
        let want = Desired {
            preference: Some(Protocol::Ipv6),
            ipv6: Ipv6Use::From(V6.into()),
            entries: entries(&[("shop.example", Protocol::Ipv4)]),
        };
        let steps = plan(&off, &want);
        // The address and the preference go into main.cf first...
        assert_eq!(
            steps.early,
            vec![
                "smtp_address_preference = ipv6".to_string(),
                format!("smtp_bind_address6 = {V6}"),
                "smtp_bind_address_enforce = yes".to_string(),
            ]
        );
        // ...then master.cf gets the service and lets smtp use IPv6...
        assert_eq!(
            steps.master,
            vec![
                vec![
                    "-M".to_string(),
                    "smtp-ipv4/unix=smtp-ipv4 unix - - y - - smtp -o smtp_address_preference=ipv4".to_string()
                ],
                vec!["-P".to_string(), "-X".to_string(), "smtp/unix/inet_protocols".to_string()],
                vec!["-P".to_string(), "-X".to_string(), "relay/unix/inet_protocols".to_string()],
            ]
        );
        // ...and only then does main.cf name the new service.
        assert_eq!(
            steps.late,
            vec!["sender_dependent_default_transport_maps = inline:{@shop.example=smtp-ipv4:}".to_string()]
        );
        let on = after(&off, &steps);
        assert_eq!(on.sending(), Sending::Pinned(V6.into()));
        assert_eq!(on.service("smtp-ipv4").unwrap().setting("smtp_address_preference"), Some("ipv4"));
        // relay keeps the setting that was not ours.
        assert_eq!(on.service("relay").unwrap().setting("syslog_name"), Some("postfix/$service_name"));
        assert!(plan(&on, &want).is_empty());
    }

    #[test]
    fn switching_ipv6_off_closes_it_before_the_address_is_dropped() {
        let on = postfix_from(
            &main_cf("ipv6", V6, "inline:{@shop.example=smtp-ipv4:}"),
            "panel.example.com",
            Some("yes"),
            &format!("{MASTER}smtp-ipv4  unix  -  -  y  -  -  smtp -o smtp_address_preference=ipv4\n"),
        );
        let want = Desired { preference: None, ipv6: Ipv6Use::Off, entries: entries(&[("shop.example", Protocol::Ipv4)]) };
        let steps = plan(&on, &want);
        assert_eq!(steps.early, vec!["sender_dependent_default_transport_maps = ".to_string()]);
        assert_eq!(steps.master.len(), 2);
        assert_eq!(steps.late, vec!["smtp_bind_address6 =".to_string()]);
        let off = after(&on, &steps);
        assert_eq!(off.sending(), Sending::Ipv4Only);
        assert_eq!(off.bind6, None);
        // The stored choice survives: the preference and the service stay.
        assert_eq!(off.preference, "ipv6");
        assert!(off.service("smtp-ipv4").is_some());
        assert!(plan(&off, &want).is_empty());
    }

    #[test]
    fn a_change_of_one_domain_touches_only_the_table() {
        let on = postfix_from(
            &main_cf("ipv4", V6, "inline:{@shop.example=smtp-ipv6:}"),
            "panel.example.com",
            Some("yes"),
            &format!("{MASTER}smtp-ipv6  unix  -  -  y  -  -  smtp -o smtp_address_preference=ipv6\n"),
        );
        let keep = Desired { preference: None, ipv6: Ipv6Use::From(V6.into()), entries: entries(&[("shop.example", Protocol::Ipv6)]) };
        assert!(plan(&on, &keep).is_empty());

        // A second domain on the same service: nothing in master.cf.
        let more = Desired {
            entries: entries(&[("shop.example", Protocol::Ipv6), ("blog.example", Protocol::Ipv6)]),
            ..keep.clone()
        };
        let steps = plan(&on, &more);
        assert!(steps.early.is_empty() && steps.master.is_empty());
        assert_eq!(
            steps.late,
            vec!["sender_dependent_default_transport_maps = inline:{@blog.example=smtp-ipv6:,@shop.example=smtp-ipv6:}".to_string()]
        );

        // Back to the server default: the entry goes at once.
        let none = Desired { entries: BTreeMap::new(), ..keep.clone() };
        let steps = plan(&on, &none);
        assert_eq!(steps.early, vec!["sender_dependent_default_transport_maps = ".to_string()]);
        assert!(steps.master.is_empty() && steps.late.is_empty());

        // A service somebody edited is written again before it is used.
        let mut edited = on.clone();
        edited.services.iter_mut().find(|s| s.name == "smtp-ipv6").unwrap().overrides.clear();
        assert_eq!(plan(&edited, &keep).master.len(), 1);

        // An address that moved is re-pinned.
        let moved = Desired { ipv6: Ipv6Use::From("2001:db8:1::2".into()), ..keep };
        assert_eq!(plan(&on, &moved).early, vec!["smtp_bind_address6 = 2001:db8:1::2".to_string()]);
    }

    #[test]
    fn a_postfix_without_bind_enforce_and_a_server_without_relay_are_handled() {
        let master = MASTER.lines().filter(|l| !l.starts_with("relay")).collect::<Vec<_>>().join("\n");
        let now = postfix_from(&main_cf("ipv4", "", ""), "panel.example.com", None, &master);
        let off = plan(&now, &Desired { preference: None, ipv6: Ipv6Use::Off, entries: BTreeMap::new() });
        assert_eq!(off.master, vec![vec!["-P".to_string(), "smtp/unix/inet_protocols=ipv4".to_string()]]);
        let on = plan(&now, &Desired { preference: None, ipv6: Ipv6Use::From(V6.into()), entries: BTreeMap::new() });
        assert_eq!(on.early, vec![format!("smtp_bind_address6 = {V6}")]);
        assert!(on.master.is_empty() && on.late.is_empty());
    }

    #[test]
    fn a_server_without_ipv4_is_never_limited_to_ipv4() {
        // No IPv4 to fall back to and no IPv6 address fit to pin: the sending
        // services stay as they are, only the stored choice is written.
        let want = Desired {
            preference: Some(Protocol::Ipv6),
            ipv6: Ipv6Use::Untouched,
            entries: entries(&[("shop.example", Protocol::Ipv6)]),
        };
        let steps = plan(&legacy(), &want);
        assert_eq!(steps.early, vec!["smtp_address_preference = ipv6".to_string()]);
        assert!(steps.master.is_empty() && steps.late.is_empty());
        let then = after(&legacy(), &steps);
        assert_eq!(then.sending(), Sending::Unpinned);
        assert!(plan(&then, &want).is_empty());
    }

    #[test]
    fn the_ipv6_address_is_the_published_one_on_an_interface() {
        let p = legacy();
        assert_eq!(mail_ipv6(Some(&p), &facts(Some(V6), &[V6])).as_deref(), Some(V6));
        // Published, but another machine's address.
        assert_eq!(mail_ipv6(Some(&p), &facts(Some("2606:4700::1"), &[V6])), None);
        // On the server, but nobody published it.
        assert_eq!(mail_ipv6(Some(&p), &facts(None, &[V6])), None);
        assert_eq!(mail_ipv6(None, &facts(Some(V6), &[V6])).as_deref(), Some(V6));

        // A lookup that fails does not unpin an address that is still there...
        let mut pinned = legacy();
        pinned.bind6 = Some(V6.into());
        assert_eq!(mail_ipv6(Some(&pinned), &facts(None, &[V6])).as_deref(), Some(V6));
        // ...but an address that left the interface is gone,
        assert_eq!(mail_ipv6(Some(&pinned), &facts(None, &[])), None);
        // and a new published address replaces the pinned one.
        assert_eq!(
            mail_ipv6(Some(&pinned), &facts(Some("2001:db8:1::2"), &[V6, "2001:db8:1::2"])).as_deref(),
            Some("2001:db8:1::2")
        );
        // Postfix limited to IPv4 by hand: no IPv6, whatever the server has.
        pinned.inet_protocols = "ipv4".into();
        assert_eq!(mail_ipv6(Some(&pinned), &facts(Some(V6), &[V6])), None);
    }

    #[test]
    fn the_reason_for_no_ipv6_says_what_to_do() {
        let p = legacy();
        let r = ipv6_missing_reason(Some(&p), &facts(None, &["fd00::5", V6]), "panel.example.com");
        assert!(r.contains(&format!("panel.example.com AAAA {V6}")), "{r}");
        assert!(r.contains("reverse DNS") && r.contains("Reconcile"), "{r}");
        let r = ipv6_missing_reason(Some(&p), &facts(Some("2606:4700::1"), &[V6]), "panel.example.com");
        assert!(r.contains("not an address of this server"), "{r}");
        assert_eq!(
            ipv6_missing_reason(Some(&p), &facts(None, &[]), "panel.example.com"),
            "This server has no public IPv6 address."
        );
        let mut ip_only = facts(None, &[V6]);
        ip_only.host = V4.into();
        let r = ipv6_missing_reason(Some(&p), &ip_only, "vps.local");
        assert!(r.contains("no panel hostname is set"), "{r}");
        let mut v4_only = legacy();
        v4_only.inet_protocols = "ipv4".into();
        let r = ipv6_missing_reason(Some(&v4_only), &facts(Some(V6), &[V6]), "panel.example.com");
        assert!(r.contains("inet_protocols = ipv4"), "{r}");
    }

    #[test]
    fn ipv6_is_only_used_when_it_was_checked_or_forced() {
        let save = Trigger::Save { force: false };
        let forced = Trigger::Save { force: true };
        // (candidate, kept, wants_ipv6, trigger, ready) -> on
        // No address: never, not even forced.
        assert!(!use_ipv6(false, false, true, forced, true));
        assert!(!needs_check(false, false, true, save));

        // Agent start. What was on and pinned stays on, without a lookup.
        assert!(!needs_check(true, true, false, Trigger::Startup));
        assert!(use_ipv6(true, true, false, Trigger::Startup, false));
        // What was off (or unpinned) comes on only when the check passes.
        assert!(needs_check(true, false, true, Trigger::Startup));
        assert!(use_ipv6(true, false, false, Trigger::Startup, true));
        assert!(!use_ipv6(true, false, true, Trigger::Startup, false));

        // A save with IPv4 everywhere: IPv6 is the fallback only when it passes.
        assert!(needs_check(true, false, false, save));
        assert!(use_ipv6(true, false, false, save, true));
        assert!(!use_ipv6(true, false, false, save, false));
        // ...and a fallback that no longer passes is switched off.
        assert!(needs_check(true, true, false, save));
        assert!(!use_ipv6(true, true, false, save, false));

        // IPv6 chosen with force: on without a check.
        assert!(!needs_check(true, false, true, forced));
        assert!(use_ipv6(true, false, true, forced, false));
        // A later save elsewhere does not undo that choice.
        assert!(!needs_check(true, true, true, save));
        assert!(use_ipv6(true, true, true, save, false));
        // Force on a save that chooses no IPv6 anywhere opens nothing.
        assert!(!use_ipv6(true, false, false, forced, false));
    }

    fn addrs(postfix: &Postfix, f: Ipv6Facts) -> MailAddrs {
        addrs_from(Some(postfix), f, Some(V4.to_string()), true)
    }

    #[test]
    fn settings_report_what_postfix_does_not_what_was_asked() {
        let prefs = vec![
            ("shop.example".to_string(), Preference::Ipv6),
            ("blog.example".to_string(), Preference::Default),
            ("old.example".to_string(), Preference::Ipv4),
        ];
        // IPv6 chosen everywhere it can be, on a server that cannot use it.
        let mut off = legacy();
        off.preference = "ipv6".into();
        off.services.iter_mut().find(|s| s.name == "smtp" && s.kind == "unix").unwrap()
            .overrides.push(("inet_protocols".into(), "ipv4".into()));
        let s = build_settings(&off, &addrs(&off, facts(None, &[V6])), &prefs);
        assert_eq!(s.default_protocol, Protocol::Ipv6);
        assert_eq!(s.server_ipv4.as_deref(), Some(V4));
        assert_eq!(s.server_ipv6, None);
        assert!(!s.ipv6_active);
        assert_eq!(
            s.domains.iter().map(|d| (d.name.as_str(), d.preference, d.effective)).collect::<Vec<_>>(),
            vec![
                ("blog.example", Preference::Default, Protocol::Ipv4),
                ("old.example", Preference::Ipv4, Protocol::Ipv4),
                ("shop.example", Preference::Ipv6, Protocol::Ipv4),
            ]
        );
        assert!(s.notes[0].starts_with("Outgoing mail uses IPv4 only. This server has the IPv6 address"), "{:?}", s.notes);
        assert!(s.notes[1].contains("IPv6 is chosen, but mail goes out over IPv4"), "{:?}", s.notes);

        // The same choices on a server that sends over IPv6.
        let on = postfix_from(
            &main_cf("ipv4", V6, "inline:{@old.example=smtp-ipv4:,@shop.example=smtp-ipv6:}"),
            "panel.example.com",
            Some("yes"),
            MASTER,
        );
        let s = build_settings(&on, &addrs(&on, facts(Some(V6), &[V6])), &prefs);
        assert_eq!(s.server_ipv6.as_deref(), Some(V6));
        assert!(s.ipv6_active);
        assert_eq!(
            s.domains.iter().map(|d| d.effective).collect::<Vec<_>>(),
            vec![Protocol::Ipv4, Protocol::Ipv4, Protocol::Ipv6]
        );
        assert_eq!(
            s.notes,
            vec![format!("Postfix tries IPv4 first, from {V4}, and falls back to IPv6 ({V6}).")]
        );

        // A choice Postfix has not been given yet shows as what really happens.
        let drift = postfix_from(&main_cf("ipv4", V6, ""), "panel.example.com", Some("yes"), MASTER);
        let s = build_settings(&drift, &addrs(&drift, facts(Some(V6), &[V6])), &prefs);
        assert_eq!(s.domains[2].effective, Protocol::Ipv4);
        assert!(s.notes.iter().any(|n| n.contains("does not route old.example, shop.example as chosen yet")), "{:?}", s.notes);
    }

    #[test]
    fn notes_cover_a_ready_but_unused_ipv6_a_hand_set_order_and_a_relay() {
        let mut off = legacy();
        off.services.iter_mut().find(|s| s.name == "smtp" && s.kind == "unix").unwrap()
            .overrides.push(("inet_protocols".into(), "ipv4".into()));
        off.preference = "any".into();
        off.relayhost = "[smtp.example.net]:587".into();
        let s = build_settings(&off, &addrs(&off, facts(Some(V6), &[V6])), &[]);
        assert_eq!(s.default_protocol, Protocol::Ipv4);
        assert_eq!(s.server_ipv6.as_deref(), Some(V6));
        assert!(!s.ipv6_active);
        assert!(s.notes[0].contains("switched on once its check passes"), "{:?}", s.notes);
        assert!(s.notes.iter().any(|n| n.contains("smtp_address_preference = any")), "{:?}", s.notes);
        assert!(s.notes.iter().any(|n| n.contains("[smtp.example.net]:587")), "{:?}", s.notes);

        // Before the agent's first start on an old install.
        let s = build_settings(&legacy(), &addrs(&legacy(), facts(None, &[])), &[]);
        assert!(s.ipv6_active);
        assert!(s.notes[0].contains("can still send over IPv6 from an address nothing vouches for"), "{:?}", s.notes);

        // A server with IPv6 only is left sending the way it does.
        let v6_only = addrs_from(Some(&legacy()), facts(None, &[V6]), None, false);
        let s = build_settings(&legacy(), &v6_only, &[]);
        assert!(s.notes[0].contains("left to send over IPv6 as it is"), "{:?}", s.notes);
    }

    #[test]
    fn the_json_is_the_contracts() {
        let s = MailIpSettings {
            default_protocol: Protocol::Ipv4,
            server_ipv4: Some(V4.into()),
            server_ipv6: None,
            domains: vec![MailIpDomain {
                name: "example.com".into(),
                preference: Preference::Default,
                effective: Protocol::Ipv4,
            }],
            notes: vec![],
            ipv6_active: false,
        };
        assert_eq!(
            serde_json::to_value(&s).unwrap(),
            serde_json::json!({
                "default_protocol": "ipv4",
                "server_ipv4": V4,
                "server_ipv6": null,
                "domains": [{ "name": "example.com", "preference": "default", "effective": "ipv4" }],
                "notes": [],
                "ipv6_active": false,
            })
        );
        assert!(force_flag(&serde_json::json!({ "force": true })));
        assert!(!force_flag(&serde_json::json!({ "force": "yes" })));
        assert!(!force_flag(&serde_json::json!({})));
    }
}
