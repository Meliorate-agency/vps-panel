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
//!     a choice of their own, and `@domain=smtp-ipv4only:` for the domains
//!     held to IPv4 (below). The first two master.cf services are `smtp`
//!     with their own `smtp_address_preference`, the third is `smtp` with
//!     `inet_protocols=ipv4`. The table lives in main.cf itself: no file or
//!     database that can be missing, stale or down, any of which makes
//!     Postfix defer all mail.
//!   - `smtp_bind_address` (main.cf): the IPv4 address Postfix sends from.
//!     Set only on a server with several public addresses, when the one A
//!     and SPF records publish is not the one the kernel sends from by
//!     itself (`ipv4_source`). It is written as
//!     `smtp_bind_address = $vps_panel_smtp_bind_address` with the address
//!     in that parameter of our own. That spelling is how a pin of this
//!     module is told from an address the operator set by hand, which is
//!     never touched.
//! The per-domain choice is stored in `mail.virtual_domains.ip_preference`;
//! the table is rebuilt from it on every change, when a mail domain is
//! created and at agent start, so a domain that is gone drops out by itself
//! (and its entry is harmless until then: the services it names stay).
//!
//! IPv6 is switched on for sending only when this server has an address to
//! send from that the panel hostname publishes, and either the live check
//! (`mail_check::ipv6_check`: reverse DNS, HELO name, SPF) passes or the
//! operator chose IPv6 with `force`. Without that, a destination that is slow
//! or greylists over IPv4 made Postfix fall back to an IPv6 address with no
//! reverse DNS and no SPF entry, and Gmail and others answer that with a
//! permanent 550: the mail bounced instead of being retried.
//!
//! That check sees the mail domains of its day. A domain added later, or
//! one whose SPF record is changed where its DNS is hosted, can lack `ip6:`,
//! and its mail then fails SPF whenever Postfix uses IPv6. So each time the
//! table is built the SPF record of every domain is looked up, and a domain
//! whose record does not allow the IPv6 address is held to IPv4
//! (`held_domains`) until it does. A domain set to IPv6 with `force` is the
//! operator's to decide (`virtual_domains.ip_forced`) and is never held.

use crate::error::{AgentError, AgentResult};
use crate::exec;
use crate::handlers::mail::mail_conn;
use crate::handlers::mail_check::{self, Dig, Spf};
use crate::server_ip::{self, Ipv4Facts, Ipv6Facts};
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    Json,
};
use mysql_async::prelude::*;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

const SENDER_MAPS: &str = "sender_dependent_default_transport_maps";
const BIND4: &str = "smtp_bind_address";
/// Our own main.cf parameter. It holds the IPv4 address Postfix is pinned
/// to, and `smtp_bind_address` refers to it.
const BIND4_PIN: &str = "vps_panel_smtp_bind_address";

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
}

/// How the per-domain table sends one domain's mail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    /// IPv4 first, IPv6 as fallback.
    Ipv4,
    /// IPv6 first, IPv4 as fallback.
    Ipv6,
    /// IPv4 and nothing else: the domain's SPF record does not allow the
    /// IPv6 address.
    Ipv4Only,
}

impl Route {
    const ALL: [Route; 3] = [Route::Ipv4, Route::Ipv6, Route::Ipv4Only];

    fn first(protocol: Protocol) -> Self {
        match protocol {
            Protocol::Ipv4 => Route::Ipv4,
            Protocol::Ipv6 => Route::Ipv6,
        }
    }

    /// The master.cf service that sends this way.
    fn service(self) -> &'static str {
        match self {
            Route::Ipv4 => "smtp-ipv4",
            Route::Ipv6 => "smtp-ipv6",
            Route::Ipv4Only => "smtp-ipv4only",
        }
    }

    /// The `-o` setting that makes the service send this way.
    fn setting(self) -> &'static str {
        match self {
            Route::Ipv4 => "smtp_address_preference=ipv4",
            Route::Ipv6 => "smtp_address_preference=ipv6",
            Route::Ipv4Only => "inet_protocols=ipv4",
        }
    }

    /// The protocol tried first.
    fn protocol(self) -> Protocol {
        match self {
            Route::Ipv6 => Protocol::Ipv6,
            Route::Ipv4 | Route::Ipv4Only => Protocol::Ipv4,
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

/// One mail domain's choice, as the database has it.
#[derive(Debug, Clone, PartialEq)]
struct DomainPref {
    name: String,
    preference: Preference,
    /// The choice was made with `force`. Only an IPv6 choice is kept that
    /// way: it stands whatever the domain's SPF record says.
    forced: bool,
}

impl DomainPref {
    fn forced_ipv6(&self) -> bool {
        self.forced && self.preference == Preference::Ipv6
    }
}

/// One master.cf service, as `postconf -M` prints it.
#[derive(Debug, Clone, PartialEq)]
struct Service {
    name: String,
    kind: String,
    chroot: String,
    command: String,
    /// The `-o name=value` settings.
    overrides: Vec<(String, String)>,
    /// The whole line, to put the service back as it was.
    line: String,
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
    /// smtp_bind_address as main.cf spells it.
    bind4_raw: String,
    /// The address in our own parameter (BIND4_PIN), while
    /// smtp_bind_address refers to it.
    bind4_pin: Option<String>,
    /// main.cf still has our parameter though nothing refers to it: the
    /// operator replaced smtp_bind_address by hand. postconf warns about the
    /// unused line on every run.
    pin_left_over: bool,
    /// None on a Postfix that has no smtp_bind_address_enforce (before 3.7).
    bind_enforce: Option<String>,
    sender_maps: String,
    relayhost: String,
    myhostname: String,
    services: Vec<Service>,
}

/// Who set smtp_bind_address, the IPv4 address Postfix sends from.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Bind4 {
    Unset,
    /// This module, to this address. None: our parameter is gone or empty,
    /// and Postfix sends from the default address.
    Ours(Option<String>),
    /// The operator, to this value. Never touched.
    ByHand(String),
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

    fn bind4(&self) -> Bind4 {
        let raw = self.bind4_raw.trim();
        if raw.is_empty() {
            Bind4::Unset
        } else if is_pin_reference(raw) {
            Bind4::Ours(self.bind4_pin.clone())
        } else {
            // Postfix takes the address in brackets too.
            let bare = raw.trim_start_matches('[').trim_end_matches(']');
            Bind4::ByHand(server_ip::parse_ipv4(bare).unwrap_or_else(|| raw.to_string()))
        }
    }
}

/// Is this smtp_bind_address value the reference to our own parameter?
fn is_pin_reference(raw: &str) -> bool {
    let raw = raw.trim();
    raw.strip_prefix('$')
        .map(|name| name.trim_start_matches('{').trim_end_matches('}'))
        == Some(BIND4_PIN)
}

/// Does main.cf define this parameter? Only asked for our own one: postconf
/// does not print a parameter nothing refers to.
fn main_cf_defines(main_cf: &str, name: &str) -> bool {
    main_cf
        .lines()
        .filter_map(|l| l.split_once('='))
        .any(|(k, _)| k.trim_end() == name)
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
/// `smtp unix - - y - - smtp -o syslog_name=postfix/$service_name`, and a
/// setting whose value has spaces as `-o {name=value with spaces}`.
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
                if t.get(i).is_some_and(|s| s.starts_with('{')) {
                    // Up to the closing brace, as written in master.cf
                    // (`{ name = value }`) or as postconf prints it.
                    let mut parts = Vec::new();
                    while i < t.len() {
                        parts.push(t[i]);
                        if t[i].ends_with('}') {
                            break;
                        }
                        i += 1;
                    }
                    let joined = parts.join(" ");
                    let inner = joined.trim_start_matches('{').trim_end_matches('}');
                    setting = Some(inner.trim().to_string());
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
            line: line.trim().to_string(),
        });
    }
    services
}

fn postfix_from(main: &str, myhostname: &str, bind_enforce: Option<&str>, master: &str) -> Postfix {
    let main = parse_postconf(main);
    let get = |name: &str| main.get(name).cloned().unwrap_or_default();
    let bind4_raw = get(BIND4);
    // postconf prints our parameter only while smtp_bind_address refers to it.
    let bind4_pin = server_ip::parse_ipv4(&get(BIND4_PIN)).filter(|_| is_pin_reference(&bind4_raw));
    Postfix {
        preference: get("smtp_address_preference").to_ascii_lowercase(),
        inet_protocols: get("inet_protocols"),
        bind6: server_ip::parse_ipv6(&get("smtp_bind_address6")),
        bind4_raw,
        bind4_pin,
        pin_left_over: false,
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
    // Our own parameter is unknown to postconf unless smtp_bind_address
    // refers to it; that is a warning, the rest is printed all the same.
    let main = postconf(&[
        "smtp_address_preference",
        "inet_protocols",
        "smtp_bind_address6",
        BIND4,
        BIND4_PIN,
        SENDER_MAPS,
        "relayhost",
        "config_directory",
    ])
    .await?;
    let myhostname = postconf(&["-xh", "myhostname"]).await.unwrap_or_default();
    // Unknown to a Postfix before 3.7: then it is left alone.
    let enforce = postconf(&["-h", "smtp_bind_address_enforce"]).await.ok();
    let master = postconf(&["-M"]).await?;
    let mut postfix = postfix_from(&main, &myhostname, enforce.as_deref(), &master);
    if !matches!(postfix.bind4(), Bind4::Ours(_)) {
        // Where the Postfix tools read main.cf: MAIL_CONFIG when the
        // environment has it, as they do themselves, else what postconf says.
        let dir = std::env::var("MAIL_CONFIG")
            .ok()
            .filter(|dir| !dir.is_empty())
            .or_else(|| parse_postconf(&main).remove("config_directory"))
            .unwrap_or_default();
        let main_cf = tokio::fs::read_to_string(std::path::Path::new(&dir).join("main.cf")).await;
        postfix.pin_left_over = main_cf.is_ok_and(|text| main_cf_defines(&text, BIND4_PIN));
    }
    Ok(postfix)
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
fn build_table(entries: &BTreeMap<String, Route>) -> Option<String> {
    if entries.is_empty() {
        return None;
    }
    let body = entries
        .iter()
        .map(|(domain, route)| format!("@{domain}={}:", route.service()))
        .collect::<Vec<_>>()
        .join(",");
    Some(format!("inline:{{{body}}}"))
}

/// The entries of a table this module wrote; None for anyone else's table.
fn parse_table(table: &str) -> Option<BTreeMap<String, Route>> {
    let body = table.strip_prefix("inline:{")?.strip_suffix('}')?;
    let mut out = BTreeMap::new();
    for pair in body
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|p| !p.is_empty())
    {
        let (key, value) = pair.split_once('=')?;
        let route = Route::ALL
            .into_iter()
            .find(|r| value.strip_suffix(':') == Some(r.service()))?;
        out.insert(key.strip_prefix('@')?.to_string(), route);
    }
    (!out.is_empty()).then_some(out)
}

/// What sender_dependent_default_transport_maps should hold: the tables
/// someone else put there, in their order, then ours.
fn sender_maps_value(current: &str, entries: &BTreeMap<String, Route>) -> String {
    let mut tables: Vec<String> = split_tables(current)
        .into_iter()
        .filter(|t| parse_table(t).is_none())
        .collect();
    tables.extend(build_table(entries));
    tables.join(", ")
}

/// The domains Postfix routes by our table right now.
fn applied_entries(sender_maps: &str) -> BTreeMap<String, Route> {
    split_tables(sender_maps)
        .iter()
        .filter_map(|t| parse_table(t))
        .flatten()
        .collect()
}

/// The domains the table holds to IPv4.
fn applied_holds(sender_maps: &str) -> BTreeSet<String> {
    applied_entries(sender_maps)
        .into_iter()
        .filter(|(_, route)| *route == Route::Ipv4Only)
        .map(|(name, _)| name)
        .collect()
}

/// A name that is not a plain hostname never goes into main.cf.
fn fit_for_main_cf(name: &str) -> bool {
    crate::validate::is_safe_subdomain(name)
}

/// The table's entries: a domain in `held` is sent over IPv4 only, any
/// other one with a choice of its own by that choice. A domain that follows
/// the server default has no entry.
fn routes(domains: &[DomainPref], held: &BTreeSet<String>) -> BTreeMap<String, Route> {
    domains
        .iter()
        .filter(|d| fit_for_main_cf(&d.name))
        .filter_map(|d| {
            let route = if held.contains(&d.name) && !d.forced_ipv6() {
                Route::Ipv4Only
            } else {
                Route::first(d.preference.protocol()?)
            };
            Some((d.name.clone(), route))
        })
        .collect()
}

/// The domains whose SPF record has to be looked up before the table is
/// built: all of them, or only `added`, a domain created just now. Not one
/// set to IPv6 with force: that choice stands whatever the record says.
fn domains_to_look_up(domains: &[DomainPref], added: Option<&str>) -> Vec<String> {
    domains
        .iter()
        .filter(|d| fit_for_main_cf(&d.name) && !d.forced_ipv6())
        .filter(|d| added.is_none_or(|name| name == d.name))
        .map(|d| d.name.clone())
        .collect()
}

/// The domains to hold to IPv4 because their SPF record does not vouch for
/// the IPv6 address mail would leave from.
///
/// `spf` is each domain's record checked against that address just now; a
/// domain missing from it was not looked up or not reached in time. A record
/// that allows the address, or no SPF record at all (then IPv4 is no better
/// off), leaves the domain free. A record that does not allow it holds the
/// domain. A lookup that failed says nothing, so what is known stands:
/// where Postfix already sends over IPv6 (`established`) the domain stays as
/// the table has it (`held_now`), but a domain created just now (`added`),
/// and every domain when IPv6 is only now being switched on, is held until
/// a lookup succeeds.
fn held_domains(
    domains: &[DomainPref],
    spf: &[(String, Spf)],
    established: bool,
    held_now: &BTreeSet<String>,
    added: Option<&str>,
) -> BTreeSet<String> {
    domains
        .iter()
        .filter(|d| fit_for_main_cf(&d.name) && !d.forced_ipv6())
        .filter(|d| {
            match spf.iter().find(|(name, _)| *name == d.name).map(|(_, r)| r) {
                Some(Spf::Pass) | Some(Spf::NoRecord) => false,
                Some(Spf::Unknown(_)) | None => {
                    !established || added == Some(d.name.as_str()) || held_now.contains(&d.name)
                }
                Some(_) => true,
            }
        })
        .map(|d| d.name.clone())
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

/// Which IPv4 address Postfix should send from.
#[derive(Debug, Clone, PartialEq)]
enum Pin4 {
    /// This one: the address records publish, on a server where the kernel
    /// would pick another.
    To(String),
    /// The one the kernel picks. A pin of this module goes.
    Off,
    /// As it is: what the kernel would pick could not be read just now.
    Keep,
}

/// What Postfix should be set to.
#[derive(Debug, Clone, PartialEq)]
struct Desired {
    /// None leaves smtp_address_preference as it is.
    preference: Option<Protocol>,
    ipv6: Ipv6Use,
    /// Domains with a choice of their own, and the ones held to IPv4. They
    /// are only routed while IPv6 is in use: without it every domain sends
    /// over IPv4 anyway.
    entries: BTreeMap<String, Route>,
    pin4: Pin4,
}

/// The edits that bring Postfix to a `Desired`, in an order that never leaves
/// it half-way: main.cf must not name a service master.cf does not have yet,
/// IPv6 must not be open before it is pinned to its address and before the
/// table that holds domains to IPv4 is in place, and it must be closed
/// before either is taken away.
#[derive(Debug, Default, PartialEq)]
struct Plan {
    /// main.cf parameters to take out (`postconf -X`), before anything else.
    drop: Vec<String>,
    /// main.cf settings (`name = value`) that are safe before master.cf.
    early: Vec<String>,
    /// postconf argument lists that edit master.cf.
    master: Vec<Vec<String>>,
    /// main.cf settings that need the master.cf changes loaded first.
    late: Vec<String>,
    /// postconf argument lists that let the stock services send over IPv6:
    /// the last step.
    open: Vec<Vec<String>>,
}

impl Plan {
    fn is_empty(&self) -> bool {
        self.drop.is_empty()
            && self.early.is_empty()
            && self.master.is_empty()
            && self.late.is_empty()
            && self.open.is_empty()
    }
}

/// The parameter a `name = value` setting is about.
fn name_of(setting: &str) -> &str {
    setting.split('=').next().unwrap_or(setting).trim()
}

/// Does this master.cf service send the way `route` asks?
fn service_sends(s: &Service, route: Route) -> bool {
    let limited = s.setting("inet_protocols");
    let first = s.setting("smtp_address_preference");
    s.command == "smtp"
        && match route {
            Route::Ipv4 => first == Some("ipv4") && limited.is_none(),
            Route::Ipv6 => first == Some("ipv6") && limited.is_none(),
            Route::Ipv4Only => limited == Some("ipv4"),
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

    // The IPv4 address. The pin goes in with the first write and a pin that
    // is no longer right goes before anything else: with
    // smtp_bind_address_enforce (set below when IPv6 comes on, and it covers
    // both protocols) an address that left the server would defer all mail
    // sent over IPv4. An address the operator set by hand is not ours to
    // change either way.
    match (&want.pin4, now.bind4()) {
        (Pin4::To(addr), Bind4::Unset) => {
            p.early.push(format!("{BIND4_PIN} = {addr}"));
            p.early.push(format!("{BIND4} = ${BIND4_PIN}"));
        }
        (Pin4::To(addr), Bind4::Ours(at)) if at.as_deref() != Some(addr.as_str()) => {
            p.early.push(format!("{BIND4_PIN} = {addr}"));
        }
        (Pin4::Off, Bind4::Ours(_)) => {
            p.drop.push(BIND4.to_string());
            p.drop.push(BIND4_PIN.to_string());
        }
        _ => {}
    }
    if now.pin_left_over
        && !p.drop.iter().any(|name| name == BIND4_PIN)
        && !p.early.iter().any(|setting| name_of(setting) == BIND4_PIN)
    {
        p.drop.push(BIND4_PIN.to_string());
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
            for route in Route::ALL {
                if !entries.values().any(|r| *r == route) {
                    continue;
                }
                let name = route.service();
                if !now.service(name).is_some_and(|s| service_sends(s, route)) {
                    p.master.push(vec![
                        "-M".to_string(),
                        format!("{name}/unix={name} unix - - {chroot} - - smtp -o {}", route.setting()),
                    ]);
                }
            }
            if maps_changed {
                let line = format!("{SENDER_MAPS} = {maps}");
                // Taking every entry away is safe at once; new ones may name
                // a service master.cf only gets in this run.
                if entries.is_empty() {
                    p.early.push(line);
                } else {
                    p.late.push(line);
                }
            }
            // Only now, with the table in place: it is what keeps a domain
            // whose SPF record lacks the IPv6 address on IPv4.
            for s in smtp_clients {
                if s.setting("inet_protocols") == Some("ipv4") {
                    p.open.push(vec![
                        "-P".to_string(),
                        "-X".to_string(),
                        format!("{}/unix/inet_protocols", s.name),
                    ]);
                }
            }
        }
        Ipv6Use::Untouched => {
            if maps_changed {
                p.early.push(format!("{SENDER_MAPS} = {maps}"));
            }
        }
        Ipv6Use::Off => {
            for s in smtp_clients {
                if s.setting("inet_protocols") != Some("ipv4") {
                    p.master.push(vec![
                        "-P".to_string(),
                        format!("{}/unix/inet_protocols=ipv4", s.name),
                    ]);
                }
            }
            // Only once the stock services no longer send over IPv6: the
            // table goes (its services still can, and a domain it held to
            // IPv4 would be free), and with it the address. Unpinned, Postfix
            // would use any address for a moment.
            if maps_changed {
                p.late.push(format!("{SENDER_MAPS} = {maps}"));
            }
            if now.bind6.is_some() {
                p.late.push("smtp_bind_address6 =".to_string());
            }
        }
    }
    p
}

/// The postconf call that puts one master.cf edit back: the service, or the
/// one setting of it, as `now` has it.
fn undo_master(args: &[String], now: &Postfix) -> Option<Vec<String>> {
    // `name/type=...` for a whole service, `name/type/setting[=value]` for one setting.
    let path = args.last()?.split('=').next()?;
    let mut parts = path.split('/');
    let (name, kind, setting) = (parts.next()?, parts.next()?, parts.next());
    let service = now
        .services
        .iter()
        .find(|s| s.name == name && s.kind == kind);
    let call = match (setting, service) {
        (None, Some(s)) => vec!["-M".to_string(), format!("{name}/{kind}={}", s.line)],
        (None, None) => vec!["-M".to_string(), "-X".to_string(), format!("{name}/{kind}")],
        (Some(setting), _) => match service.and_then(|s| s.setting(setting)) {
            Some(value) => vec!["-P".to_string(), format!("{name}/{kind}/{setting}={value}")],
            None => vec!["-P".to_string(), "-X".to_string(), format!("{name}/{kind}/{setting}")],
        },
    };
    Some(call)
}

/// The postconf calls that put main.cf parameters back: to the value main.cf
/// had for them (`explicit`, from `postconf -n`), or out of main.cf again.
fn undo_main_cf<'a>(
    names: impl Iterator<Item = &'a str>,
    explicit: &BTreeMap<String, String>,
) -> Vec<Vec<String>> {
    let mut set = vec!["-e".to_string()];
    let mut unset = vec!["-X".to_string()];
    for name in names {
        match explicit.get(name) {
            Some(value) => set.push(format!("{name} = {value}")),
            None => unset.push(name.to_string()),
        }
    }
    [set, unset]
        .into_iter()
        .filter(|call| call.len() > 1)
        .collect()
}

/// The postconf calls that take a plan back, last step first. They restore
/// the parameters and services the plan touches and nothing else. The whole
/// of main.cf and master.cf used to be written back from a copy, which also
/// undid what was changed in between by someone else: the certificate paths
/// ssl.rs sets after an order, or myhostname from Settings > Panel hostname.
/// postconf locks the file for each edit, so these calls and theirs cannot
/// lose one another's changes.
fn undo(plan: &Plan, now: &Postfix, explicit: &BTreeMap<String, String>) -> Vec<Vec<String>> {
    let mut calls: Vec<Vec<String>> = Vec::new();
    calls.extend(plan.open.iter().rev().filter_map(|args| undo_master(args, now)));
    calls.extend(undo_main_cf(plan.late.iter().map(|s| name_of(s)), explicit));
    calls.extend(plan.master.iter().rev().filter_map(|args| undo_master(args, now)));
    let first = plan
        .early
        .iter()
        .map(|s| name_of(s))
        .chain(plan.drop.iter().map(String::as_str));
    calls.extend(undo_main_cf(first, explicit));
    calls
}

async fn reload_postfix() {
    // Fails when Postfix is stopped; the files are right for its next start.
    if let Err(e) = exec::run("systemctl", ["reload", "postfix"]).await {
        tracing::warn!(error = %e, "postfix reload failed");
    }
}

async fn postconf_owned(args: &[String]) -> AgentResult<String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    postconf(&args).await
}

async fn edit_main_cf(settings: &[String]) -> AgentResult<()> {
    let mut args = vec!["-e"];
    args.extend(settings.iter().map(String::as_str));
    postconf(&args).await.map(|_| ())
}

async fn check_and_reload() -> AgentResult<()> {
    exec::run("postfix", ["check"]).await?;
    reload_postfix().await;
    Ok(())
}

async fn run_plan(plan: &Plan) -> AgentResult<()> {
    if !plan.drop.is_empty() {
        let mut args = vec!["-X"];
        args.extend(plan.drop.iter().map(String::as_str));
        postconf(&args).await?;
    }
    if !plan.early.is_empty() {
        edit_main_cf(&plan.early).await?;
    }
    for args in &plan.master {
        postconf_owned(args).await?;
    }
    if !plan.drop.is_empty() || !plan.early.is_empty() || !plan.master.is_empty() {
        check_and_reload().await?;
    }
    if !plan.late.is_empty() {
        edit_main_cf(&plan.late).await?;
        check_and_reload().await?;
    }
    if !plan.open.is_empty() {
        for args in &plan.open {
            postconf_owned(args).await?;
        }
        check_and_reload().await?;
    }
    Ok(())
}

/// What main.cf itself says about the parameters a plan touches (`postconf
/// -n`). One it does not mention is taken out again on the way back, not
/// set to its default.
async fn main_cf_values(steps: &Plan) -> AgentResult<BTreeMap<String, String>> {
    let names: Vec<&str> = steps
        .early
        .iter()
        .chain(steps.late.iter())
        .map(|s| name_of(s))
        .chain(steps.drop.iter().map(String::as_str))
        .collect();
    if names.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut args = vec!["-n"];
    args.extend(names);
    Ok(parse_postconf(&postconf(&args).await?))
}

/// Bring Postfix to `want`. Every step is checked with `postfix check`, and
/// Postfix is read back at the end; when a step fails or Postfix did not end
/// up as asked, the settings and services the plan touched are put back as
/// they were (`undo`). Returns whether anything had to change.
async fn converge(now: &Postfix, want: &Desired) -> AgentResult<bool> {
    let steps = plan(now, want);
    if steps.is_empty() {
        return Ok(false);
    }
    // Before anything is written: does this Postfix read the table the way
    // it is meant? A wrong answer here would misroute or defer mail.
    if matches!(want.ipv6, Ipv6Use::From(_)) {
        if let (Some(table), Some((domain, route))) =
            (build_table(&want.entries), want.entries.iter().next())
        {
            let key = format!("@{domain}");
            let got = exec::run("postmap", ["-q", key.as_str(), table.as_str()]).await?;
            if got.stdout.trim() != format!("{}:", route.service()) {
                return Err(AgentError::Other(anyhow::anyhow!(
                    "Postfix does not read the per-domain table as written; nothing was changed"
                )));
            }
        }
    }
    let back = undo(&steps, now, &main_cf_values(&steps).await?);

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
        tracing::warn!(error = %e, "postfix change failed; putting back the settings it touched");
        for args in &back {
            if let Err(e) = postconf_owned(args).await {
                tracing::warn!(error = %e, ?args, "could not put a postfix setting back");
            }
        }
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

/// The IPv4 address A and SPF records publish, and that mail should leave
/// from (`server_ip::resolve_ipv4` has the rule). When the panel hostname
/// could not be looked up, an address this module pinned Postfix to stays
/// for as long as it is on an interface: a failed lookup at boot must not
/// move outgoing mail to another address.
fn mail_ipv4(postfix: Option<&Postfix>, facts: &Ipv4Facts) -> Option<String> {
    if facts.named.is_empty() {
        if let Some(Bind4::Ours(Some(addr))) = postfix.map(Postfix::bind4) {
            if facts.on_interface(&addr) {
                return Some(addr);
            }
        }
    }
    facts.address()
}

pub(crate) fn is_private_v4(ip: &str) -> bool {
    ip.parse::<std::net::Ipv4Addr>()
        .map(|ip| server_ip::is_private_v4(&ip))
        .unwrap_or(false)
}

/// How the address IPv4 mail leaves from relates to the published one.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Ipv4Source {
    /// Nothing to do: the kernel sends from the published address by itself,
    /// or this server does not hold it (behind NAT), or it leaves through a
    /// private address (a gateway or VPN whose public address is not known).
    AsPublished,
    /// The kernel would send from `default`, another public address of this
    /// server, and Postfix can be told to use the published one.
    Pin { default: String },
    /// It would have to be told, but a connection from the published address
    /// might not get out; `why` says what is in the way.
    CannotPin { default: String, why: String },
    /// What the kernel would send from could not be read.
    Unknown,
}

/// `ipv4` is the published address, `leaves_by` the device packets from it
/// leave through (`server_ip::route_dev_from`), None when not looked up.
///
/// Pinning is safe only when those packets leave through the interface the
/// address is on. On a server with two uplinks and no routing rule for the
/// second address they would go out of the first, where the provider drops
/// them: every delivery would time out.
fn ipv4_source(ipv4: Option<&str>, facts: &Ipv4Facts, leaves_by: Option<&str>) -> Ipv4Source {
    let Some(addr) = ipv4 else {
        return Ipv4Source::AsPublished;
    };
    if !facts.on_interface(addr) || is_private_v4(addr) {
        return Ipv4Source::AsPublished;
    }
    let Some(default) = facts.default_source.clone() else {
        return Ipv4Source::Unknown;
    };
    if default == addr || is_private_v4(&default) {
        return Ipv4Source::AsPublished;
    }
    match (facts.device_of(addr), leaves_by) {
        (Some(own), Some(dev)) if own == dev => Ipv4Source::Pin { default },
        (Some(own), Some(dev)) => Ipv4Source::CannotPin {
            default,
            why: format!(
                "connections from {addr} would leave through {dev}, not through {own}, the interface the address is on"
            ),
        },
        _ => Ipv4Source::CannotPin {
            default,
            why: format!("the route connections from {addr} would take could not be read"),
        },
    }
}

/// The IPv4 pin Postfix should have.
fn pin4_wanted(postfix: &Postfix, addrs: &MailAddrs) -> Pin4 {
    match (&addrs.source4, &addrs.ipv4) {
        // Mail handed to a relay host is judged by the relay's address, and
        // the relay may only be reachable from the default one.
        (Ipv4Source::Pin { .. }, Some(addr)) if postfix.relayhost.is_empty() => {
            Pin4::To(addr.clone())
        }
        (Ipv4Source::Unknown, _) if addrs.pin4_gone().is_none() => Pin4::Keep,
        _ => Pin4::Off,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Trigger<'a> {
    /// The agent started: nobody asked for a change.
    Startup,
    /// The operator saved a choice.
    Save { force: bool },
    /// This mail domain was created just now.
    DomainAdded(&'a str),
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
            Trigger::DomainAdded(_) => false,
        }
}

/// May Postfix send over IPv6?
///
/// Never without an address. At agent start, what was on stays on, and what
/// was off comes on only when the check passes. On a save, a passing check
/// switches it on; without one it stays on only while IPv6 is somebody's
/// explicit choice (accepted earlier, after a passing check or with force),
/// and goes off when IPv6 would merely be the fallback. A new mail domain
/// changes nothing about it.
fn use_ipv6(candidate: bool, kept: bool, wants_ipv6: bool, trigger: Trigger, ready: bool) -> bool {
    candidate
        && match trigger {
            Trigger::Startup => kept || ready,
            Trigger::Save { force } => ready || (wants_ipv6 && (force || kept)),
            Trigger::DomainAdded(_) => kept,
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
    /// The IPv4 address A and SPF records publish.
    pub ipv4: Option<String>,
    /// The IPv6 address mail can leave from.
    pub ipv6: Option<String>,
    pub facts: Ipv6Facts,
    pub facts4: Ipv4Facts,
    /// Why `ipv6` is None, as a sentence.
    pub ipv6_missing: Option<String>,
    /// The server has an IPv4 address on an interface.
    pub has_ipv4: bool,
    /// Whether IPv4 mail leaves from `ipv4` by itself.
    pub source4: Ipv4Source,
    /// Who set the address Postfix sends IPv4 mail from.
    pub bind4: Bind4,
    /// The host Postfix hands all outgoing mail to; empty when it delivers
    /// the mail itself.
    pub relayhost: String,
    /// Postfix sends over IPv6, from `ipv6`.
    pub sends_ipv6: bool,
    /// The domains it sends over IPv4 only all the same, because their SPF
    /// record did not allow `ipv6`.
    pub held: Vec<String>,
}

impl MailAddrs {
    /// The address this module pinned IPv4 mail to, when it has left the
    /// server since. The pin is released at the next agent start or save;
    /// until then Postfix cannot bind to it, and where
    /// smtp_bind_address_enforce is on (it is while IPv6 is in use) IPv4
    /// deliveries are deferred.
    pub(crate) fn pin4_gone(&self) -> Option<&str> {
        match &self.bind4 {
            Bind4::Ours(Some(at))
                if !self.facts4.interfaces.is_empty() && !self.facts4.on_interface(at) =>
            {
                Some(at.as_str())
            }
            _ => None,
        }
    }
}

fn helo_name(postfix: Option<&Postfix>, facts: &Ipv6Facts) -> String {
    match postfix.map(|p| p.myhostname.as_str()).filter(|h| !h.is_empty()) {
        Some(h) => h.to_string(),
        None => facts.host.clone(),
    }
}

/// `leaves_by`: see `ipv4_source`.
fn addrs_from(
    postfix: Option<&Postfix>,
    facts: Ipv6Facts,
    facts4: Ipv4Facts,
    leaves_by: Option<&str>,
) -> MailAddrs {
    let helo = helo_name(postfix, &facts);
    let ipv6 = mail_ipv6(postfix, &facts);
    let ipv6_missing = match ipv6 {
        Some(_) => None,
        None => Some(ipv6_missing_reason(postfix, &facts, &helo)),
    };
    let ipv4 = mail_ipv4(postfix, &facts4);
    let source4 = ipv4_source(ipv4.as_deref(), &facts4, leaves_by);
    let sends_ipv6 = matches!(
        (postfix.map(Postfix::sending), &ipv6),
        (Some(Sending::Pinned(pinned)), Some(addr)) if pinned == *addr
    );
    let held = match postfix {
        Some(p) if sends_ipv6 => applied_holds(&p.sender_maps).into_iter().collect(),
        _ => Vec::new(),
    };
    MailAddrs {
        helo,
        ipv4,
        ipv6,
        facts,
        has_ipv4: !facts4.interfaces.is_empty(),
        facts4,
        ipv6_missing,
        source4,
        bind4: postfix.map(Postfix::bind4).unwrap_or(Bind4::Unset),
        relayhost: postfix.map(|p| p.relayhost.clone()).unwrap_or_default(),
        sends_ipv6,
        held,
    }
}

async fn gather_addrs(state: &AppState, postfix: Option<&Postfix>) -> MailAddrs {
    let facts = server_ip::ipv6_facts(&state.config.panel_host).await;
    let facts4 = server_ip::ipv4_facts(&state.config.panel_host).await;
    // Which way its connections would go is only asked for an address that
    // would have to be pinned.
    let ipv4 = mail_ipv4(postfix, &facts4);
    let leaves_by = match (ipv4_source(ipv4.as_deref(), &facts4, None), &ipv4) {
        (Ipv4Source::CannotPin { .. }, Some(addr)) => server_ip::route_dev_from(addr).await,
        _ => None,
    };
    addrs_from(postfix, facts, facts4, leaves_by.as_deref())
}

pub(crate) async fn mail_addrs(state: &AppState) -> MailAddrs {
    let postfix = read_postfix().await.ok();
    gather_addrs(state, postfix.as_ref()).await
}

async fn domain_prefs(state: &AppState) -> AgentResult<Vec<DomainPref>> {
    let mut conn = mail_conn(state).await?;
    let rows: Vec<(String, String, u8)> = conn
        .query("SELECT name, ip_preference, ip_forced FROM virtual_domains ORDER BY name")
        .await?;
    Ok(rows
        .into_iter()
        .map(|(name, pref, forced)| DomainPref {
            name,
            preference: Preference::from_db(&pref),
            forced: forced != 0,
        })
        .collect())
}

/// Look up the SPF records and decide which domains to hold to IPv4
/// (`held_domains`). `ipv6` is the address mail would leave from, `kept`
/// whether Postfix already sends over IPv6 from it.
async fn holds(
    postfix: &Postfix,
    domains: &[DomainPref],
    ipv6: &str,
    kept: bool,
    trigger: Trigger<'_>,
    dig: &Dig,
) -> BTreeSet<String> {
    let added = match trigger {
        Trigger::DomainAdded(name) => Some(name),
        _ => None,
    };
    let names = domains_to_look_up(domains, added);
    let spf = match ipv6.parse::<std::net::IpAddr>() {
        Ok(ip) if !names.is_empty() => mail_check::spf_sweep(dig, &names, ip).await,
        _ => Vec::new(),
    };
    held_domains(domains, &spf, kept, &applied_holds(&postfix.sender_maps), added)
}

/// Work out what Postfix should be set to and bring it there. `default` is
/// a new server-wide choice, `domains` every domain's choice as it will be
/// (None when the database cannot be read: the table then stays as it is),
/// `known_ready` the result of a live check the caller already ran. `dig`
/// keeps the answers of that check for the SPF lookups made here.
#[allow(clippy::too_many_arguments)]
async fn settle(
    state: &AppState,
    postfix: &Postfix,
    addrs: &MailAddrs,
    default: Option<Protocol>,
    domains: Option<&[DomainPref]>,
    trigger: Trigger<'_>,
    known_ready: Option<bool>,
    dig: &Dig,
) -> AgentResult<bool> {
    let default_after = default.unwrap_or_else(|| postfix.default_protocol());
    let applied = applied_entries(&postfix.sender_maps);
    let wants_ipv6 = default_after == Protocol::Ipv6
        || match domains {
            Some(domains) => domains.iter().any(|d| d.preference == Preference::Ipv6),
            None => applied.values().any(|route| *route == Route::Ipv6),
        };
    let candidate = addrs.ipv6.is_some();
    let kept = addrs.sends_ipv6;
    let ready = match known_ready {
        Some(ready) => ready,
        None if needs_check(candidate, kept, wants_ipv6, trigger) => {
            mail_check::ipv6_check(state, addrs, dig).await.ready
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
    let entries = match (domains, &ipv6) {
        (None, _) => applied,
        // A server without IPv4 has nothing to hold a domain to.
        (Some(domains), Ipv6Use::From(addr)) if addrs.has_ipv4 => {
            let held = holds(postfix, domains, addr, kept, trigger, dig).await;
            routes(domains, &held)
        }
        (Some(domains), _) => routes(domains, &BTreeSet::new()),
    };
    let want = Desired {
        preference: default,
        ipv6,
        entries,
        pin4: pin4_wanted(postfix, addrs),
    };
    converge(postfix, &want).await
}

/// Refuse IPv6 as a choice while it is not fit to send from.
async fn require_ipv6_ready(state: &AppState, addrs: &MailAddrs, dig: &Dig) -> AgentResult<()> {
    let check = mail_check::ipv6_check(state, addrs, dig).await;
    if check.ready {
        Ok(())
    } else {
        Err(AgentError::Conflict(mail_check::not_ready_message(&check)))
    }
}

/// Refuse IPv6 as one domain's choice while the domain's own SPF record does
/// not allow the address: it would be saved and the domain held to IPv4 all
/// the same. The refusal is what lets the operator choose it with force, and
/// a forced choice is not held.
async fn require_spf_allows(domain: &str, addrs: &MailAddrs, dig: &Dig) -> AgentResult<()> {
    let Some((v6, ip)) = addrs
        .ipv6
        .as_deref()
        .and_then(|v6| Some((v6, v6.parse::<std::net::IpAddr>().ok()?)))
    else {
        return Ok(());
    };
    if !addrs.has_ipv4 || !fit_for_main_cf(domain) {
        return Ok(());
    }
    match mail_check::spf_check(dig, domain, ip).await.result {
        Spf::Pass | Spf::NoRecord | Spf::Unknown(_) => Ok(()),
        _ => Err(AgentError::Conflict(format!(
            "IPv6 is not ready for {domain}: its SPF record does not allow mail from {v6}. \
             Run the DNS check of {domain} for the record to publish."
        ))),
    }
}

/// Does the table send this domain the way it was chosen? A domain held to
/// IPv4 counts as that, whatever was chosen, unless the choice was forced.
fn routed_as_chosen(domain: &DomainPref, applied: Option<Route>) -> bool {
    match (domain.preference.protocol(), applied) {
        (_, Some(Route::Ipv4Only)) => !domain.forced_ipv6(),
        (None, None) => true,
        (Some(chosen), Some(route)) => route == Route::first(chosen),
        _ => false,
    }
}

/// What to say about the address IPv4 mail leaves from, when it is not
/// simply the published one.
fn ipv4_notes(addrs: &MailAddrs) -> Vec<String> {
    let mut notes = Vec::new();
    // Receivers judge mail that goes through a relay host by the relay's address.
    let (Some(v4), true) = (addrs.ipv4.as_deref(), addrs.relayhost.is_empty()) else {
        return notes;
    };
    if let Bind4::ByHand(set) = &addrs.bind4 {
        // A private address set by hand leaves through NAT: nothing to say.
        if set != v4 && !is_private_v4(set) {
            notes.push(format!(
                "Postfix is set by hand to send IPv4 mail from {set} (smtp_bind_address in main.cf), but A and SPF records publish {v4}, so receivers see that mail fail SPF. \
                 The panel does not change a setting made by hand. Point the panel hostname at {set}, or remove the setting (postconf -X smtp_bind_address) and save here again."
            ));
        }
        return notes;
    }
    if let Some(gone) = addrs.pin4_gone() {
        notes.push(format!(
            "Postfix is still set to send IPv4 mail from {gone}, which is no longer an address of this server. \
             Until that is released, mail sent over IPv4 can stay in the queue. Save this setting to release it."
        ));
        return notes;
    }
    match &addrs.source4 {
        Ipv4Source::Pin { default } => notes.push(match &addrs.bind4 {
            Bind4::Ours(Some(at)) if at == v4 => format!(
                "This server has more than one public IPv4 address and would send from {default} by itself. Postfix is set to send from {v4}, the address A and SPF records publish."
            ),
            _ => format!(
                "This server would send IPv4 mail from {default}, but A and SPF records publish {v4}. Save this setting to make Postfix send from {v4}."
            ),
        }),
        Ipv4Source::CannotPin { default, why } => notes.push(format!(
            "This server sends IPv4 mail from {default}, but A and SPF records publish {v4}, so receivers see that mail fail SPF. \
             Postfix was not set to send from {v4}: {why}. Point the panel hostname at {default}, then run Reconcile on each domain."
        )),
        Ipv4Source::AsPublished | Ipv4Source::Unknown => {}
    }
    notes
}

fn notes(postfix: &Postfix, addrs: &MailAddrs, domains: &[DomainPref]) -> Vec<String> {
    let mut notes = Vec::new();
    let default = postfix.default_protocol();
    let wants_ipv6 = default == Protocol::Ipv6
        || domains.iter().any(|d| d.preference == Preference::Ipv6);
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
            let mut stale: Vec<String> = domains
                .iter()
                .filter(|d| fit_for_main_cf(&d.name))
                .filter(|d| !routed_as_chosen(d, applied.get(&d.name).copied()))
                .map(|d| d.name.clone())
                .collect();
            stale.sort();
            if !stale.is_empty() {
                notes.push(format!(
                    "Postfix does not route {} as chosen yet. Save the choice again.",
                    stale.join(", ")
                ));
            }
            let held: Vec<String> = domains
                .iter()
                .filter(|d| applied.get(&d.name) == Some(&Route::Ipv4Only) && !d.forced_ipv6())
                .map(|d| d.name.clone())
                .collect();
            if !held.is_empty() {
                let (its, that) = if held.len() == 1 {
                    ("its", "that domain")
                } else {
                    ("their", "those domains")
                };
                notes.push(format!(
                    "Mail from {} goes out over IPv4 only: when Postfix was last set up, {its} SPF record did not allow mail from {a}, or could not be looked up. \
                     Run the DNS check of {that} for the record to publish, then save this setting again. A domain set to IPv6 with \"Use IPv6 anyway\" is not held back.",
                    mail_check::join_names(&held)
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
    notes.extend(ipv4_notes(addrs));
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

fn build_settings(postfix: &Postfix, addrs: &MailAddrs, prefs: &[DomainPref]) -> MailIpSettings {
    let default_protocol = postfix.default_protocol();
    let sending = postfix.sending();
    let applied = applied_entries(&postfix.sender_maps);
    let mut domains: Vec<MailIpDomain> = prefs
        .iter()
        .map(|d| MailIpDomain {
            name: d.name.clone(),
            preference: d.preference,
            effective: match sending {
                Sending::Ipv4Only => Protocol::Ipv4,
                _ => applied
                    .get(&d.name)
                    .map(|route| route.protocol())
                    .unwrap_or(default_protocol),
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
    let dig = Dig::default();
    let known_ready = if protocol == Protocol::Ipv6 && !force {
        require_ipv6_ready(&state, &addrs, &dig).await?;
        Some(true)
    } else {
        None
    };
    settle(
        &state,
        &postfix,
        &addrs,
        Some(protocol),
        Some(&prefs),
        Trigger::Save { force },
        known_ready,
        &dig,
    )
    .await?;
    current_settings(&state).await.map(Json)
}

/// PUT /mail/domains/{name}/ip-preference  { preference: "default" | "ipv4" | "ipv6", force?: bool }
pub async fn put_domain_preference(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> AgentResult<Json<MailIpDomain>> {
    const STORE: &str = "UPDATE virtual_domains SET ip_preference = ?, ip_forced = ? WHERE name = ?";
    let name = name.trim().trim_end_matches('.').to_lowercase();
    let preference = body
        .get("preference")
        .and_then(|v| v.as_str())
        .and_then(Preference::parse)
        .ok_or_else(|| AgentError::Invalid("preference must be default, ipv4 or ipv6".into()))?;
    let force = force_flag(&body);
    // Only an IPv6 choice can be forced; it then stands whatever the
    // domain's SPF record says.
    let forced = force && preference == Preference::Ipv6;

    let _guard = APPLY_LOCK.lock().await;
    let mut conn = mail_conn(&state).await?;
    let previous: Option<(String, u8)> = conn
        .exec_first(
            "SELECT ip_preference, ip_forced FROM virtual_domains WHERE name = ?",
            (&name,),
        )
        .await?;
    let Some(previous) = previous else {
        return Err(AgentError::NotFound);
    };
    // Nothing is stored on a server where Postfix cannot be asked.
    let postfix = read_postfix().await?;
    let mut prefs = domain_prefs(&state).await?;
    let addrs = gather_addrs(&state, Some(&postfix)).await;
    let dig = Dig::default();
    let known_ready = if preference == Preference::Ipv6 && !force {
        require_ipv6_ready(&state, &addrs, &dig).await?;
        require_spf_allows(&name, &addrs, &dig).await?;
        Some(true)
    } else {
        None
    };
    for domain in prefs.iter_mut().filter(|d| d.name == name) {
        domain.preference = preference;
        domain.forced = forced;
    }
    conn.exec_drop(STORE, (preference.as_str(), u8::from(forced), &name))
        .await?;
    let settled = settle(
        &state,
        &postfix,
        &addrs,
        None,
        Some(&prefs),
        Trigger::Save { force },
        known_ready,
        &dig,
    )
    .await;
    if let Err(e) = settled {
        // Postfix kept its old routing, so the stored choice goes back too.
        let _ = conn.exec_drop(STORE, (&previous.0, previous.1, &name)).await;
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

/// A mail domain was created. While Postfix sends over IPv6, its SPF record
/// is looked up right away and the table rebuilt, so that a domain whose
/// record does not allow the IPv6 address sends over IPv4 from its first
/// mail on. Returns a sentence for the operator when the domain is held to
/// IPv4. Never an error: the domain exists either way, and the next save
/// or agent start builds the table again.
pub(crate) async fn domain_added(state: &AppState, name: &str) -> Option<String> {
    let _guard = APPLY_LOCK.lock().await;
    let postfix = read_postfix().await.ok()?;
    // Without IPv6 every domain sends over IPv4 anyway.
    if !matches!(postfix.sending(), Sending::Pinned(_)) {
        return None;
    }
    let domains = domain_prefs(state).await.ok()?;
    let addrs = gather_addrs(state, Some(&postfix)).await;
    let v6 = addrs.ipv6.clone().filter(|_| addrs.sends_ipv6)?;
    let dig = Dig::default();
    let settled = settle(
        state,
        &postfix,
        &addrs,
        None,
        Some(&domains),
        Trigger::DomainAdded(name),
        None,
        &dig,
    )
    .await;
    if let Err(e) = settled {
        tracing::warn!(domain = %name, error = %e, "mail routing not rebuilt for the new mail domain");
        return None;
    }
    let after = read_postfix().await.ok()?;
    applied_holds(&after.sender_maps).contains(name).then(|| {
        format!(
            "Mail from {name} goes out over IPv4 only for now: its SPF record does not allow mail from {v6} yet, or could not be looked up. \
             Once it does (Mailboxes > the domain's DNS check shows the record), save the outgoing mail setting again under Settings > Mail."
        )
    })
}

/// Runs at every agent start, so every server gets here with its next
/// update, without install.sh. Brings Postfix in line with the rules at the
/// top of this file: a server whose Postfix could fall back to an IPv6
/// address nothing vouches for is limited to IPv4, one that is fit for IPv6
/// is pinned to its address, the per-domain table is rebuilt from the
/// database and the SPF records as they are now, and the IPv4 address is
/// pinned or released. The operator's choices are not touched. No-op when
/// Postfix is already as it should be, or is not installed.
pub async fn ensure_at_startup(state: AppState) {
    if read_postfix().await.is_err() {
        return;
    }
    // MariaDB may still be starting.
    let mut prefs: Option<Vec<DomainPref>> = None;
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
    let addrs = gather_addrs(&state, Some(&postfix)).await;
    let dig = Dig::default();
    let settled = settle(
        &state,
        &postfix,
        &addrs,
        None,
        prefs.as_deref(),
        Trigger::Startup,
        None,
        &dig,
    )
    .await;
    match settled {
        Ok(false) => {}
        Ok(true) => {
            let after = read_postfix().await.ok();
            match after.as_ref().map(Postfix::sending) {
                Some(Sending::Pinned(addr)) => {
                    tracing::info!(%addr, "postfix: outgoing mail may use IPv6, from this address only")
                }
                _ => tracing::info!("postfix: outgoing mail limited to IPv4"),
            }
            if let Some(Bind4::Ours(Some(addr))) = after.as_ref().map(Postfix::bind4) {
                tracing::info!(%addr, "postfix: IPv4 mail is sent from this address, the one DNS records publish");
            }
        }
        Err(e) => tracing::warn!(error = %e, "could not bring postfix in line with the mail IP settings"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const V4: &str = "198.51.100.127";
    /// A second public address of the same server.
    const V4B: &str = "203.0.113.9";
    const V6: &str = "2001:db8:1::1";

    /// master.cf as Ubuntu ships it, plus the installer's submission
    /// service, the way `postconf -M` prints them.
    const MASTER: &str = "\
smtp       inet  n       -       y       -       -       smtpd
pickup     unix  n       -       y       60      1       pickup
qmgr       unix  n       -       n       300     1       qmgr
smtp       unix  -       -       y       -       -       smtp
relay      unix  -       -       y       -       -       smtp -o syslog_name=postfix/$service_name
lmtp       unix  -       -       y       -       -       lmtp
maildrop   unix  -       n       n       -       -       pipe flags=DRXhu user=vmail argv=/usr/bin/maildrop -d ${recipient}
submission inet  n       -       y       -       -       smtpd -o syslog_name=postfix/submission -o smtpd_tls_security_level=encrypt -o {smtpd_relay_restrictions=permit_sasl_authenticated, reject}
";

    fn main_cf(preference: &str, bind6: &str, maps: &str) -> String {
        format!(
            "smtp_address_preference = {preference}\ninet_protocols = all\nsmtp_bind_address6 = {bind6}\n\
             smtp_bind_address =\nsender_dependent_default_transport_maps = {maps}\nrelayhost = \n\
             config_directory = /etc/postfix\n"
        )
    }

    /// A server as install.sh left it before this module: both protocols,
    /// IPv4 first, IPv6 from any address.
    fn legacy() -> Postfix {
        postfix_from(&main_cf("ipv4", "", ""), "panel.example.com\n", Some("no\n"), MASTER)
    }

    /// `p` with the IPv4 address pinned by this module.
    fn pinned4(mut p: Postfix, addr: &str) -> Postfix {
        p.bind4_raw = format!("${BIND4_PIN}");
        p.bind4_pin = Some(addr.to_string());
        p
    }

    /// `p` with smtp_bind_address set by hand.
    fn by_hand(mut p: Postfix, value: &str) -> Postfix {
        p.bind4_raw = value.to_string();
        p
    }

    /// Postfix that sends over IPv6 from V6, with this table.
    fn sending_v6(preference: &str, maps: &str) -> Postfix {
        postfix_from(&main_cf(preference, V6, maps), "panel.example.com", Some("yes"), MASTER)
    }

    fn set_main(p: &mut Postfix, setting: &str) {
        let (k, v) = setting.split_once('=').unwrap();
        let (k, v) = (k.trim(), v.trim());
        match k {
            "smtp_address_preference" => p.preference = v.to_string(),
            "smtp_bind_address6" => p.bind6 = server_ip::parse_ipv6(v),
            "smtp_bind_address_enforce" => p.bind_enforce = Some(v.to_string()),
            SENDER_MAPS => p.sender_maps = v.to_string(),
            BIND4 => p.bind4_raw = v.to_string(),
            BIND4_PIN => p.bind4_pin = server_ip::parse_ipv4(v),
            other => panic!("unexpected main.cf setting {other}"),
        }
    }

    fn unset_main(p: &mut Postfix, name: &str) {
        match name {
            "smtp_bind_address6" => p.bind6 = None,
            "smtp_bind_address_enforce" => p.bind_enforce = Some("no".to_string()),
            SENDER_MAPS => p.sender_maps.clear(),
            BIND4 => p.bind4_raw.clear(),
            BIND4_PIN => {
                p.bind4_pin = None;
                p.pin_left_over = false;
            }
            other => panic!("unexpected main.cf parameter {other}"),
        }
    }

    fn edit_master(p: &mut Postfix, args: &[String]) {
        fn target<'a>(p: &'a mut Postfix, path: &str) -> (&'a mut Service, String) {
            let mut it = path.split('/');
            let (name, kind, param) = (it.next().unwrap(), it.next().unwrap(), it.next().unwrap());
            let s = p.services.iter_mut().find(|s| s.name == name && s.kind == kind).unwrap();
            (s, param.to_string())
        }
        match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
            ["-M", "-X", what] => {
                let (name, kind) = what.split_once('/').unwrap();
                p.services.retain(|s| !(s.name == name && s.kind == kind));
            }
            ["-M", entry] => {
                let (_, line) = entry.split_once('=').unwrap();
                let new = parse_master(line).remove(0);
                p.services.retain(|s| !(s.name == new.name && s.kind == new.kind));
                p.services.push(new);
            }
            ["-P", "-X", what] => {
                let (s, param) = target(p, what);
                s.overrides.retain(|(k, _)| *k != param);
            }
            ["-P", what] => {
                let (path, value) = what.split_once('=').unwrap();
                let (s, param) = target(p, path);
                s.overrides.retain(|(k, _)| *k != param);
                s.overrides.push((param, value.to_string()));
            }
            other => panic!("unexpected postconf call {other:?}"),
        }
    }

    /// Apply a plan the way postconf would, for the transitions below.
    fn after(now: &Postfix, plan: &Plan) -> Postfix {
        let mut p = now.clone();
        plan.drop.iter().for_each(|name| unset_main(&mut p, name));
        plan.early.iter().for_each(|s| set_main(&mut p, s));
        plan.master.iter().for_each(|args| edit_master(&mut p, args));
        plan.late.iter().for_each(|s| set_main(&mut p, s));
        plan.open.iter().for_each(|args| edit_master(&mut p, args));
        p
    }

    /// Apply the calls of an `undo`, in their order.
    fn undone(mut p: Postfix, calls: &[Vec<String>]) -> Postfix {
        for call in calls {
            match call[0].as_str() {
                "-e" => call[1..].iter().for_each(|s| set_main(&mut p, s)),
                "-X" => call[1..].iter().for_each(|name| unset_main(&mut p, name)),
                _ => edit_master(&mut p, call),
            }
        }
        p
    }

    /// A service's name, type and settings.
    type ServiceShape = (String, String, Vec<(String, String)>);

    /// Everything about Postfix but the order of its services and the
    /// spelling of their lines.
    fn shape(p: &Postfix) -> (Postfix, Vec<ServiceShape>) {
        let mut services: Vec<_> = p
            .services
            .iter()
            .map(|s| {
                let mut o = s.overrides.clone();
                o.sort();
                (s.name.clone(), s.kind.clone(), o)
            })
            .collect();
        services.sort();
        let mut rest = p.clone();
        rest.services.clear();
        (rest, services)
    }

    /// What `postconf -n` says about these parameters of `main_cf()`.
    fn explicit(p: &Postfix, names: &[&str]) -> BTreeMap<String, String> {
        names
            .iter()
            .filter_map(|name| {
                let value = match *name {
                    "smtp_address_preference" => p.preference.clone(),
                    "smtp_bind_address6" => p.bind6.clone().unwrap_or_default(),
                    SENDER_MAPS => p.sender_maps.clone(),
                    BIND4 if !p.bind4_raw.is_empty() => p.bind4_raw.clone(),
                    BIND4_PIN if p.bind4_pin.is_some() => p.bind4_pin.clone().unwrap(),
                    // Not in main.cf: smtp_bind_address_enforce, an unset pin.
                    _ => return None,
                };
                Some((name.to_string(), value))
            })
            .collect()
    }

    fn strs(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn entries(list: &[(&str, Route)]) -> BTreeMap<String, Route> {
        list.iter().map(|(d, r)| (d.to_string(), *r)).collect()
    }

    fn want(preference: Option<Protocol>, ipv6: Ipv6Use, entries: BTreeMap<String, Route>) -> Desired {
        Desired { preference, ipv6, entries, pin4: Pin4::Off }
    }

    fn off() -> Desired {
        want(None, Ipv6Use::Off, BTreeMap::new())
    }

    fn pref(name: &str, preference: Preference) -> DomainPref {
        DomainPref { name: name.to_string(), preference, forced: false }
    }

    fn forced(name: &str) -> DomainPref {
        DomainPref { name: name.to_string(), preference: Preference::Ipv6, forced: true }
    }

    fn names(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn facts(published: Option<&str>, on: &[&str]) -> Ipv6Facts {
        Ipv6Facts {
            host: "panel.example.com".into(),
            published: published.map(str::to_string),
            on_interfaces: on.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// The first interface address is the one connections leave from.
    fn facts4(named: &[&str], interfaces: &[(&str, &str)]) -> Ipv4Facts {
        Ipv4Facts {
            host: "panel.example.com".into(),
            named: strs(named),
            interfaces: interfaces
                .iter()
                .map(|(dev, ip)| (dev.to_string(), ip.to_string()))
                .collect(),
            default_source: interfaces.first().map(|(_, ip)| ip.to_string()),
        }
    }

    /// The usual server: one public IPv4 address, the panel hostname on it.
    fn one_v4() -> Ipv4Facts {
        facts4(&[V4], &[("eth0", V4)])
    }

    /// Two public addresses on one interface; the panel hostname is on the
    /// second, connections leave from the first.
    fn two_v4() -> Ipv4Facts {
        facts4(&[V4B], &[("eth0", V4), ("eth0", V4B)])
    }

    fn addrs(postfix: &Postfix, f: Ipv6Facts) -> MailAddrs {
        addrs_from(Some(postfix), f, one_v4(), None)
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
        assert_eq!(p.bind4(), Bind4::Unset);

        let smtp = p.service("smtp").unwrap();
        assert_eq!((smtp.kind.as_str(), smtp.chroot.as_str(), smtp.command.as_str()), ("unix", "y", "smtp"));
        assert!(smtp.overrides.is_empty());
        assert_eq!(p.service("relay").unwrap().setting("syslog_name"), Some("postfix/$service_name"));
        // The whole line is kept, to put a service back as it was.
        assert_eq!(
            p.service("relay").unwrap().line,
            "relay      unix  -       -       y       -       -       smtp -o syslog_name=postfix/$service_name"
        );
        let submission = p.services.iter().find(|s| s.name == "submission").unwrap();
        assert_eq!(submission.setting("smtpd_tls_security_level"), Some("encrypt"));
        assert_eq!(
            submission.setting("smtpd_relay_restrictions"),
            Some("permit_sasl_authenticated, reject")
        );
        assert!(p.service("smtp-ipv6").is_none());

        // A setting with spaces as master.cf may spell it, and one without
        // a space after -o.
        let written = parse_master(
            "smtp unix - - y - - smtp -o { smtp_helo_name = a b } -oinet_protocols=ipv4 -o smtp_address_preference=ipv6\n",
        );
        assert_eq!(written[0].setting("smtp_helo_name"), Some("a b"));
        assert_eq!(written[0].setting("inet_protocols"), Some("ipv4"));
        assert_eq!(written[0].setting("smtp_address_preference"), Some("ipv6"));
    }

    #[test]
    fn the_ipv4_address_postfix_sends_from_is_this_modules_or_the_operators() {
        let read = |bind4: &str, pin: &str| {
            let main = format!(
                "{}smtp_bind_address = {bind4}\n{pin}",
                main_cf("ipv4", "", "")
            );
            postfix_from(&main, "panel.example.com", Some("no"), MASTER)
        };
        assert_eq!(read("", "").bind4(), Bind4::Unset);
        // Written by this module: the address is in our own parameter.
        let ours = read("$vps_panel_smtp_bind_address", "vps_panel_smtp_bind_address = 203.0.113.9\n");
        assert_eq!(ours.bind4(), Bind4::Ours(Some(V4B.into())));
        assert_eq!(read("${vps_panel_smtp_bind_address}", "vps_panel_smtp_bind_address = 203.0.113.9\n").bind4(), ours.bind4());
        // Our parameter was deleted: Postfix reads the reference as empty.
        assert_eq!(read("$vps_panel_smtp_bind_address", "").bind4(), Bind4::Ours(None));
        // Anything else is the operator's.
        assert_eq!(read("203.0.113.9", "").bind4(), Bind4::ByHand(V4B.into()));
        assert_eq!(read("[203.0.113.9]", "").bind4(), Bind4::ByHand(V4B.into()));
        assert_eq!(read("$some_other_parameter", "").bind4(), Bind4::ByHand("$some_other_parameter".into()));
        // postconf does not print our parameter once nothing refers to it;
        // a value that slipped through does not make the address ours.
        let theirs = read("203.0.113.9", "vps_panel_smtp_bind_address = 198.51.100.127\n");
        assert_eq!(theirs.bind4(), Bind4::ByHand(V4B.into()));
        assert_eq!(theirs.bind4_pin, None);

        let main_cf_text = "# vps_panel_smtp_bind_address = 1.2.3.4\nmyhostname = a\n\
                            smtp_bind_address = 203.0.113.9\n  vps_panel_smtp_bind_address = x\n";
        assert!(!main_cf_defines(main_cf_text, BIND4_PIN));
        assert!(main_cf_defines(main_cf_text, "smtp_bind_address"));
        assert!(main_cf_defines("vps_panel_smtp_bind_address=203.0.113.9\n", BIND4_PIN));
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
        let e = entries(&[
            ("b.example", Route::Ipv4),
            ("a.example", Route::Ipv6),
            ("c.example", Route::Ipv4Only),
        ]);
        let table = build_table(&e).unwrap();
        assert_eq!(
            table,
            "inline:{@a.example=smtp-ipv6:,@b.example=smtp-ipv4:,@c.example=smtp-ipv4only:}"
        );
        assert_eq!(parse_table(&table), Some(e.clone()));
        assert_eq!(build_table(&BTreeMap::new()), None);
        assert_eq!(applied_holds(&table), names(&["c.example"]));
        // What each entry stands for.
        assert_eq!(Route::Ipv4Only.protocol(), Protocol::Ipv4);
        assert_eq!(Route::Ipv4.protocol(), Protocol::Ipv4);
        assert_eq!(Route::Ipv6.protocol(), Protocol::Ipv6);

        // Somebody else's tables are not ours to read or drop.
        assert_eq!(parse_table("hash:/etc/postfix/sender_transport"), None);
        assert_eq!(parse_table("inline:{@x.example=relay:[smtp.example.net]:587}"), None);
        assert_eq!(parse_table("inline:{ { @x.example = smtp-ipv6: } }"), None);
        assert_eq!(parse_table("inline:{@x.example=smtp-ipv4only-old:}"), None);

        let theirs = "hash:/etc/postfix/sender_transport, inline:{ {@x.example = relay:[mx.example.net]} }";
        assert_eq!(
            split_tables(theirs),
            vec!["hash:/etc/postfix/sender_transport", "inline:{ {@x.example = relay:[mx.example.net]} }"]
        );
        let merged = sender_maps_value(theirs, &e);
        assert_eq!(merged, format!("{theirs}, {table}"));
        assert_eq!(applied_entries(&merged), e);
        // Replacing ours keeps theirs; removing ours leaves only theirs.
        let one = entries(&[("a.example", Route::Ipv4)]);
        assert_eq!(
            sender_maps_value(&merged, &one),
            format!("{theirs}, inline:{{@a.example=smtp-ipv4:}}")
        );
        assert_eq!(sender_maps_value(&merged, &BTreeMap::new()), theirs);
        assert_eq!(sender_maps_value("", &BTreeMap::new()), "");
    }

    #[test]
    fn only_plain_domain_names_reach_main_cf() {
        let domains = vec![
            pref("example.com", Preference::Ipv6),
            pref("follows.example", Preference::Default),
            pref("bad name}, x", Preference::Ipv6),
            pref("UPPER.example", Preference::Ipv4),
        ];
        assert_eq!(
            routes(&domains, &BTreeSet::new()),
            entries(&[("example.com", Route::Ipv6)])
        );
        // Not as a held domain either, and it is never looked up.
        let all_held = names(&["example.com", "follows.example", "bad name}, x", "UPPER.example"]);
        assert_eq!(
            routes(&domains, &all_held),
            entries(&[("example.com", Route::Ipv4Only), ("follows.example", Route::Ipv4Only)])
        );
        assert_eq!(domains_to_look_up(&domains, None), strs(&["example.com", "follows.example"]));
        assert_eq!(Preference::from_db(" ipv6 "), Preference::Ipv6);
        assert_eq!(Preference::from_db("nonsense"), Preference::Default);
    }

    #[test]
    fn a_domain_is_held_to_ipv4_while_its_spf_record_does_not_allow_the_ipv6_address() {
        let domains = vec![
            pref("covers.example", Preference::Default),
            pref("lacks.example", Preference::Default),
            pref("soft.example", Preference::Ipv6),
            pref("none.example", Preference::Ipv4),
            pref("broken.example", Preference::Default),
        ];
        let spf = vec![
            ("covers.example".to_string(), Spf::Pass),
            ("lacks.example".to_string(), Spf::Fail),
            ("soft.example".to_string(), Spf::SoftFail),
            // No SPF record at all: IPv4 would be no better off.
            ("none.example".to_string(), Spf::NoRecord),
            ("broken.example".to_string(), Spf::Broken("it needs more than 10 DNS lookups".into())),
        ];
        let held = held_domains(&domains, &spf, true, &BTreeSet::new(), None);
        assert_eq!(held, names(&["lacks.example", "soft.example", "broken.example"]));
        // Held whatever was chosen for the domain; the others as chosen.
        assert_eq!(
            routes(&domains, &held),
            entries(&[
                ("lacks.example", Route::Ipv4Only),
                ("soft.example", Route::Ipv4Only),
                ("none.example", Route::Ipv4),
                ("broken.example", Route::Ipv4Only),
            ])
        );
        // The record was fixed: the next build of the table lets the domain go.
        let fixed = vec![("lacks.example".to_string(), Spf::Pass)];
        let one = vec![pref("lacks.example", Preference::Default)];
        assert!(held_domains(&one, &fixed, true, &names(&["lacks.example"]), None).is_empty());
        assert!(routes(&one, &BTreeSet::new()).is_empty());
    }

    #[test]
    fn a_failed_spf_lookup_changes_nothing_that_is_known_and_holds_what_is_new() {
        let domains = vec![
            pref("free.example", Preference::Default),
            pref("held.example", Preference::Default),
            pref("slow.example", Preference::Ipv6),
        ];
        let spf = vec![
            ("free.example".to_string(), Spf::Unknown("no resolver".into())),
            ("held.example".to_string(), Spf::Unknown("no resolver".into())),
            // slow.example was not reached before the time ran out.
        ];
        let held_now = names(&["held.example"]);
        // Postfix already sends over IPv6: every domain stays as the table has it.
        assert_eq!(held_domains(&domains, &spf, true, &held_now, None), held_now);
        // IPv6 is only now being switched on: nothing is known, all are held.
        assert_eq!(
            held_domains(&domains, &spf, false, &BTreeSet::new(), None),
            names(&["free.example", "held.example", "slow.example"])
        );
        // A domain created just now is held until its record was seen...
        assert_eq!(
            held_domains(&domains, &[], true, &held_now, Some("slow.example")),
            names(&["held.example", "slow.example"])
        );
        // ...and only that one is looked up then; the others keep their state.
        assert_eq!(domains_to_look_up(&domains, Some("slow.example")), strs(&["slow.example"]));
        let seen = vec![("slow.example".to_string(), Spf::Pass)];
        assert_eq!(held_domains(&domains, &seen, true, &held_now, Some("slow.example")), held_now);
        let seen = vec![("slow.example".to_string(), Spf::Fail)];
        assert_eq!(
            held_domains(&domains, &seen, true, &BTreeSet::new(), Some("slow.example")),
            names(&["slow.example"])
        );
    }

    #[test]
    fn a_domain_set_to_ipv6_with_force_is_never_held() {
        let domains = vec![forced("mine.example"), pref("other.example", Preference::Ipv6)];
        let spf = vec![
            ("mine.example".to_string(), Spf::Fail),
            ("other.example".to_string(), Spf::Fail),
        ];
        // Its record is not even looked up.
        assert_eq!(domains_to_look_up(&domains, None), strs(&["other.example"]));
        assert_eq!(domains_to_look_up(&domains, Some("mine.example")), Vec::<String>::new());
        let held = held_domains(&domains, &spf, false, &names(&["mine.example"]), Some("mine.example"));
        assert_eq!(held, names(&["other.example"]));
        assert_eq!(
            routes(&domains, &names(&["mine.example", "other.example"])),
            entries(&[("mine.example", Route::Ipv6), ("other.example", Route::Ipv4Only)])
        );
        // Force means something for an IPv6 choice only.
        let mut v4 = forced("v4.example");
        v4.preference = Preference::Ipv4;
        assert!(!v4.forced_ipv6());
        assert_eq!(
            routes(&[v4], &names(&["v4.example"])),
            entries(&[("v4.example", Route::Ipv4Only)])
        );
    }

    #[test]
    fn a_server_without_a_published_ipv6_is_limited_to_ipv4() {
        // What the agent does at its first start on an existing install.
        let now = legacy();
        let steps = plan(&now, &off());
        assert_eq!(
            steps,
            Plan {
                master: vec![
                    strs(&["-P", "smtp/unix/inet_protocols=ipv4"]),
                    strs(&["-P", "relay/unix/inet_protocols=ipv4"]),
                ],
                ..Plan::default()
            }
        );
        let then = after(&now, &steps);
        assert_eq!(then.sending(), Sending::Ipv4Only);
        // The operator's protocol choice and smtpd's protocols are untouched.
        assert_eq!(then.preference, "ipv4");
        assert_eq!(then.inet_protocols, "all");
        // And the next start changes nothing.
        assert!(plan(&then, &off()).is_empty());
    }

    #[test]
    fn switching_ipv6_on_pins_it_and_sets_the_table_before_it_opens() {
        let closed = after(&legacy(), &plan(&legacy(), &off()));
        let wanted = want(
            Some(Protocol::Ipv6),
            Ipv6Use::From(V6.into()),
            entries(&[("shop.example", Route::Ipv4), ("old.example", Route::Ipv4Only)]),
        );
        let steps = plan(&closed, &wanted);
        assert!(steps.drop.is_empty());
        // The address and the preference go into main.cf first...
        assert_eq!(
            steps.early,
            vec![
                "smtp_address_preference = ipv6".to_string(),
                format!("smtp_bind_address6 = {V6}"),
                "smtp_bind_address_enforce = yes".to_string(),
            ]
        );
        // ...then master.cf gets the services the table names...
        assert_eq!(
            steps.master,
            vec![
                strs(&["-M", "smtp-ipv4/unix=smtp-ipv4 unix - - y - - smtp -o smtp_address_preference=ipv4"]),
                strs(&["-M", "smtp-ipv4only/unix=smtp-ipv4only unix - - y - - smtp -o inet_protocols=ipv4"]),
            ]
        );
        // ...then main.cf the table, which keeps old.example on IPv4...
        assert_eq!(
            steps.late,
            vec!["sender_dependent_default_transport_maps = inline:{@old.example=smtp-ipv4only:,@shop.example=smtp-ipv4:}".to_string()]
        );
        // ...and only then may the stock services send over IPv6.
        assert_eq!(
            steps.open,
            vec![
                strs(&["-P", "-X", "smtp/unix/inet_protocols"]),
                strs(&["-P", "-X", "relay/unix/inet_protocols"]),
            ]
        );
        let on = after(&closed, &steps);
        assert_eq!(on.sending(), Sending::Pinned(V6.into()));
        assert_eq!(on.service("smtp-ipv4").unwrap().setting("smtp_address_preference"), Some("ipv4"));
        assert_eq!(on.service("smtp-ipv4only").unwrap().setting("inet_protocols"), Some("ipv4"));
        // relay keeps the setting that was not ours.
        assert_eq!(on.service("relay").unwrap().setting("syslog_name"), Some("postfix/$service_name"));
        assert!(plan(&on, &wanted).is_empty());
    }

    #[test]
    fn switching_ipv6_off_closes_it_before_the_table_and_the_address_go() {
        let on = postfix_from(
            &main_cf("ipv6", V6, "inline:{@old.example=smtp-ipv4only:,@shop.example=smtp-ipv4:}"),
            "panel.example.com",
            Some("yes"),
            &format!(
                "{MASTER}smtp-ipv4  unix  -  -  y  -  -  smtp -o smtp_address_preference=ipv4\n\
                 smtp-ipv4only  unix  -  -  y  -  -  smtp -o inet_protocols=ipv4\n"
            ),
        );
        let wanted = want(
            None,
            Ipv6Use::Off,
            entries(&[("shop.example", Route::Ipv4), ("old.example", Route::Ipv4Only)]),
        );
        let steps = plan(&on, &wanted);
        // The stock services first: while they can still send over IPv6,
        // the table is what keeps old.example off it.
        assert!(steps.drop.is_empty() && steps.early.is_empty() && steps.open.is_empty());
        assert_eq!(
            steps.master,
            vec![
                strs(&["-P", "smtp/unix/inet_protocols=ipv4"]),
                strs(&["-P", "relay/unix/inet_protocols=ipv4"]),
            ]
        );
        assert_eq!(
            steps.late,
            vec![
                "sender_dependent_default_transport_maps = ".to_string(),
                "smtp_bind_address6 =".to_string(),
            ]
        );
        let closed = after(&on, &steps);
        assert_eq!(closed.sending(), Sending::Ipv4Only);
        assert_eq!(closed.bind6, None);
        // The stored choice survives: the preference and the services stay.
        assert_eq!(closed.preference, "ipv6");
        assert!(closed.service("smtp-ipv4").is_some() && closed.service("smtp-ipv4only").is_some());
        assert!(plan(&closed, &wanted).is_empty());
    }

    #[test]
    fn a_change_of_one_domain_touches_only_the_table() {
        let on = postfix_from(
            &main_cf("ipv4", V6, "inline:{@shop.example=smtp-ipv6:}"),
            "panel.example.com",
            Some("yes"),
            &format!("{MASTER}smtp-ipv6  unix  -  -  y  -  -  smtp -o smtp_address_preference=ipv6\n"),
        );
        let keep = want(None, Ipv6Use::From(V6.into()), entries(&[("shop.example", Route::Ipv6)]));
        assert!(plan(&on, &keep).is_empty());

        // A second domain on the same service: nothing in master.cf.
        let more = Desired {
            entries: entries(&[("shop.example", Route::Ipv6), ("blog.example", Route::Ipv6)]),
            ..keep.clone()
        };
        let steps = plan(&on, &more);
        assert_eq!(
            steps,
            Plan {
                late: vec!["sender_dependent_default_transport_maps = inline:{@blog.example=smtp-ipv6:,@shop.example=smtp-ipv6:}".to_string()],
                ..Plan::default()
            }
        );

        // Back to the server default: the entry goes at once.
        let none = Desired { entries: BTreeMap::new(), ..keep.clone() };
        let steps = plan(&on, &none);
        assert_eq!(
            steps,
            Plan {
                early: vec!["sender_dependent_default_transport_maps = ".to_string()],
                ..Plan::default()
            }
        );

        // A service somebody edited is written again before it is used.
        let mut edited = on.clone();
        edited.services.iter_mut().find(|s| s.name == "smtp-ipv6").unwrap().overrides.clear();
        assert_eq!(plan(&edited, &keep).master.len(), 1);

        // An address that moved is re-pinned.
        let moved = Desired { ipv6: Ipv6Use::From("2001:db8:1::2".into()), ..keep };
        assert_eq!(plan(&on, &moved).early, vec!["smtp_bind_address6 = 2001:db8:1::2".to_string()]);
    }

    #[test]
    fn holding_and_releasing_a_domain_needs_its_service_first_and_nothing_else() {
        let on = sending_v6("ipv4", "");
        // A domain created with an SPF record that lacks the address.
        let hold = want(None, Ipv6Use::From(V6.into()), entries(&[("new.example", Route::Ipv4Only)]));
        let steps = plan(&on, &hold);
        assert_eq!(
            steps,
            Plan {
                master: vec![strs(&[
                    "-M",
                    "smtp-ipv4only/unix=smtp-ipv4only unix - - y - - smtp -o inet_protocols=ipv4"
                ])],
                late: vec!["sender_dependent_default_transport_maps = inline:{@new.example=smtp-ipv4only:}".to_string()],
                ..Plan::default()
            }
        );
        let held = after(&on, &steps);
        assert_eq!(applied_holds(&held.sender_maps), names(&["new.example"]));
        // The stock services are not touched: every other domain keeps IPv6.
        assert_eq!(held.sending(), Sending::Pinned(V6.into()));
        assert!(plan(&held, &hold).is_empty());

        // A second held domain: the service is there already.
        let two = Desired {
            entries: entries(&[("new.example", Route::Ipv4Only), ("other.example", Route::Ipv4Only)]),
            ..hold.clone()
        };
        let steps = plan(&held, &two);
        assert!(steps.master.is_empty());
        assert_eq!(steps.late.len(), 1);

        // Its record was fixed: only the table changes, the service stays.
        let free = want(None, Ipv6Use::From(V6.into()), BTreeMap::new());
        let steps = plan(&held, &free);
        assert_eq!(
            steps,
            Plan {
                early: vec!["sender_dependent_default_transport_maps = ".to_string()],
                ..Plan::default()
            }
        );
        assert!(after(&held, &steps).service("smtp-ipv4only").is_some());

        // A service of that name that does something else is written again.
        let mut edited = held.clone();
        edited.services.iter_mut().find(|s| s.name == "smtp-ipv4only").unwrap().overrides.clear();
        assert_eq!(plan(&edited, &hold).master.len(), 1);
    }

    #[test]
    fn a_postfix_without_bind_enforce_and_a_server_without_relay_are_handled() {
        let master = MASTER.lines().filter(|l| !l.starts_with("relay")).collect::<Vec<_>>().join("\n");
        let now = postfix_from(&main_cf("ipv4", "", ""), "panel.example.com", None, &master);
        let closed = plan(&now, &off());
        assert_eq!(closed.master, vec![strs(&["-P", "smtp/unix/inet_protocols=ipv4"])]);
        let on = plan(&now, &want(None, Ipv6Use::From(V6.into()), BTreeMap::new()));
        assert_eq!(on.early, vec![format!("smtp_bind_address6 = {V6}")]);
        assert!(on.master.is_empty() && on.late.is_empty() && on.open.is_empty());
    }

    #[test]
    fn a_server_without_ipv4_is_never_limited_to_ipv4() {
        // No IPv4 to fall back to and no IPv6 address fit to pin: the sending
        // services stay as they are, only the stored choice is written.
        let wanted = want(
            Some(Protocol::Ipv6),
            Ipv6Use::Untouched,
            entries(&[("shop.example", Route::Ipv6)]),
        );
        let steps = plan(&legacy(), &wanted);
        assert_eq!(
            steps,
            Plan {
                early: vec!["smtp_address_preference = ipv6".to_string()],
                ..Plan::default()
            }
        );
        let then = after(&legacy(), &steps);
        assert_eq!(then.sending(), Sending::Unpinned);
        assert!(plan(&then, &wanted).is_empty());
    }

    #[test]
    fn the_ipv4_address_is_pinned_with_the_first_write_and_released_before_anything_else() {
        let pin = Desired { pin4: Pin4::To(V4B.into()), ..off() };
        let closed = after(&legacy(), &plan(&legacy(), &off()));

        // Not pinned yet: our parameter and the reference to it, in one write.
        let steps = plan(&closed, &pin);
        assert_eq!(
            steps,
            Plan {
                early: vec![
                    format!("vps_panel_smtp_bind_address = {V4B}"),
                    "smtp_bind_address = $vps_panel_smtp_bind_address".to_string(),
                ],
                ..Plan::default()
            }
        );
        let pinned = after(&closed, &steps);
        assert_eq!(pinned.bind4(), Bind4::Ours(Some(V4B.into())));
        // smtp_bind_address_enforce is not set for it: a pin that cannot be
        // applied must not stop mail on a server that sends over IPv4 only.
        assert_eq!(pinned.bind_enforce.as_deref(), Some("no"));
        assert!(plan(&pinned, &pin).is_empty());

        // The published address moved to another one of this server.
        let moved = Desired { pin4: Pin4::To("203.0.113.77".into()), ..off() };
        assert_eq!(
            plan(&pinned, &moved),
            Plan {
                early: vec!["vps_panel_smtp_bind_address = 203.0.113.77".to_string()],
                ..Plan::default()
            }
        );

        // No longer needed, or no longer right: both lines go, first of all.
        let steps = plan(&pinned, &off());
        assert_eq!(
            steps,
            Plan {
                drop: strs(&["smtp_bind_address", "vps_panel_smtp_bind_address"]),
                ..Plan::default()
            }
        );
        let released = after(&pinned, &steps);
        assert_eq!(released.bind4(), Bind4::Unset);
        assert!(plan(&released, &off()).is_empty());

        // What the kernel would send from is not known: nothing is touched.
        let keep = Desired { pin4: Pin4::Keep, ..off() };
        assert!(plan(&pinned, &keep).is_empty());
        assert!(plan(&closed, &keep).is_empty());

        // Our parameter was deleted by hand: written again, or the rest dropped.
        let mut broken = pinned.clone();
        broken.bind4_pin = None;
        assert_eq!(plan(&broken, &pin).early, vec![format!("vps_panel_smtp_bind_address = {V4B}")]);
        assert_eq!(plan(&broken, &off()).drop, strs(&["smtp_bind_address", "vps_panel_smtp_bind_address"]));
    }

    #[test]
    fn the_ipv4_pin_and_ipv6_come_on_in_one_write_and_the_pin_goes_before_ipv6_opens() {
        let closed = after(&legacy(), &plan(&legacy(), &off()));
        let wanted = Desired {
            pin4: Pin4::To(V4B.into()),
            ..want(Some(Protocol::Ipv6), Ipv6Use::From(V6.into()), BTreeMap::new())
        };
        let steps = plan(&closed, &wanted);
        // One postconf -e, so one write of main.cf: the pin is there when
        // smtp_bind_address_enforce starts to count.
        assert_eq!(
            steps.early,
            vec![
                "smtp_address_preference = ipv6".to_string(),
                format!("vps_panel_smtp_bind_address = {V4B}"),
                "smtp_bind_address = $vps_panel_smtp_bind_address".to_string(),
                format!("smtp_bind_address6 = {V6}"),
                "smtp_bind_address_enforce = yes".to_string(),
            ]
        );
        assert_eq!(steps.open.len(), 2);
        let on = after(&closed, &steps);
        assert!(plan(&on, &wanted).is_empty());

        // The pinned address left the server while IPv6 is on and the bind
        // is enforced: the pin is dropped before any other step.
        let gone = Desired { pin4: Pin4::Off, entries: entries(&[("shop.example", Route::Ipv6)]), ..wanted };
        let steps = plan(&on, &gone);
        assert_eq!(steps.drop, strs(&["smtp_bind_address", "vps_panel_smtp_bind_address"]));
        assert!(steps.early.is_empty());
        assert_eq!(steps.master.len(), 1);
        assert_eq!(steps.late.len(), 1);
    }

    #[test]
    fn an_address_the_operator_set_by_hand_is_never_touched() {
        for value in [V4B, "[203.0.113.9]", "192.0.2.1", "$my_bind_address"] {
            let theirs = by_hand(legacy(), value);
            for pin4 in [Pin4::To(V4.into()), Pin4::To(V4B.into()), Pin4::Off, Pin4::Keep] {
                let wanted = Desired { pin4, ..off() };
                let steps = plan(&theirs, &wanted);
                assert!(steps.drop.is_empty(), "{value}: {steps:?}");
                assert!(steps.early.is_empty(), "{value}: {steps:?}");
                assert_eq!(after(&theirs, &steps).bind4_raw, value);
            }
        }
        // They replaced our pin: the parameter nothing refers to any more is
        // taken out (postconf warns about it on every run), their value stays.
        let mut replaced = by_hand(legacy(), V4);
        replaced.pin_left_over = true;
        let steps = plan(&replaced, &Desired { pin4: Pin4::To(V4B.into()), ..off() });
        assert_eq!(steps.drop, strs(&["vps_panel_smtp_bind_address"]));
        assert!(steps.early.is_empty());
        let then = after(&replaced, &steps);
        assert_eq!(then.bind4(), Bind4::ByHand(V4.into()));
        assert!(!then.pin_left_over);
        // They removed smtp_bind_address altogether and the pin is still
        // needed: it is set again, nothing is dropped.
        let mut cleared = legacy();
        cleared.pin_left_over = true;
        let steps = plan(&cleared, &Desired { pin4: Pin4::To(V4B.into()), ..off() });
        assert!(steps.drop.is_empty());
        assert_eq!(steps.early.len(), 2);
    }

    #[test]
    fn a_failed_change_is_taken_back_setting_by_setting() {
        // IPv6 comes on with a pin, a held domain and a domain's own choice.
        let closed = after(&legacy(), &plan(&legacy(), &off()));
        let wanted = Desired {
            pin4: Pin4::To(V4B.into()),
            ..want(
                Some(Protocol::Ipv6),
                Ipv6Use::From(V6.into()),
                entries(&[("shop.example", Route::Ipv6), ("old.example", Route::Ipv4Only)]),
            )
        };
        let steps = plan(&closed, &wanted);
        let touched = [
            "smtp_address_preference",
            BIND4_PIN,
            BIND4,
            "smtp_bind_address6",
            "smtp_bind_address_enforce",
            SENDER_MAPS,
        ];
        let before = explicit(&closed, &touched);
        let back = undo(&steps, &closed, &before);
        assert_eq!(
            back,
            vec![
                // Last step first: the stock services are closed again...
                strs(&["-P", "relay/unix/inet_protocols=ipv4"]),
                strs(&["-P", "smtp/unix/inet_protocols=ipv4"]),
                // ...the table goes back...
                strs(&["-e", "sender_dependent_default_transport_maps = "]),
                // ...the services it named are removed again...
                strs(&["-M", "-X", "smtp-ipv4only/unix"]),
                strs(&["-M", "-X", "smtp-ipv6/unix"]),
                // ...and the first write is undone: what main.cf had is set
                // again, what it did not mention is taken out.
                strs(&["-e", "smtp_address_preference = ipv4", "smtp_bind_address6 = "]),
                strs(&["-X", "vps_panel_smtp_bind_address", "smtp_bind_address", "smtp_bind_address_enforce"]),
            ]
        );
        // Wherever the change stopped, the calls bring Postfix back.
        let mut part = closed.clone();
        assert_eq!(shape(&undone(part.clone(), &back)), shape(&closed));
        steps.early.iter().for_each(|s| set_main(&mut part, s));
        assert_eq!(shape(&undone(part.clone(), &back)), shape(&closed));
        steps.master.iter().for_each(|args| edit_master(&mut part, args));
        assert_eq!(shape(&undone(part.clone(), &back)), shape(&closed));
        steps.late.iter().for_each(|s| set_main(&mut part, s));
        assert_eq!(shape(&undone(part.clone(), &back)), shape(&closed));
        assert_eq!(shape(&undone(after(&closed, &steps), &back)), shape(&closed));
    }

    #[test]
    fn taking_a_change_back_restores_a_replaced_service_and_a_dropped_pin() {
        // Postfix sends over IPv6, pinned on IPv4 too, with a service of
        // ours that somebody edited.
        let on = pinned4(
            postfix_from(
                &main_cf("ipv6", V6, "inline:{@shop.example=smtp-ipv6:}"),
                "panel.example.com",
                Some("yes"),
                &format!("{MASTER}smtp-ipv6  unix  -  -  n  -  -  smtp -o smtp_helo_name=x.example\n"),
            ),
            V4B,
        );

        // The edited service is written again, the pin is released.
        let wanted = want(None, Ipv6Use::From(V6.into()), entries(&[("shop.example", Route::Ipv6)]));
        let steps = plan(&on, &wanted);
        assert_eq!(steps.drop, strs(&[BIND4, BIND4_PIN]));
        assert_eq!(steps.master.len(), 1);
        let back = undo(&steps, &on, &explicit(&on, &[BIND4, BIND4_PIN]));
        assert_eq!(
            back,
            vec![
                // The service exactly as it was read.
                strs(&[
                    "-M",
                    "smtp-ipv6/unix=smtp-ipv6  unix  -  -  n  -  -  smtp -o smtp_helo_name=x.example"
                ]),
                strs(&[
                    "-e",
                    "smtp_bind_address = $vps_panel_smtp_bind_address",
                    "vps_panel_smtp_bind_address = 203.0.113.9"
                ]),
            ]
        );

        // IPv6 goes off: closing the stock services is undone by taking the
        // setting out again, and the table and the address come back.
        let steps = plan(&on, &Desired { pin4: Pin4::To(V4B.into()), ..off() });
        let back = undo(&steps, &on, &explicit(&on, &[SENDER_MAPS, "smtp_bind_address6"]));
        assert_eq!(
            back,
            vec![
                strs(&[
                    "-e",
                    "sender_dependent_default_transport_maps = inline:{@shop.example=smtp-ipv6:}",
                    "smtp_bind_address6 = 2001:db8:1::1"
                ]),
                strs(&["-P", "-X", "relay/unix/inet_protocols"]),
                strs(&["-P", "-X", "smtp/unix/inet_protocols"]),
            ]
        );
        // Nothing else is ever named: myhostname, the certificate paths and
        // whatever else changed in main.cf meanwhile stay as they are.
        assert!(back.iter().flatten().all(|arg| !arg.contains("myhostname") && !arg.contains("tls")));
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
    fn the_ipv4_address_is_pinned_only_when_the_kernel_would_pick_another_and_it_is_safe() {
        // One public address: the kernel sends from it by itself.
        assert_eq!(ipv4_source(Some(V4), &one_v4(), None), Ipv4Source::AsPublished);
        // The panel hostname on the second address of the same interface.
        assert_eq!(
            ipv4_source(Some(V4B), &two_v4(), Some("eth0")),
            Ipv4Source::Pin { default: V4.into() }
        );
        // The second address is on another interface and no routing rule
        // sends its connections out there: they would leave through eth0.
        let two_links = facts4(&[V4B], &[("eth0", V4), ("eth1", V4B)]);
        match ipv4_source(Some(V4B), &two_links, Some("eth0")) {
            Ipv4Source::CannotPin { default, why } => {
                assert_eq!(default, V4);
                assert!(why.contains("would leave through eth0, not through eth1"), "{why}");
            }
            other => panic!("{other:?}"),
        }
        // With such a rule they leave through eth1: safe.
        assert_eq!(
            ipv4_source(Some(V4B), &two_links, Some("eth1")),
            Ipv4Source::Pin { default: V4.into() }
        );
        // The route could not be read: not pinned either.
        assert!(matches!(ipv4_source(Some(V4B), &two_v4(), None), Ipv4Source::CannotPin { .. }));

        // Behind NAT the published address is on no interface: binding to it
        // would fail, and the kernel's choice is the right one anyway.
        let nat = facts4(&[V4], &[("ens5", "172.31.20.4")]);
        assert_eq!(ipv4_source(Some(V4), &nat, None), Ipv4Source::AsPublished);
        // Connections leave through a private address (a VPN, a gateway):
        // where they come out is not known, nothing is done.
        let mut vpn = two_v4();
        vpn.default_source = Some("10.8.0.2".into());
        assert_eq!(ipv4_source(Some(V4B), &vpn, Some("eth0")), Ipv4Source::AsPublished);
        // A LAN install published under its private address.
        let lan = facts4(&["192.168.1.10"], &[("eth0", "192.168.1.11"), ("eth0", "192.168.1.10")]);
        assert_eq!(ipv4_source(Some("192.168.1.10"), &lan, Some("eth0")), Ipv4Source::AsPublished);
        // No default route, or no `ip`: not known.
        let mut dark = two_v4();
        dark.default_source = None;
        assert_eq!(ipv4_source(Some(V4B), &dark, None), Ipv4Source::Unknown);
        assert_eq!(ipv4_source(None, &two_v4(), None), Ipv4Source::AsPublished);
    }

    #[test]
    fn the_pin_follows_the_published_address_and_survives_a_failed_lookup() {
        let closed = after(&legacy(), &plan(&legacy(), &off()));
        let a = addrs_from(Some(&closed), facts(None, &[]), two_v4(), Some("eth0"));
        assert_eq!(a.ipv4.as_deref(), Some(V4B));
        assert_eq!(pin4_wanted(&closed, &a), Pin4::To(V4B.into()));
        // One address: no pin, and one that is there goes.
        let a = addrs_from(Some(&closed), facts(None, &[]), one_v4(), None);
        assert_eq!(pin4_wanted(&closed, &a), Pin4::Off);
        // Not safe: no pin.
        let a = addrs_from(Some(&closed), facts(None, &[]), two_v4(), Some("eth9"));
        assert_eq!(pin4_wanted(&closed, &a), Pin4::Off);

        // Mail handed to a relay host is not pinned.
        let mut relayed = closed.clone();
        relayed.relayhost = "[smtp.example.net]:587".into();
        let a = addrs_from(Some(&relayed), facts(None, &[]), two_v4(), Some("eth0"));
        assert_eq!(pin4_wanted(&relayed, &a), Pin4::Off);

        // The panel hostname could not be looked up at boot: the address
        // Postfix is pinned to stays the published one while it is on an
        // interface, and the pin with it...
        let pinned = pinned4(closed.clone(), V4B);
        let mut no_lookup = two_v4();
        no_lookup.named.clear();
        assert_eq!(mail_ipv4(Some(&pinned), &no_lookup).as_deref(), Some(V4B));
        let a = addrs_from(Some(&pinned), facts(None, &[]), no_lookup.clone(), Some("eth0"));
        assert_eq!(pin4_wanted(&pinned, &a), Pin4::To(V4B.into()));
        assert!(plan(&pinned, &Desired { pin4: pin4_wanted(&pinned, &a), ..off() }).is_empty());
        // ...without a pin the server's own address is taken, as before.
        assert_eq!(mail_ipv4(Some(&closed), &no_lookup).as_deref(), Some(V4));
        // An address set by hand is never taken for ours.
        assert_eq!(mail_ipv4(Some(&by_hand(closed.clone(), V4B)), &no_lookup).as_deref(), Some(V4));
        // The pinned address left the server: the pin goes.
        let mut left = no_lookup.clone();
        left.interfaces.retain(|(_, ip)| ip != V4B);
        assert_eq!(mail_ipv4(Some(&pinned), &left).as_deref(), Some(V4));
        let a = addrs_from(Some(&pinned), facts(None, &[]), left.clone(), None);
        assert_eq!(pin4_wanted(&pinned, &a), Pin4::Off);

        // What the kernel sends from cannot be read: the pin stays as it is,
        // unless its address is known to be gone.
        let mut dark = two_v4();
        dark.default_source = None;
        let a = addrs_from(Some(&pinned), facts(None, &[]), dark.clone(), None);
        assert_eq!(pin4_wanted(&pinned, &a), Pin4::Keep);
        let moved = pinned4(closed.clone(), "192.0.2.77");
        let a = addrs_from(Some(&moved), facts(None, &[]), dark, None);
        assert_eq!(pin4_wanted(&moved, &a), Pin4::Off);
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

        // A new mail domain neither runs the check nor switches anything.
        let added = Trigger::DomainAdded("new.example");
        for wants in [false, true] {
            assert!(!needs_check(true, true, wants, added) && !needs_check(true, false, wants, added));
            assert!(use_ipv6(true, true, wants, added, false));
            assert!(!use_ipv6(true, false, wants, added, true));
        }
    }

    #[test]
    fn settings_report_what_postfix_does_not_what_was_asked() {
        let prefs = vec![
            pref("shop.example", Preference::Ipv6),
            pref("blog.example", Preference::Default),
            pref("old.example", Preference::Ipv4),
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
        let on = sending_v6("ipv4", "inline:{@old.example=smtp-ipv4:,@shop.example=smtp-ipv6:}");
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
        let drift = sending_v6("ipv4", "");
        let s = build_settings(&drift, &addrs(&drift, facts(Some(V6), &[V6])), &prefs);
        assert_eq!(s.domains[2].effective, Protocol::Ipv4);
        assert!(s.notes.iter().any(|n| n.contains("does not route old.example, shop.example as chosen yet")), "{:?}", s.notes);
        // So does an entry for a domain that follows the server default.
        let stray = sending_v6("ipv4", "inline:{@blog.example=smtp-ipv6:,@old.example=smtp-ipv4:,@shop.example=smtp-ipv6:}");
        let s = build_settings(&stray, &addrs(&stray, facts(Some(V6), &[V6])), &prefs);
        assert!(s.notes.iter().any(|n| n.contains("does not route blog.example as chosen yet")), "{:?}", s.notes);
    }

    #[test]
    fn a_held_domain_shows_as_ipv4_and_a_note_says_why_and_what_to_do() {
        let prefs = vec![
            pref("blog.example", Preference::Default),
            forced("mine.example"),
            pref("shop.example", Preference::Ipv6),
        ];
        let on = sending_v6(
            "ipv6",
            "inline:{@blog.example=smtp-ipv4only:,@mine.example=smtp-ipv6:,@shop.example=smtp-ipv4only:}",
        );
        let a = addrs(&on, facts(Some(V6), &[V6]));
        assert!(a.sends_ipv6);
        assert_eq!(a.held, strs(&["blog.example", "shop.example"]));
        let s = build_settings(&on, &a, &prefs);
        // What was chosen stays stored; what happens is IPv4.
        assert_eq!(
            s.domains.iter().map(|d| (d.name.as_str(), d.preference, d.effective)).collect::<Vec<_>>(),
            vec![
                ("blog.example", Preference::Default, Protocol::Ipv4),
                ("mine.example", Preference::Ipv6, Protocol::Ipv6),
                ("shop.example", Preference::Ipv6, Protocol::Ipv4),
            ]
        );
        assert_eq!(s.notes.len(), 2, "{:?}", s.notes);
        let note = &s.notes[1];
        assert!(note.starts_with("Mail from blog.example, shop.example goes out over IPv4 only"), "{note}");
        assert!(note.contains(&format!("their SPF record did not allow mail from {V6}")), "{note}");
        assert!(note.contains("Run the DNS check of those domains") && note.contains("save this setting again"), "{note}");
        // Held is how they are meant to be routed: not reported as drift.
        assert!(!s.notes.iter().any(|n| n.contains("as chosen yet")), "{:?}", s.notes);

        // A forced domain the table holds all the same is drift.
        let wrong = sending_v6("ipv6", "inline:{@mine.example=smtp-ipv4only:}");
        let s = build_settings(&wrong, &addrs(&wrong, facts(Some(V6), &[V6])), &prefs[1..2]);
        assert!(s.notes.iter().any(|n| n.contains("does not route mine.example as chosen yet")), "{:?}", s.notes);
        assert!(!s.notes.iter().any(|n| n.contains("goes out over IPv4 only")), "{:?}", s.notes);

        // While Postfix does not send over IPv6 nothing is held.
        let mut closed = on.clone();
        closed.services.iter_mut().find(|s| s.name == "smtp" && s.kind == "unix").unwrap()
            .overrides.push(("inet_protocols".into(), "ipv4".into()));
        let a = addrs(&closed, facts(Some(V6), &[V6]));
        assert!(!a.sends_ipv6 && a.held.is_empty());
    }

    #[test]
    fn notes_say_where_ipv4_mail_leaves_from_when_that_is_not_plain() {
        let closed = after(&legacy(), &plan(&legacy(), &off()));
        let ipv4_only = |p: &Postfix, f4: Ipv4Facts, dev: Option<&str>| {
            let a = addrs_from(Some(p), facts(None, &[]), f4, dev);
            build_settings(p, &a, &[]).notes
        };
        // The usual server: nothing to say about IPv4.
        assert_eq!(ipv4_only(&closed, one_v4(), None).len(), 1);

        // Two addresses, pinned by this module.
        let notes = ipv4_only(&pinned4(closed.clone(), V4B), two_v4(), Some("eth0"));
        assert!(notes[1].contains(&format!("would send from {V4} by itself. Postfix is set to send from {V4B}")), "{notes:?}");
        // Not pinned yet.
        let notes = ipv4_only(&closed, two_v4(), Some("eth0"));
        assert!(notes[1].contains(&format!("Save this setting to make Postfix send from {V4B}")), "{notes:?}");
        // Cannot be pinned.
        let two_links = facts4(&[V4B], &[("eth0", V4), ("eth1", V4B)]);
        let notes = ipv4_only(&closed, two_links, Some("eth0"));
        assert!(notes[1].contains(&format!("sends IPv4 mail from {V4}")), "{notes:?}");
        assert!(notes[1].contains("would leave through eth0, not through eth1"), "{notes:?}");
        assert!(notes[1].contains(&format!("Point the panel hostname at {V4}")), "{notes:?}");

        // Pinned to an address that has left the server since.
        let notes = ipv4_only(&pinned4(closed.clone(), "192.0.2.77"), two_v4(), Some("eth0"));
        assert!(notes[1].contains("still set to send IPv4 mail from 192.0.2.77, which is no longer an address of this server"), "{notes:?}");
        assert!(notes[1].contains("Save this setting to release it"), "{notes:?}");
        assert_eq!(notes.len(), 2, "{notes:?}");

        // Set by hand to another public address: reported, with the way out.
        let notes = ipv4_only(&by_hand(closed.clone(), V4), two_v4(), Some("eth0"));
        assert!(notes[1].contains(&format!("set by hand to send IPv4 mail from {V4}")), "{notes:?}");
        assert!(notes[1].contains(&format!("A and SPF records publish {V4B}")), "{notes:?}");
        assert!(notes[1].contains("postconf -X smtp_bind_address"), "{notes:?}");
        // By hand to the published address, or to a private one behind NAT:
        // that is as it should be.
        assert_eq!(ipv4_only(&by_hand(closed.clone(), V4B), two_v4(), Some("eth0")).len(), 1);
        let nat = facts4(&[V4], &[("ens5", "172.31.20.4"), ("ens6", "172.31.30.4")]);
        assert_eq!(ipv4_only(&by_hand(closed.clone(), "172.31.30.4"), nat, None).len(), 1);

        // With a relay host only the relay note is shown.
        let mut relayed = by_hand(closed.clone(), V4);
        relayed.relayhost = "[smtp.example.net]:587".into();
        let notes = ipv4_only(&relayed, two_v4(), Some("eth0"));
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes[1].contains("[smtp.example.net]:587"), "{notes:?}");
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
        let v6_only = addrs_from(Some(&legacy()), facts(None, &[V6]), Ipv4Facts::default(), None);
        assert!(!v6_only.has_ipv4);
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

    /// main.cf as `postconf -n` lists it. Not config_directory: with
    /// MAIL_CONFIG set, postconf lists it at some times and not at others.
    async fn main_cf_now() -> BTreeMap<String, String> {
        let mut main = parse_postconf(&postconf(&["-n"]).await.unwrap());
        main.remove("config_directory");
        main
    }

    async fn master_cf_now() -> Vec<String> {
        let mut lines: Vec<String> = postconf(&["-M"])
            .await
            .unwrap()
            .lines()
            .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect();
        lines.sort();
        lines
    }

    /// The real Postfix tools (postconf, postmap, postfix check) against a
    /// copy of a Postfix configuration, never against /etc/postfix itself:
    ///
    ///   cp -a /etc/postfix /tmp/pf
    ///   MAIL_CONFIG=/tmp/pf cargo test --manifest-path agent/Cargo.toml -- --ignored real_postfix
    #[tokio::test]
    #[ignore = "edits the Postfix configuration MAIL_CONFIG names; point it at a copy (see above)"]
    async fn real_postfix_takes_the_settings_and_gives_them_back() {
        let dir = std::env::var("MAIL_CONFIG").expect("set MAIL_CONFIG to a copy of /etc/postfix");
        assert!(!dir.starts_with("/etc"), "MAIL_CONFIG must name a copy, not {dir}");

        // From wherever the copy stands to IPv4 only, without a pin.
        let start = read_postfix().await.unwrap();
        converge(&start, &off()).await.unwrap();
        let closed = read_postfix().await.unwrap();
        assert_eq!(closed.sending(), Sending::Ipv4Only);
        assert_eq!(closed.bind4(), Bind4::Unset);
        assert!(!closed.pin_left_over);
        assert!(plan(&closed, &off()).is_empty());

        // IPv6 on, with an entry of each kind and the IPv4 address pinned.
        let wanted = Desired {
            pin4: Pin4::To(V4B.into()),
            ..want(
                Some(Protocol::Ipv6),
                Ipv6Use::From(V6.into()),
                entries(&[
                    ("a.example", Route::Ipv4Only),
                    ("b.example", Route::Ipv6),
                    ("c.example", Route::Ipv4),
                ]),
            )
        };
        assert!(converge(&closed, &wanted).await.unwrap());
        let on = read_postfix().await.unwrap();
        assert_eq!(on.sending(), Sending::Pinned(V6.into()));
        assert_eq!(on.default_protocol(), Protocol::Ipv6);
        assert_eq!(on.bind4(), Bind4::Ours(Some(V4B.into())));
        assert_eq!(applied_entries(&on.sender_maps), wanted.entries);
        for route in Route::ALL {
            assert!(on.service(route.service()).is_some_and(|s| service_sends(s, route)), "{route:?}");
        }
        // Postfix resolves the reference to the address,
        assert_eq!(postconf(&["-xh", BIND4]).await.unwrap().trim(), V4B);
        // reads the table the way it is meant,
        let table = build_table(&wanted.entries).unwrap();
        for (domain, route) in &wanted.entries {
            let key = format!("@{domain}");
            let got = exec::run("postmap", ["-q", key.as_str(), table.as_str()]).await.unwrap();
            assert_eq!(got.stdout.trim(), format!("{}:", route.service()));
        }
        // and a second run finds nothing to do.
        assert!(!converge(&on, &wanted).await.unwrap());
        let (main_on, master_on) = (main_cf_now().await, master_cf_now().await);

        // A change Postfix does not take as asked (it reads the address back
        // in lower case) is taken back completely.
        let refused = Desired {
            ipv6: Ipv6Use::From("2001:DB8:1::2".into()),
            pin4: Pin4::Off,
            entries: entries(&[("d.example", Route::Ipv4Only)]),
            ..wanted.clone()
        };
        let err = converge(&on, &refused).await.unwrap_err();
        assert!(err.to_string().contains("did not take the new settings"), "{err}");
        assert_eq!(main_cf_now().await, main_on);
        assert_eq!(master_cf_now().await, master_on);

        // Taking a change back does not undo what somebody else changed
        // meanwhile: a certificate (ssl.rs) and myhostname (system.rs).
        let steps = plan(&on, &off());
        assert!(!steps.drop.is_empty() && !steps.master.is_empty() && !steps.late.is_empty());
        let back = undo(&steps, &on, &main_cf_values(&steps).await.unwrap());
        run_plan(&steps).await.unwrap();
        assert_eq!(read_postfix().await.unwrap().sending(), Sending::Ipv4Only);
        postconf(&[
            "-e",
            "smtpd_tls_cert_file = /etc/vps-panel/certs/new.example.com/fullchain.pem",
            "myhostname = new.example.com",
        ])
        .await
        .unwrap();
        for call in &back {
            postconf_owned(call).await.unwrap();
        }
        let mut expected = main_on.clone();
        expected.insert(
            "smtpd_tls_cert_file".into(),
            "/etc/vps-panel/certs/new.example.com/fullchain.pem".into(),
        );
        expected.insert("myhostname".into(), "new.example.com".into());
        assert_eq!(main_cf_now().await, expected);
        assert_eq!(master_cf_now().await, master_on);
        exec::run("postfix", ["check"]).await.unwrap();

        // The operator replaces the pin by hand: their address stays, and
        // the parameter of ours that nothing refers to any more is removed.
        postconf(&["-e", "smtp_bind_address = 192.0.2.1"]).await.unwrap();
        let theirs = read_postfix().await.unwrap();
        assert_eq!(theirs.bind4(), Bind4::ByHand("192.0.2.1".into()));
        assert!(theirs.pin_left_over);
        assert!(converge(&theirs, &wanted).await.unwrap());
        let theirs = read_postfix().await.unwrap();
        assert_eq!(theirs.bind4(), Bind4::ByHand("192.0.2.1".into()));
        assert!(!theirs.pin_left_over);
        assert!(!main_cf_now().await.contains_key(BIND4_PIN));

        // Back to IPv4 only: the table and the IPv6 address go, the
        // operator's address and the services stay.
        assert!(converge(&theirs, &off()).await.unwrap());
        let end = read_postfix().await.unwrap();
        assert_eq!(end.sending(), Sending::Ipv4Only);
        assert_eq!(end.bind6, None);
        assert!(applied_entries(&end.sender_maps).is_empty());
        assert_eq!(end.bind4(), Bind4::ByHand("192.0.2.1".into()));
        assert!(end.service("smtp-ipv4only").is_some());
        exec::run("postfix", ["check"]).await.unwrap();
    }
}
