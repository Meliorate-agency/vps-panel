//! Live checks for outgoing mail: is each IP protocol fit to send from
//! (`GET /mail/ip-check`), and are one domain's DNS records right for both
//! (`GET /mail/domains/{name}/dns-check`).
//!
//! Everything is looked up at public resolvers with `dig`, the way a receiving
//! server sees it, never through this server's own resolver or /etc/hosts.
//! Every lookup has a timeout. A lookup that cannot be made (no `dig`, no
//! network) becomes a check item that says so, never an error response.
//! Read-only: nothing here changes DNS or Postfix.

use crate::error::{AgentError, AgentResult};
use crate::exec;
use crate::handlers::mail::mail_conn;
use crate::handlers::mail_ip::{self, Bind4, Ipv4Source, MailAddrs};
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    Json,
};
use futures::StreamExt;
use mysql_async::prelude::*;
use serde::Serialize;
use std::collections::HashMap;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

const DKIM_SELECTOR: &str = "default";
/// Tried in turn: the next one is asked when one does not answer, and once
/// more when the first has nothing (it may still hold "no such record" from
/// before the record was added).
const RESOLVERS: [&str; 3] = ["1.1.1.1", "8.8.8.8", "9.9.9.9"];
/// One `dig` run: it waits 2 seconds for an answer itself.
const DIG_TIMEOUT: Duration = Duration::from_secs(4);
/// SPF lookups for all mail domains together.
const SPF_SWEEP_TIMEOUT: Duration = Duration::from_secs(12);
/// A whole family or domain check.
const CHECK_TIMEOUT: Duration = Duration::from_secs(25);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MailCheckItem {
    pub id: String,
    pub label: String,
    pub status: Status,
    pub detail: String,
}

fn item(id: &str, label: &str, status: Status, detail: impl Into<String>) -> MailCheckItem {
    MailCheckItem {
        id: id.to_string(),
        label: label.to_string(),
        status,
        detail: detail.into(),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MailIpFamilyCheck {
    pub address: Option<String>,
    /// The address is known and no check failed.
    pub ready: bool,
    pub checks: Vec<MailCheckItem>,
}

#[derive(Serialize)]
pub struct MailIpCheck {
    /// The name Postfix introduces itself with (myhostname).
    pub helo_host: String,
    pub ipv4: MailIpFamilyCheck,
    pub ipv6: MailIpFamilyCheck,
}

#[derive(Serialize)]
pub struct MailDomainDnsCheck {
    pub domain: String,
    /// The zone is served from this panel's PowerDNS, so Reconcile repairs it.
    pub managed: bool,
    pub checks: Vec<MailCheckItem>,
}

fn is_ready(address: Option<&str>, checks: &[MailCheckItem]) -> bool {
    address.is_some() && !checks.iter().any(|c| c.status == Status::Fail)
}

/// What stops IPv6 from being chosen, for the 409 of the settings endpoints.
pub(crate) fn not_ready_message(check: &MailIpFamilyCheck) -> String {
    let failed: Vec<&MailCheckItem> = check
        .checks
        .iter()
        .filter(|c| c.status == Status::Fail)
        .collect();
    let mut msg = String::from("IPv6 is not ready to send mail from.");
    if let Some(first) = failed.first() {
        msg.push(' ');
        msg.push_str(&first.detail);
    }
    if failed.len() > 1 {
        msg.push_str(&format!(
            " ({} more: run the check to see them.)",
            failed.len() - 1
        ));
    }
    msg
}

// ---------------------------------------------------------------------------
// DNS lookups
// ---------------------------------------------------------------------------

pub(crate) type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Lookup {
    /// A and AAAA: addresses. MX: host names, best priority first. TXT: the
    /// text with its quoting removed. PTR and NS: host names. Names are
    /// lowercase without the trailing dot.
    Records(Vec<String>),
    /// The resolvers answered: there is no such record.
    Empty,
    /// No answer could be had; the reason, as a sentence fragment.
    Failed(String),
}

/// Public DNS as the checks see it. A trait so the checks run in tests
/// against a table of answers.
pub(crate) trait Resolver: Send + Sync {
    /// `rtype` is A, AAAA, MX, TXT, NS or PTR; for PTR `name` is the address.
    fn lookup<'a>(&'a self, name: &'a str, rtype: &'a str) -> BoxFut<'a, Lookup>;
}

/// `dig` against the public resolvers. Answers are kept for the run, and once
/// lookups turn out to be impossible the rest fail at once instead of each
/// waiting for its own timeout.
#[derive(Default)]
pub(crate) struct Dig {
    cache: tokio::sync::Mutex<HashMap<(String, String), Lookup>>,
    down: std::sync::Mutex<Option<String>>,
}

impl Dig {
    fn down(&self) -> Option<String> {
        self.down.lock().ok().and_then(|d| d.clone())
    }

    fn set_down(&self, why: &str) -> Lookup {
        if let Ok(mut d) = self.down.lock() {
            *d = Some(why.to_string());
        }
        Lookup::Failed(why.to_string())
    }

    async fn query(&self, name: &str, rtype: &str) -> Lookup {
        if let Some(why) = self.down() {
            return Lookup::Failed(why);
        }
        let mut saw_empty = false;
        let mut no_reply = 0;
        for resolver in RESOLVERS {
            let at = format!("@{resolver}");
            let mut args = vec!["+short", "+time=2", "+tries=1", at.as_str()];
            if rtype == "PTR" {
                args.extend(["-x", name]);
            } else {
                args.extend([name, rtype]);
            }
            match tokio::time::timeout(DIG_TIMEOUT, exec::run("dig", args)).await {
                Ok(Ok(out)) => {
                    let records = parse_dig_short(&out.stdout, rtype);
                    if !records.is_empty() {
                        return Lookup::Records(records);
                    }
                    if saw_empty {
                        return Lookup::Empty;
                    }
                    saw_empty = true;
                }
                Ok(Err(AgentError::Io(e))) if e.kind() == std::io::ErrorKind::NotFound => {
                    return self.set_down(
                        "dig is not installed on this server (apt install bind9-dnsutils)",
                    );
                }
                // No reply from this resolver (dig exits with 9): ask the next.
                Ok(Err(AgentError::Subprocess { code: 9, .. })) | Err(_) => no_reply += 1,
                Ok(Err(_)) => {}
            }
        }
        if saw_empty {
            return Lookup::Empty;
        }
        // No resolver answered at all: the rest of the run would only wait
        // for the same timeouts. Anything else is about this one name.
        if no_reply == RESOLVERS.len() {
            return self.set_down("this server could not reach a public DNS resolver");
        }
        Lookup::Failed(format!("dig could not look up {name}"))
    }
}

impl Resolver for Dig {
    fn lookup<'a>(&'a self, name: &'a str, rtype: &'a str) -> BoxFut<'a, Lookup> {
        Box::pin(async move {
            let key = (name.to_ascii_lowercase(), rtype.to_string());
            if let Some(hit) = self.cache.lock().await.get(&key) {
                return hit.clone();
            }
            let answer = self.query(&key.0, rtype).await;
            self.cache.lock().await.insert(key, answer.clone());
            answer
        })
    }
}

fn host_name(s: &str) -> String {
    s.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// The records in `dig +short` output. A lookup that goes through a CNAME
/// prints the CNAME target first; only lines of the asked type are kept.
fn parse_dig_short(stdout: &str, rtype: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut mx: Vec<(u16, String)> = Vec::new();
    for line in stdout.lines().map(str::trim) {
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        match rtype {
            "A" => {
                if let Ok(ip) = line.parse::<Ipv4Addr>() {
                    out.push(ip.to_string());
                }
            }
            "AAAA" => {
                if let Ok(ip) = line.parse::<Ipv6Addr>() {
                    out.push(ip.to_string());
                }
            }
            "TXT" => {
                if line.starts_with('"') {
                    out.push(unquote_txt(line));
                }
            }
            "MX" => {
                let mut it = line.split_whitespace();
                if let (Some(prio), Some(host)) =
                    (it.next().and_then(|p| p.parse::<u16>().ok()), it.next())
                {
                    let host = host_name(host);
                    // "0 ." is a null MX: the domain takes no mail.
                    if !host.is_empty() {
                        mx.push((prio, host));
                    }
                }
            }
            _ => {
                let name = host_name(line);
                if !name.is_empty()
                    && !name.contains(char::is_whitespace)
                    && name.parse::<IpAddr>().is_err()
                    && !name.ends_with(".arpa")
                {
                    out.push(name);
                }
            }
        }
    }
    if rtype == "MX" {
        mx.sort();
        out = mx.into_iter().map(|(_, h)| h).collect();
    }
    out.dedup();
    out
}

/// `"v=DKIM1; k=rsa; p=MIIB..." "...rest"` -> the text. A TXT record longer
/// than 255 bytes comes as several quoted strings that belong together.
fn unquote_txt(line: &str) -> String {
    let mut out = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => quoted = !quoted,
            '\\' if quoted => {
                let mut digits = String::new();
                while digits.len() < 3 {
                    match chars.peek() {
                        Some(d) if d.is_ascii_digit() => {
                            digits.push(*d);
                            chars.next();
                        }
                        _ => break,
                    }
                }
                if digits.is_empty() {
                    if let Some(next) = chars.next() {
                        out.push(next);
                    }
                } else if let (3, Ok(byte)) = (digits.len(), digits.parse::<u8>()) {
                    out.push(byte as char);
                } else {
                    out.push_str(&digits);
                }
            }
            c if quoted => out.push(c),
            _ => {}
        }
    }
    out
}

// ---------------------------------------------------------------------------
// SPF
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Spf {
    Pass,
    Fail,
    SoftFail,
    Neutral,
    /// The domain has no SPF record.
    NoRecord,
    /// More than one SPF record: receivers then use none of them.
    Multiple,
    /// The record is there but receivers cannot use it (syntax, more than
    /// ten lookups, an include without a record).
    Broken(String),
    /// A lookup failed, so the answer is not known.
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SpfOutcome {
    pub result: Spf,
    /// The domain's SPF record, when it has exactly one.
    pub record: Option<String>,
}

fn is_spf(txt: &str) -> bool {
    let t = txt.trim_start().to_ascii_lowercase();
    t == "v=spf1" || t.starts_with("v=spf1 ")
}

#[derive(Debug, PartialEq)]
enum Term {
    Mechanism {
        qualifier: char,
        name: String,
        arg: Option<String>,
        /// The prefix lengths after the mechanism: `/n` and `//n`.
        prefix: Option<u8>,
        prefix6: Option<u8>,
    },
    Redirect(String),
    /// exp= and unknown modifiers: nothing to evaluate.
    Modifier,
}

/// One term of an SPF record; None when it is not valid.
fn parse_term(term: &str) -> Option<Term> {
    let t = term.to_ascii_lowercase();
    if let Some(i) = t.find(|c| c == ':' || c == '/' || c == '=') {
        if t.as_bytes()[i] == b'=' {
            return Some(match &t[..i] {
                "redirect" => Term::Redirect(t[i + 1..].to_string()),
                _ => Term::Modifier,
            });
        }
    }
    let (qualifier, rest) = match t.chars().next()? {
        c @ ('+' | '-' | '~' | '?') => (c, &t[1..]),
        _ => ('+', &t[..]),
    };
    let (head, lengths) = match rest.find('/') {
        Some(i) => (&rest[..i], Some(&rest[i..])),
        None => (rest, None),
    };
    let (prefix, prefix6) = match lengths {
        None => (None, None),
        Some(l) => match l.strip_prefix("//") {
            Some(six) => (None, Some(six.parse().ok()?)),
            None => match l[1..].split_once("//") {
                Some((four, six)) => (Some(four.parse().ok()?), Some(six.parse().ok()?)),
                None => (Some(l[1..].parse().ok()?), None),
            },
        },
    };
    let (name, arg) = match head.split_once(':') {
        Some((n, a)) => (n, Some(a.to_string())),
        None => (head, None),
    };
    if name.is_empty() {
        return None;
    }
    Some(Term::Mechanism {
        qualifier,
        name: name.to_string(),
        arg,
        prefix,
        prefix6,
    })
}

fn in_cidr(ip: IpAddr, net: IpAddr, prefix: u8) -> bool {
    match (ip, net) {
        (IpAddr::V4(a), IpAddr::V4(b)) => {
            let p = u32::from(prefix.min(32));
            let mask = if p == 0 { 0 } else { u32::MAX << (32 - p) };
            u32::from(a) & mask == u32::from(b) & mask
        }
        (IpAddr::V6(a), IpAddr::V6(b)) => {
            let p = u32::from(prefix.min(128));
            let mask = if p == 0 { 0 } else { u128::MAX << (128 - p) };
            u128::from(a) & mask == u128::from(b) & mask
        }
        _ => false,
    }
}

/// Would a receiver that checks SPF accept mail for `domain` from `ip`?
/// Evaluates the record as RFC 7208 says (first match wins, ten lookups at
/// most). `exists`, `ptr` and macros are not followed: they never match here.
pub(crate) async fn spf_check(r: &dyn Resolver, domain: &str, ip: IpAddr) -> SpfOutcome {
    let lookups = AtomicU32::new(0);
    spf_eval(r, domain.to_ascii_lowercase(), ip, &lookups).await
}

fn spf_eval<'a>(
    r: &'a dyn Resolver,
    domain: String,
    ip: IpAddr,
    lookups: &'a AtomicU32,
) -> BoxFut<'a, SpfOutcome> {
    Box::pin(async move {
        let done = |result: Spf, record: &Option<String>| SpfOutcome {
            result,
            record: record.clone(),
        };
        let txts = match r.lookup(&domain, "TXT").await {
            Lookup::Records(t) => t,
            Lookup::Empty => return done(Spf::NoRecord, &None),
            Lookup::Failed(why) => return done(Spf::Unknown(why), &None),
        };
        let mut records = txts.into_iter().filter(|t| is_spf(t));
        let record = match (records.next(), records.next()) {
            (None, _) => return done(Spf::NoRecord, &None),
            (Some(_), Some(_)) => return done(Spf::Multiple, &None),
            (Some(one), None) => Some(one),
        };
        let text = record.clone().unwrap_or_default();
        let rtype = if ip.is_ipv4() { "A" } else { "AAAA" };
        let full = if ip.is_ipv4() { 32 } else { 128 };
        // A term that needs DNS counts against the limit of ten.
        let spend = || lookups.fetch_add(1, Ordering::Relaxed) < 10;
        let too_many = || Spf::Broken("it needs more than 10 DNS lookups".to_string());

        let mut redirect: Option<String> = None;
        for raw in text.split_whitespace().skip(1) {
            let Some(term) = parse_term(raw) else {
                return done(Spf::Broken(format!("\"{raw}\" is not valid SPF")), &record);
            };
            let (qualifier, name, arg, prefix, prefix6) = match term {
                Term::Modifier => continue,
                Term::Redirect(target) => {
                    redirect = Some(target);
                    continue;
                }
                Term::Mechanism { qualifier, name, arg, prefix, prefix6 } => {
                    (qualifier, name, arg, prefix, prefix6)
                }
            };
            // The length that applies to the address family being checked.
            let host_prefix = if ip.is_ipv4() { prefix } else { prefix6 }.unwrap_or(full);
            let target = arg.clone().unwrap_or_else(|| domain.clone());
            let macro_in_target = target.contains('%');
            let matched = match name.as_str() {
                "all" => true,
                "ip4" | "ip6" => {
                    let Some(net) = arg.as_deref().and_then(|a| a.parse::<IpAddr>().ok()) else {
                        return done(Spf::Broken(format!("\"{raw}\" is not valid SPF")), &record);
                    };
                    in_cidr(ip, net, prefix.unwrap_or(full))
                }
                "a" if !macro_in_target => {
                    if !spend() {
                        return done(too_many(), &record);
                    }
                    match r.lookup(&target, rtype).await {
                        Lookup::Records(addrs) => addrs
                            .iter()
                            .filter_map(|a| a.parse::<IpAddr>().ok())
                            .any(|a| in_cidr(ip, a, host_prefix)),
                        Lookup::Empty => false,
                        Lookup::Failed(why) => return done(Spf::Unknown(why), &record),
                    }
                }
                "mx" if !macro_in_target => {
                    if !spend() {
                        return done(too_many(), &record);
                    }
                    let hosts = match r.lookup(&target, "MX").await {
                        Lookup::Records(h) => h,
                        Lookup::Empty => Vec::new(),
                        Lookup::Failed(why) => return done(Spf::Unknown(why), &record),
                    };
                    let mut hit = false;
                    for host in hosts.iter().take(10) {
                        match r.lookup(host, rtype).await {
                            Lookup::Records(addrs) => {
                                if addrs
                                    .iter()
                                    .filter_map(|a| a.parse::<IpAddr>().ok())
                                    .any(|a| in_cidr(ip, a, host_prefix))
                                {
                                    hit = true;
                                    break;
                                }
                            }
                            Lookup::Empty => {}
                            Lookup::Failed(why) => return done(Spf::Unknown(why), &record),
                        }
                    }
                    hit
                }
                "include" if !macro_in_target => {
                    let Some(included) = arg else {
                        return done(Spf::Broken(format!("\"{raw}\" is not valid SPF")), &record);
                    };
                    if !spend() {
                        return done(too_many(), &record);
                    }
                    match spf_eval(r, included.clone(), ip, lookups).await.result {
                        Spf::Pass => true,
                        Spf::Fail | Spf::SoftFail | Spf::Neutral => false,
                        Spf::NoRecord | Spf::Multiple => {
                            return done(
                                Spf::Broken(format!("include:{included} has no usable SPF record")),
                                &record,
                            )
                        }
                        other => return done(other, &record),
                    }
                }
                // Not followed: they do not name fixed addresses.
                "a" | "mx" | "include" | "exists" | "ptr" => {
                    if !spend() {
                        return done(too_many(), &record);
                    }
                    false
                }
                _ => return done(Spf::Broken(format!("\"{raw}\" is not valid SPF")), &record),
            };
            if matched {
                let result = match qualifier {
                    '-' => Spf::Fail,
                    '~' => Spf::SoftFail,
                    '?' => Spf::Neutral,
                    _ => Spf::Pass,
                };
                return done(result, &record);
            }
        }
        match redirect {
            Some(target) if !target.contains('%') => {
                if !spend() {
                    return done(too_many(), &record);
                }
                match spf_eval(r, target.clone(), ip, lookups).await.result {
                    Spf::NoRecord | Spf::Multiple => done(
                        Spf::Broken(format!("redirect={target} has no usable SPF record")),
                        &record,
                    ),
                    other => done(other, &record),
                }
            }
            _ => done(Spf::Neutral, &record),
        }
    })
}

/// How a receiver treats a result that is not a pass, in words.
fn spf_words(result: &Spf) -> String {
    match result {
        Spf::Pass => "allowed".into(),
        Spf::Fail => "refused, -all".into(),
        Spf::SoftFail => "marked as suspicious, ~all".into(),
        Spf::Neutral => "neither allowed nor refused".into(),
        Spf::NoRecord => "no SPF record".into(),
        Spf::Multiple => "more than one SPF record".into(),
        Spf::Broken(why) | Spf::Unknown(why) => why.clone(),
    }
}

// ---------------------------------------------------------------------------
// GET /mail/ip-check
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
enum Family {
    V4,
    V6,
}

impl Family {
    fn rtype(self) -> &'static str {
        match self {
            Family::V4 => "A",
            Family::V6 => "AAAA",
        }
    }
}

pub(crate) fn join_names(names: &[String]) -> String {
    const SHOWN: usize = 5;
    let mut s = names.iter().take(SHOWN).cloned().collect::<Vec<_>>().join(", ");
    if names.len() > SHOWN {
        s.push_str(&format!(" and {} more", names.len() - SHOWN));
    }
    s
}

/// Is the IPv4 address the records publish this server's, and the one mail
/// really leaves from? `addrs.ipv4` is that address (`server_ip::resolve_ipv4`
/// has the rule), `addrs.source4` what the kernel would send from, and
/// `addrs.bind4` what Postfix is set to.
fn ipv4_address_item(addrs: &MailAddrs) -> MailCheckItem {
    const ID: &str = "address";
    const LABEL: &str = "Public IPv4 address";
    let Some(p) = addrs.ipv4.as_deref() else {
        return item(ID, LABEL, Status::Fail, "This server's public IPv4 address could not be worked out.");
    };
    let facts = &addrs.facts4;
    // Receivers judge mail that goes through a relay host by the relay's
    // address: what this server sends from does not matter then.
    let direct = addrs.relayhost.is_empty();
    if let Bind4::ByHand(set) = &addrs.bind4 {
        // A private address set by hand leaves through NAT: nothing to say.
        if set != p && direct && !mail_ip::is_private_v4(set) {
            return item(
                ID,
                LABEL,
                Status::Fail,
                format!(
                    "A and SPF records publish {p}, but Postfix is set by hand to send from {set} (smtp_bind_address in main.cf), and mail from {set} fails SPF. \
                     Point the panel hostname at {set}, or remove the setting: postconf -X smtp_bind_address && systemctl reload postfix. \
                     The panel does not change a setting made by hand."
                ),
            );
        }
    }
    if let (Some(gone), true) = (addrs.pin4_gone(), direct) {
        return item(
            ID,
            LABEL,
            Status::Fail,
            format!(
                "Postfix is still set to send from {gone}, which is no longer an address of this server, so mail sent over IPv4 can stay in the queue. \
                 Save the outgoing mail setting (Settings > Mail): the panel then releases that address. The address A and SPF records publish is {p}."
            ),
        );
    }
    if let Some(foreign) = facts.foreign() {
        return item(
            ID,
            LABEL,
            Status::Warn,
            format!(
                "{p} is on this server, and it is the address the panel writes into A and SPF records. \
                 The panel hostname {} resolves to {foreign}, which is not an address of this server: a proxy stands in front of the panel, or the name still points at another server. \
                 A record that still names {foreign} makes mail from this server fail SPF: run Reconcile on each domain.",
                facts.host
            ),
        );
    }
    match (&addrs.source4, &addrs.bind4) {
        (Ipv4Source::Pin { default }, Bind4::Ours(Some(at))) if direct && at == p => item(
            ID,
            LABEL,
            Status::Ok,
            format!(
                "{p} is on this server, and it is the address A and SPF records publish. \
                 Left alone this server would send from {default}, so Postfix is set to send from {p} (smtp_bind_address)."
            ),
        ),
        (Ipv4Source::Pin { default }, Bind4::ByHand(set)) if direct && set == p => item(
            ID,
            LABEL,
            Status::Ok,
            format!(
                "{p} is on this server, and it is the address A and SPF records publish. \
                 Left alone this server would send from {default}; Postfix is set by hand to send from {p} (smtp_bind_address)."
            ),
        ),
        (Ipv4Source::Pin { default }, _) if direct => item(
            ID,
            LABEL,
            Status::Fail,
            format!(
                "A and SPF records publish {p}, but this server sends from {default} unless told otherwise, and mail from {default} fails SPF. \
                 Save the outgoing mail setting (Settings > Mail): Postfix is then set to send from {p}."
            ),
        ),
        (Ipv4Source::CannotPin { default, why }, _) if direct => item(
            ID,
            LABEL,
            Status::Fail,
            format!(
                "A and SPF records publish {p}, but this server sends from {default}, and mail from {default} fails SPF. \
                 Postfix could not be set to send from {p}: {why}. Point the panel hostname at {default}, then run Reconcile on each domain."
            ),
        ),
        _ if facts.on_interface(p) => item(
            ID,
            LABEL,
            Status::Ok,
            format!("{p} is on this server, and it is the address A and SPF records publish."),
        ),
        _ => item(
            ID,
            LABEL,
            Status::Ok,
            format!("{p}. This server has no public address of its own (it is behind NAT), so the panel takes it from the panel hostname."),
        ),
    }
}

fn ipv6_address_item(addrs: &MailAddrs) -> MailCheckItem {
    const ID: &str = "address";
    const LABEL: &str = "Public IPv6 address";
    match (&addrs.ipv6, &addrs.ipv6_missing) {
        // Kept because Postfix is pinned to it, though the lookup found no
        // AAAA (mail_ip::mail_ipv6).
        (Some(a), _) if addrs.facts.published.is_none() => item(
            ID,
            LABEL,
            Status::Warn,
            format!(
                "{a} is on this server and Postfix sends over IPv6 from it, but this server found no AAAA record for the panel hostname {} just now.",
                addrs.facts.host
            ),
        ),
        (Some(a), _) => item(
            ID,
            LABEL,
            Status::Ok,
            format!(
                "{a} is on this server and is the address the panel hostname {} publishes. Postfix sends over IPv6 from this address only.",
                addrs.facts.host
            ),
        ),
        (None, Some(why)) => item(ID, LABEL, Status::Fail, why.clone()),
        (None, None) => item(ID, LABEL, Status::Fail, "This server has no public IPv6 address."),
    }
}

fn ptr_item(ptr: &Lookup, addr: &str, helo: &str) -> MailCheckItem {
    const ID: &str = "ptr";
    const LABEL: &str = "Reverse DNS (PTR)";
    match ptr {
        Lookup::Records(names) if names.iter().any(|n| n == helo) => item(
            ID,
            LABEL,
            Status::Ok,
            format!("{addr} has the reverse name {helo}, the name Postfix introduces itself with."),
        ),
        Lookup::Records(names) => item(
            ID,
            LABEL,
            Status::Warn,
            format!(
                "{addr} has the reverse name {}, not {helo}. Receivers trust a server more when the two match: \
                 set the reverse DNS of {addr} to {helo} at your hosting provider.",
                join_names(names)
            ),
        ),
        Lookup::Empty => item(
            ID,
            LABEL,
            Status::Fail,
            format!(
                "{addr} has no reverse DNS record. Gmail and others refuse mail from such an address (550 5.7.25). \
                 Set the reverse DNS of {addr} to {helo} at your hosting provider; the panel cannot do that for you."
            ),
        ),
        Lookup::Failed(why) => item(
            ID,
            LABEL,
            Status::Fail,
            format!("The reverse DNS of {addr} could not be looked up: {why}."),
        ),
    }
}

/// Does a reverse name resolve back to the address (forward-confirmed
/// reverse DNS)? `forward` holds the lookup of each reverse name.
fn fcrdns_item(forward: &[(String, Lookup)], addr: &str, helo: &str, family: Family) -> MailCheckItem {
    const ID: &str = "fcrdns";
    const LABEL: &str = "Reverse name resolves back";
    let rtype = family.rtype();
    if forward.is_empty() {
        return item(ID, LABEL, Status::Fail, format!("Nothing to check: {addr} has no reverse name."));
    }
    for (name, lookup) in forward {
        if matches!(lookup, Lookup::Records(a) if a.iter().any(|x| x == addr)) {
            return item(ID, LABEL, Status::Ok, format!("{name} resolves back to {addr}."));
        }
    }
    let (name, lookup) = &forward[0];
    let found = match lookup {
        Lookup::Records(a) => format!("it resolves to {}", join_names(a)),
        Lookup::Empty => format!("it has no {rtype} record"),
        Lookup::Failed(why) => format!("the lookup failed: {why}"),
    };
    item(
        ID,
        LABEL,
        Status::Fail,
        format!(
            "{name} does not resolve back to {addr} ({found}). Receivers that check this refuse the mail (Gmail: 550 5.7.25). \
             Set the reverse DNS of {addr} to {helo} at your hosting provider, or add the record {name} {rtype} {addr}."
        ),
    )
}

fn helo_item(lookup: &Lookup, helo: &str, addr: &str, family: Family) -> MailCheckItem {
    const ID: &str = "helo";
    const LABEL: &str = "HELO name resolves";
    let rtype = family.rtype();
    match lookup {
        Lookup::Records(a) if a.iter().any(|x| x == addr) => {
            item(ID, LABEL, Status::Ok, format!("{helo} resolves to {addr}."))
        }
        Lookup::Records(a) => item(
            ID,
            LABEL,
            Status::Fail,
            format!(
                "{helo}, the name Postfix introduces itself with, resolves to {}, not {addr}. Change its {rtype} record to {addr}.",
                join_names(a)
            ),
        ),
        Lookup::Empty => item(
            ID,
            LABEL,
            Status::Fail,
            format!(
                "{helo}, the name Postfix introduces itself with, has no {rtype} record. Add the record {helo} {rtype} {addr}, \
                 or set a panel hostname that has one (Settings > Panel hostname)."
            ),
        ),
        Lookup::Failed(why) => item(
            ID,
            LABEL,
            Status::Fail,
            format!("{helo} could not be looked up: {why}."),
        ),
    }
}

/// SPF of every mail domain against one address. `results` pairs each domain
/// with its outcome; a domain missing from it was not reached in time.
/// `held` are the domains Postfix keeps off this address meanwhile
/// (`mail_ip::held_domains`).
fn spf_sweep_item(
    domains: &[String],
    results: &[(String, Spf)],
    addr: &str,
    held: &[String],
) -> MailCheckItem {
    const ID: &str = "spf";
    const LABEL: &str = "SPF covers the address";
    if domains.is_empty() {
        return item(ID, LABEL, Status::Ok, "No mail domains yet.");
    }
    let mut refused: Vec<String> = Vec::new();
    let mut unknown: Vec<String> = Vec::new();
    for d in domains {
        match results.iter().find(|(name, _)| name == d).map(|(_, r)| r) {
            Some(Spf::Pass) => {}
            Some(Spf::NoRecord) | Some(Spf::Unknown(_)) | None => unknown.push(d.clone()),
            Some(_) => refused.push(d.clone()),
        }
    }
    if !refused.is_empty() {
        let those = if refused.len() == 1 { "that domain" } else { "those domains" };
        // Held to IPv4, their mail never leaves from this address: not a
        // reason to keep every other domain off it.
        if refused.iter().all(|d| held.contains(d)) {
            return item(
                ID,
                LABEL,
                Status::Warn,
                format!(
                    "The SPF record of {} does not allow mail from {addr}. Until it does, Postfix sends mail from {those} over IPv4 only. \
                     Run the DNS check of {those} for the record to publish.",
                    join_names(&refused),
                ),
            );
        }
        return item(
            ID,
            LABEL,
            Status::Fail,
            format!(
                "The SPF record of {} does not allow mail from {addr}, so receivers refuse or distrust it. \
                 Run the DNS check of {those} for the record to publish.",
                join_names(&refused),
            ),
        );
    }
    if !unknown.is_empty() {
        return item(
            ID,
            LABEL,
            Status::Warn,
            format!(
                "No SPF record could be read for {}. Run the DNS check of {} to see why.",
                join_names(&unknown),
                if unknown.len() == 1 { "that domain" } else { "those domains" }
            ),
        );
    }
    item(
        ID,
        LABEL,
        Status::Ok,
        format!(
            "The SPF record of {} allows mail from {addr}.",
            if domains.len() == 1 {
                domains[0].clone()
            } else {
                format!("all {} mail domains", domains.len())
            }
        ),
    )
}

/// The SPF record of each domain checked against `ip`, a few at a time and
/// for SPF_SWEEP_TIMEOUT at most; a domain not reached by then is left out.
pub(crate) async fn spf_sweep(r: &dyn Resolver, domains: &[String], ip: IpAddr) -> Vec<(String, Spf)> {
    let mut results: Vec<(String, Spf)> = Vec::new();
    let mut stream = futures::stream::iter(domains.iter().cloned())
        .map(|d| async move {
            let result = spf_check(r, &d, ip).await.result;
            (d, result)
        })
        .buffer_unordered(6);
    let deadline = tokio::time::sleep(SPF_SWEEP_TIMEOUT);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            next = stream.next() => match next {
                Some(pair) => results.push(pair),
                None => break,
            },
            _ = &mut deadline => break,
        }
    }
    results
}

/// The checks for one protocol. `address_item` is the finding about the
/// address itself; without an address nothing else can be checked. `held`:
/// see `spf_sweep_item`.
async fn family_check(
    r: &dyn Resolver,
    family: Family,
    address: Option<String>,
    address_item: MailCheckItem,
    helo: &str,
    domains: &Result<Vec<String>, String>,
    held: &[String],
) -> MailIpFamilyCheck {
    let mut checks = vec![address_item];
    if let Some(addr) = address.as_deref() {
        let ptr = r.lookup(addr, "PTR").await;
        let mut forward: Vec<(String, Lookup)> = Vec::new();
        if let Lookup::Records(names) = &ptr {
            for name in names.iter().take(3) {
                forward.push((name.clone(), r.lookup(name, family.rtype()).await));
            }
        }
        checks.push(ptr_item(&ptr, addr, helo));
        checks.push(fcrdns_item(&forward, addr, helo, family));
        checks.push(helo_item(&r.lookup(helo, family.rtype()).await, helo, addr, family));
        checks.push(match (domains, addr.parse::<IpAddr>()) {
            (Ok(domains), Ok(ip)) => {
                let results = spf_sweep(r, domains, ip).await;
                spf_sweep_item(domains, &results, addr, held)
            }
            (Err(why), _) => item(
                "spf",
                "SPF covers the address",
                Status::Warn,
                format!("The mail domains could not be read: {why}."),
            ),
            (_, Err(_)) => item("spf", "SPF covers the address", Status::Warn, "Not checked."),
        });
    }
    let ready = is_ready(address.as_deref(), &checks);
    MailIpFamilyCheck { address, ready, checks }
}

/// `family_check` with an overall limit, so a slow network cannot hold the
/// request open.
async fn bounded_family_check(
    r: &dyn Resolver,
    family: Family,
    address: Option<String>,
    address_item: MailCheckItem,
    helo: &str,
    domains: &Result<Vec<String>, String>,
    held: &[String],
) -> MailIpFamilyCheck {
    let run = family_check(r, family, address.clone(), address_item.clone(), helo, domains, held);
    match tokio::time::timeout(CHECK_TIMEOUT, run).await {
        Ok(check) => check,
        Err(_) => MailIpFamilyCheck {
            address,
            ready: false,
            checks: vec![
                address_item,
                item("dns", "DNS lookups", Status::Fail, "The lookups took too long. Try again in a moment."),
            ],
        },
    }
}

async fn mail_domain_names(state: &AppState) -> Result<Vec<String>, String> {
    let mut conn = mail_conn(state).await.map_err(|e| e.to_string())?;
    conn.query("SELECT name FROM virtual_domains ORDER BY name")
        .await
        .map_err(|e| e.to_string())
}

/// Is IPv6 fit to send from? The settings endpoints ask this before they
/// let IPv6 be chosen, and the agent before it switches IPv6 on by itself.
/// `dig` keeps the answers for the caller's own lookups afterwards.
pub(crate) async fn ipv6_check(state: &AppState, addrs: &MailAddrs, dig: &Dig) -> MailIpFamilyCheck {
    let domains = mail_domain_names(state).await;
    bounded_family_check(
        dig,
        Family::V6,
        addrs.ipv6.clone(),
        ipv6_address_item(addrs),
        &addrs.helo,
        &domains,
        &addrs.held,
    )
    .await
}

/// GET /mail/ip-check
pub async fn ip_check(State(state): State<AppState>) -> AgentResult<Json<MailIpCheck>> {
    let addrs = mail_ip::mail_addrs(&state).await;
    let domains = mail_domain_names(&state).await;
    let v4_item = ipv4_address_item(&addrs);
    let dig = Dig::default();
    let (ipv4, ipv6) = tokio::join!(
        bounded_family_check(&dig, Family::V4, addrs.ipv4.clone(), v4_item, &addrs.helo, &domains, &[]),
        bounded_family_check(
            &dig,
            Family::V6,
            addrs.ipv6.clone(),
            ipv6_address_item(&addrs),
            &addrs.helo,
            &domains,
            &addrs.held,
        ),
    );
    Ok(Json(MailIpCheck {
        helo_host: addrs.helo,
        ipv4,
        ipv6,
    }))
}

// ---------------------------------------------------------------------------
// GET /mail/domains/{name}/dns-check
// ---------------------------------------------------------------------------

/// What a domain's records are checked against.
struct DomainCtx<'a> {
    domain: &'a str,
    /// The addresses mail leaves from, and that the records should name.
    ipv4: Option<&'a str>,
    ipv6: Option<&'a str>,
    /// Every global IPv6 address of this server: an AAAA for any of them
    /// still reaches it.
    own_ipv6s: &'a [String],
    /// The zone's NS records in this panel's PowerDNS; None when the zone is
    /// not hosted here.
    zone_ns: Option<&'a [String]>,
    /// A zone hosted here that the domain is a name in.
    parent_zone: Option<&'a str>,
    /// The public key Rspamd signs this domain's mail with.
    dkim_key: Option<&'a str>,
    /// The address the panel hostname resolves to when that is not this
    /// server (`Ipv4Facts::foreign`). Records written before the panel told
    /// the two apart carry it.
    foreign_v4: Option<&'a str>,
    /// How Postfix sends this domain's mail while it sends over IPv6; None
    /// while it does not.
    ipv6_route: Option<Ipv6Route>,
}

/// What Postfix's per-domain table does with a domain (mail_ip.rs).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Ipv6Route {
    /// Held to IPv4: its SPF record did not allow the IPv6 address when the
    /// table was built.
    Held,
    /// Set to IPv6 with force: never held.
    Forced,
    /// Over IPv6 whenever the server default or the domain's choice says so,
    /// and as the fallback otherwise.
    Free,
}

impl DomainCtx<'_> {
    /// What to do about a wrong or missing record. `record` is the record
    /// as it should be.
    fn fix(&self, managed: bool, record: &str) -> String {
        let d = self.domain;
        if managed {
            format!(
                "Run Reconcile on {d} (Domains > {d}) to set it: {record}. Public DNS can take up to an hour to follow."
            )
        } else if let Some(parent) = self.parent_zone {
            format!("Add this record in the zone {parent} (Domains > {parent}): {record}")
        } else {
            format!("Add this record where the DNS of {d} is hosted: {record}")
        }
    }

    /// Where a surplus record is removed.
    fn remove_at(&self, managed: bool) -> String {
        let d = self.domain;
        if managed {
            format!("under Domains > {d} > DNS records")
        } else if let Some(parent) = self.parent_zone {
            format!("under Domains > {parent} > DNS records")
        } else {
            format!("where the DNS of {d} is hosted")
        }
    }
}

fn mx_item(mx: &Lookup, c: &DomainCtx<'_>, managed: bool) -> MailCheckItem {
    const ID: &str = "mx";
    const LABEL: &str = "MX record";
    let d = c.domain;
    match mx {
        Lookup::Records(hosts) => item(ID, LABEL, Status::Ok, format!("Mail for {d} goes to {}.", join_names(hosts))),
        Lookup::Empty => item(
            ID,
            LABEL,
            Status::Fail,
            format!(
                "{d} has no MX record, so other servers do not know where to deliver its mail. {}",
                c.fix(managed, &format!("{d} MX 10 mail.{d}"))
            ),
        ),
        Lookup::Failed(why) => item(ID, LABEL, Status::Fail, format!("The MX record of {d} could not be looked up: {why}.")),
    }
}

fn a_item(a: &Lookup, host: &str, c: &DomainCtx<'_>, managed: bool) -> MailCheckItem {
    const ID: &str = "a";
    const LABEL: &str = "Mail host A record";
    let Some(v4) = c.ipv4 else {
        return item(ID, LABEL, Status::Warn, "Not checked: this server's IPv4 address is not known.");
    };
    let record = format!("{host} A {v4}");
    match a {
        Lookup::Records(addrs) if addrs.iter().any(|x| x == v4) => {
            item(ID, LABEL, Status::Ok, format!("{host} resolves to {v4}, this server."))
        }
        Lookup::Records(addrs) => {
            let whose = match c.foreign_v4 {
                Some(foreign) if addrs.iter().any(|x| x == foreign) => {
                    ", the address the panel hostname has, and that is not this server"
                }
                _ => ", not to this server",
            };
            item(
                ID,
                LABEL,
                Status::Fail,
                format!(
                    "{host} resolves to {}{whose} ({v4}): mail for {} is delivered somewhere else. {}",
                    join_names(addrs),
                    c.domain,
                    c.fix(managed, &record)
                ),
            )
        }
        Lookup::Empty => item(
            ID,
            LABEL,
            Status::Fail,
            format!("{host} has no A record, so mail for {} cannot be delivered. {}", c.domain, c.fix(managed, &record)),
        ),
        Lookup::Failed(why) => item(ID, LABEL, Status::Fail, format!("{host} could not be looked up: {why}.")),
    }
}

fn aaaa_item(aaaa: &Lookup, host: &str, c: &DomainCtx<'_>, managed: bool) -> MailCheckItem {
    const ID: &str = "aaaa";
    const LABEL: &str = "Mail host AAAA record";
    match aaaa {
        Lookup::Records(addrs) => {
            if let Some(mine) = addrs
                .iter()
                .find(|a| Some(a.as_str()) == c.ipv6 || c.own_ipv6s.contains(*a))
            {
                return item(ID, LABEL, Status::Ok, format!("{host} resolves to {mine}, this server."));
            }
            let found = join_names(addrs);
            match c.ipv6 {
                Some(v6) => item(
                    ID,
                    LABEL,
                    Status::Fail,
                    format!(
                        "{host} has an AAAA record for {found}, which is not this server ({v6}): senders that try IPv6 first cannot deliver. {}",
                        c.fix(managed, &format!("{host} AAAA {v6}"))
                    ),
                ),
                None => item(
                    ID,
                    LABEL,
                    Status::Fail,
                    format!(
                        "{host} has an AAAA record for {found}, but that is not an address of this server: senders that try IPv6 first cannot deliver. Remove it {}.",
                        c.remove_at(managed)
                    ),
                ),
            }
        }
        Lookup::Empty => match c.ipv6 {
            Some(v6) => item(
                ID,
                LABEL,
                Status::Warn,
                format!(
                    "{host} has no AAAA record, so mail reaches this server over IPv4 only. {}",
                    c.fix(managed, &format!("{host} AAAA {v6}"))
                ),
            ),
            None => item(
                ID,
                LABEL,
                Status::Ok,
                "No AAAA record, and none is needed: this server does not use IPv6 for mail.",
            ),
        },
        Lookup::Failed(why) => item(ID, LABEL, Status::Warn, format!("{host} could not be looked up: {why}.")),
    }
}

fn spf_item(v4: Option<&SpfOutcome>, v6: Option<&SpfOutcome>, c: &DomainCtx<'_>, managed: bool) -> MailCheckItem {
    const ID: &str = "spf";
    const LABEL: &str = "SPF record";
    let d = c.domain;
    let wanted = crate::handlers::dns::format_spf(c.ipv4, c.ipv6);
    let record = format!("{d} TXT {wanted}");
    let Some(first) = v4.or(v6) else {
        return item(ID, LABEL, Status::Warn, "Not checked: this server's addresses are not known.");
    };
    let found = first.record.clone().unwrap_or_default();
    match &first.result {
        Spf::NoRecord => {
            return item(
                ID,
                LABEL,
                Status::Fail,
                format!(
                    "{d} has no SPF record, so receivers cannot tell that this server may send its mail. {}",
                    c.fix(managed, &record)
                ),
            )
        }
        Spf::Multiple => {
            return item(
                ID,
                LABEL,
                Status::Fail,
                format!(
                    "{d} has more than one SPF record, and receivers then use none of them. Keep one ({wanted}) and remove the others {}.",
                    c.remove_at(managed)
                ),
            )
        }
        Spf::Unknown(why) => {
            return item(ID, LABEL, Status::Warn, format!("The SPF record of {d} could not be checked: {why}."))
        }
        Spf::Broken(why) => {
            return item(
                ID,
                LABEL,
                Status::Fail,
                format!("Receivers cannot use the SPF record of {d} (\"{found}\"): {why}. {}", c.fix(managed, &record)),
            )
        }
        _ => {}
    }
    for (outcome, addr) in [(v4, c.ipv4), (v6, c.ipv6)] {
        let (Some(outcome), Some(addr)) = (outcome, addr) else { continue };
        match &outcome.result {
            Spf::Pass => {}
            Spf::Unknown(why) => {
                return item(ID, LABEL, Status::Warn, format!("The SPF record of {d} could not be checked for {addr}: {why}."))
            }
            other => {
                let names_foreign = c
                    .foreign_v4
                    .filter(|f| found.split_whitespace().any(|term| term.strip_prefix("ip4:") == Some(*f)));
                let whose = match names_foreign {
                    Some(f) => format!(
                        " It names {f}, the address the panel hostname has, and that is not this server."
                    ),
                    None => String::new(),
                };
                return item(
                    ID,
                    LABEL,
                    Status::Fail,
                    format!(
                        "The SPF record of {d} (\"{found}\") does not allow mail from {addr} ({}).{whose} {}",
                        spf_words(other),
                        c.fix(managed, &record)
                    ),
                )
            }
        }
    }
    let allowed: Vec<&str> = [c.ipv4, c.ipv6].into_iter().flatten().collect();
    item(ID, LABEL, Status::Ok, format!("\"{found}\" allows mail from {}.", allowed.join(" and ")))
}

/// What Postfix does with the domain's mail while its SPF record and the
/// IPv6 address do not agree; None when there is nothing to say. `spf6` is
/// the record checked against the IPv6 address just now.
fn ipv6_route_item(spf6: Option<&SpfOutcome>, c: &DomainCtx<'_>) -> Option<MailCheckItem> {
    const ID: &str = "ipv6";
    const LABEL: &str = "Outgoing mail over IPv6";
    const SAVE: &str = "save the outgoing mail setting again (Settings > Mail)";
    let (d, v6) = (c.domain, c.ipv6?);
    let result = spf6.map(|o| &o.result);
    let allowed = matches!(result, Some(Spf::Pass) | Some(Spf::NoRecord));
    let unknown = matches!(result, Some(Spf::Unknown(_)) | None);
    let detail = match c.ipv6_route? {
        Ipv6Route::Held if allowed => format!(
            "Mail from {d} still goes out over IPv4 only: when Postfix was last set up, its SPF record did not allow mail from {v6}. \
             Nothing stands in the way now: {SAVE} and {d} can use IPv6 like the other domains."
        ),
        Ipv6Route::Held if unknown => format!(
            "Mail from {d} goes out over IPv4 only: its SPF record is not known to allow mail from {v6}, and it could not be looked up just now. \
             It is looked up again when the outgoing mail setting is saved (Settings > Mail) and when the panel's agent starts."
        ),
        Ipv6Route::Held => format!(
            "Mail from {d} goes out over IPv4 only, because its SPF record does not allow mail from {v6}. \
             Publish the SPF record shown above, then {SAVE}."
        ),
        Ipv6Route::Forced if !allowed && !unknown => format!(
            "{d} is set to IPv6 with \"Use IPv6 anyway\", so its mail goes out over IPv6 although its SPF record does not allow mail from {v6}. \
             Publish the SPF record shown above, or choose another protocol for the domain."
        ),
        Ipv6Route::Free if !allowed && !unknown => format!(
            "The SPF record of {d} does not allow mail from {v6}, and Postfix does not keep its mail on IPv4 yet. \
             Publish the SPF record shown above, or {SAVE}: until the record is right, {d} is then sent over IPv4 only."
        ),
        _ => return None,
    };
    Some(item(ID, LABEL, Status::Warn, detail))
}

/// The `p=` value of a DKIM record, without whitespace.
fn dkim_public_key(txt: &str) -> Option<String> {
    txt.split(';')
        .filter_map(|tag| tag.split_once('='))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("p"))
        .map(|(_, v)| v.chars().filter(|c| !c.is_whitespace()).collect())
}

fn dkim_item(txt: &Lookup, c: &DomainCtx<'_>, managed: bool) -> MailCheckItem {
    const ID: &str = "dkim";
    const LABEL: &str = "DKIM record";
    let d = c.domain;
    let name = format!("{DKIM_SELECTOR}._domainkey.{d}");
    let Some(key) = c.dkim_key else {
        return item(
            ID,
            LABEL,
            Status::Fail,
            format!("This server has no DKIM key for {d} (/var/lib/rspamd/dkim/{d}.{DKIM_SELECTOR}.key), so its mail goes out unsigned."),
        );
    };
    let record = format!("{name} TXT \"v=DKIM1; k=rsa; p={key}\"");
    match txt {
        Lookup::Records(txts) => {
            let keys: Vec<String> = txts.iter().filter_map(|t| dkim_public_key(t)).collect();
            if keys.iter().any(|k| k == key) {
                item(ID, LABEL, Status::Ok, format!("{name} publishes the key this server signs with."))
            } else {
                item(
                    ID,
                    LABEL,
                    Status::Fail,
                    format!(
                        "{name} does not publish the key this server signs with, so receivers reject the signature. {}",
                        c.fix(managed, &record)
                    ),
                )
            }
        }
        Lookup::Empty => item(
            ID,
            LABEL,
            Status::Fail,
            format!(
                "{name} has no DKIM record, so receivers cannot verify this server's signature. {}",
                c.fix(managed, &record)
            ),
        ),
        Lookup::Failed(why) => item(ID, LABEL, Status::Warn, format!("{name} could not be looked up: {why}.")),
    }
}

fn dmarc_item(txt: &Lookup, c: &DomainCtx<'_>, managed: bool) -> MailCheckItem {
    const ID: &str = "dmarc";
    const LABEL: &str = "DMARC record";
    let d = c.domain;
    let name = format!("_dmarc.{d}");
    let record = format!("{name} TXT \"v=DMARC1; p=none; rua=mailto:postmaster@{d}\"");
    let found: Vec<&String> = match txt {
        Lookup::Records(txts) => txts
            .iter()
            .filter(|t| t.trim_start().to_ascii_lowercase().starts_with("v=dmarc1"))
            .collect(),
        Lookup::Empty => Vec::new(),
        Lookup::Failed(why) => {
            return item(ID, LABEL, Status::Warn, format!("{name} could not be looked up: {why}."))
        }
    };
    match found.as_slice() {
        [one] => item(ID, LABEL, Status::Ok, format!("\"{one}\"")),
        [] => item(
            ID,
            LABEL,
            Status::Warn,
            format!(
                "{d} has no DMARC record. Gmail and Yahoo expect one from anyone who sends them much mail. {}",
                c.fix(managed, &record)
            ),
        ),
        _ => item(
            ID,
            LABEL,
            Status::Fail,
            format!(
                "{name} has more than one DMARC record, and receivers then use none of them. Keep one and remove the others {}.",
                c.remove_at(managed)
            ),
        ),
    }
}

/// Every check for one domain. Returns whether Reconcile can repair it, and
/// the items.
async fn domain_checks(r: &dyn Resolver, c: &DomainCtx<'_>) -> (bool, Vec<MailCheckItem>) {
    let d = c.domain;
    let mut checks: Vec<MailCheckItem> = Vec::new();

    // A zone in the panel only helps when the domain is delegated to it.
    let mut managed = false;
    if let Some(local) = c.zone_ns {
        managed = true;
        if let Lookup::Records(public) = r.lookup(d, "NS").await {
            if !public.iter().any(|ns| local.contains(ns)) {
                managed = false;
                checks.push(item(
                    "ns",
                    "Name servers",
                    Status::Warn,
                    format!(
                        "{d} is a zone in this panel, but its name servers are {}, not {}. The internet does not see the records kept here: \
                         set them where the DNS of {d} is hosted, or point the domain at this server's name servers at the registrar.",
                        join_names(&public),
                        if local.is_empty() { "this server's".to_string() } else { join_names(local) }
                    ),
                ));
            }
        }
    }

    let mail_host = format!("mail.{d}");
    let dkim_name = format!("{DKIM_SELECTOR}._domainkey.{d}");
    let dmarc_name = format!("_dmarc.{d}");
    let mx = r.lookup(d, "MX").await;
    // Without an MX the host the panel would point it at is checked, so
    // everything that is missing shows up in one run.
    let host = match &mx {
        Lookup::Records(hosts) if !hosts.is_empty() => hosts[0].clone(),
        _ => mail_host,
    };
    let v4 = c.ipv4.and_then(|a| a.parse::<IpAddr>().ok());
    let v6 = c.ipv6.and_then(|a| a.parse::<IpAddr>().ok());
    let (a, aaaa, spf4, spf6, dkim, dmarc) = tokio::join!(
        r.lookup(&host, "A"),
        r.lookup(&host, "AAAA"),
        async {
            match v4 {
                Some(ip) => Some(spf_check(r, d, ip).await),
                None => None,
            }
        },
        async {
            match v6 {
                Some(ip) => Some(spf_check(r, d, ip).await),
                None => None,
            }
        },
        r.lookup(&dkim_name, "TXT"),
        r.lookup(&dmarc_name, "TXT"),
    );

    checks.push(mx_item(&mx, c, managed));
    checks.push(a_item(&a, &host, c, managed));
    checks.push(aaaa_item(&aaaa, &host, c, managed));
    checks.push(spf_item(spf4.as_ref(), spf6.as_ref(), c, managed));
    checks.extend(ipv6_route_item(spf6.as_ref(), c));
    checks.push(dkim_item(&dkim, c, managed));
    checks.push(dmarc_item(&dmarc, c, managed));
    (managed, checks)
}

/// GET /mail/domains/{name}/dns-check
pub async fn domain_dns_check(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> AgentResult<Json<MailDomainDnsCheck>> {
    let domain = name.trim().trim_end_matches('.').to_lowercase();
    let mut conn = mail_conn(&state).await?;
    let known: Option<(String, u8)> = conn
        .exec_first(
            "SELECT ip_preference, ip_forced FROM virtual_domains WHERE name = ?",
            (&domain,),
        )
        .await?;
    drop(conn);
    let Some((preference, forced)) = known else {
        return Err(AgentError::NotFound);
    };

    let addrs = mail_ip::mail_addrs(&state).await;
    let ipv6_route = addrs.sends_ipv6.then(|| {
        if forced != 0 && mail_ip::Preference::from_db(&preference) == mail_ip::Preference::Ipv6 {
            Ipv6Route::Forced
        } else if addrs.held.contains(&domain) {
            Ipv6Route::Held
        } else {
            Ipv6Route::Free
        }
    });
    let zone_ns = crate::handlers::dns::zone_ns(&state, &domain).await;
    let parent_zone = match zone_ns {
        Some(_) => None,
        None => crate::handlers::dns::find_enclosing_zone(&state, &domain)
            .await
            .map(|(zone, _)| zone),
    };
    let dkim_key = crate::handlers::dns::read_dkim_pubkey(&domain).await.ok();
    let ctx = DomainCtx {
        domain: &domain,
        ipv4: addrs.ipv4.as_deref(),
        ipv6: addrs.ipv6.as_deref(),
        own_ipv6s: &addrs.facts.on_interfaces,
        zone_ns: zone_ns.as_deref(),
        parent_zone: parent_zone.as_deref(),
        dkim_key: dkim_key.as_deref(),
        foreign_v4: addrs.facts4.foreign(),
        ipv6_route,
    };
    let dig = Dig::default();
    let (managed, checks) = match tokio::time::timeout(CHECK_TIMEOUT, domain_checks(&dig, &ctx)).await {
        Ok(done) => done,
        Err(_) => (
            zone_ns.is_some(),
            vec![item("dns", "DNS lookups", Status::Fail, "The lookups took too long. Try again in a moment.")],
        ),
    };
    Ok(Json(MailDomainDnsCheck {
        domain,
        managed,
        checks,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server_ip::{Ipv4Facts, Ipv6Facts};

    /// Public DNS as a table: (name, type) -> answer; anything else is empty.
    #[derive(Default)]
    struct Table(HashMap<(String, String), Lookup>);

    impl Table {
        fn with(mut self, name: &str, rtype: &str, records: &[&str]) -> Self {
            self.0.insert(
                (name.to_string(), rtype.to_string()),
                Lookup::Records(records.iter().map(|s| s.to_string()).collect()),
            );
            self
        }
        fn failing(mut self, name: &str, rtype: &str) -> Self {
            self.0.insert(
                (name.to_string(), rtype.to_string()),
                Lookup::Failed("no resolver".to_string()),
            );
            self
        }
    }

    impl Resolver for Table {
        fn lookup<'a>(&'a self, name: &'a str, rtype: &'a str) -> BoxFut<'a, Lookup> {
            let answer = self
                .0
                .get(&(name.to_ascii_lowercase(), rtype.to_string()))
                .cloned()
                .unwrap_or(Lookup::Empty);
            Box::pin(async move { answer })
        }
    }

    const V4: &str = "198.51.100.127";
    const V6: &str = "2001:db8:1::1";

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn status_of<'a>(checks: &'a [MailCheckItem], id: &str) -> (Status, &'a str) {
        let c = checks.iter().find(|c| c.id == id).unwrap_or_else(|| panic!("no {id} item"));
        (c.status, c.detail.as_str())
    }

    #[test]
    fn dig_output_is_read_per_record_type() {
        assert_eq!(parse_dig_short("198.51.100.127\n", "A"), vec![V4]);
        // A CNAME on the way prints its target first.
        assert_eq!(parse_dig_short("example.com.\n198.51.100.127\n", "A"), vec![V4]);
        assert_eq!(parse_dig_short("2001:DB8:1:0::1\n", "AAAA"), vec![V6]);
        assert_eq!(
            parse_dig_short("20 backup.example.com.\n10 Mail.Example.com.\n0 .\n", "MX"),
            vec!["mail.example.com", "backup.example.com"]
        );
        assert_eq!(parse_dig_short("panel.example.com.\n", "PTR"), vec!["panel.example.com"]);
        // Classless reverse delegation answers with a CNAME into .arpa first.
        assert_eq!(
            parse_dig_short("127.0-127.100.51.198.in-addr.arpa.\npanel.example.com.\n", "PTR"),
            vec!["panel.example.com"]
        );
        assert_eq!(
            parse_dig_short("\"v=spf1 mx -all\"\n\"google-site-verification=abc\"\n", "TXT"),
            vec!["v=spf1 mx -all", "google-site-verification=abc"]
        );
        assert!(parse_dig_short(";; communications error to 1.1.1.1#53: timed out\n", "A").is_empty());
        assert!(parse_dig_short("", "TXT").is_empty());
    }

    #[test]
    fn a_long_txt_record_is_put_back_together() {
        assert_eq!(
            unquote_txt("\"v=DKIM1; k=rsa; p=MIIB\" \"IjANBg\""),
            "v=DKIM1; k=rsa; p=MIIBIjANBg"
        );
        assert_eq!(unquote_txt("\"a \\\"b\\\" \\092 c\""), "a \"b\" \\ c");
    }

    #[test]
    fn spf_terms() {
        assert_eq!(
            parse_term("-all"),
            Some(Term::Mechanism { qualifier: '-', name: "all".into(), arg: None, prefix: None, prefix6: None })
        );
        assert_eq!(
            parse_term("ip6:2001:db8::/32"),
            Some(Term::Mechanism {
                qualifier: '+',
                name: "ip6".into(),
                arg: Some("2001:db8::".into()),
                prefix: Some(32),
                prefix6: None
            })
        );
        assert_eq!(
            parse_term("~a:mail.example.com/24//64"),
            Some(Term::Mechanism {
                qualifier: '~',
                name: "a".into(),
                arg: Some("mail.example.com".into()),
                prefix: Some(24),
                prefix6: Some(64)
            })
        );
        assert_eq!(parse_term("redirect=_spf.example.net"), Some(Term::Redirect("_spf.example.net".into())));
        assert_eq!(parse_term("exp=explain.example.com"), Some(Term::Modifier));
        assert_eq!(parse_term("ip4:198.51.100.0/x"), None);
    }

    #[test]
    fn cidr_matching() {
        assert!(in_cidr(ip("198.51.100.127"), ip("198.51.100.0"), 24));
        assert!(!in_cidr(ip("198.51.101.1"), ip("198.51.100.0"), 24));
        assert!(in_cidr(ip("198.51.100.127"), ip("0.0.0.0"), 0));
        assert!(in_cidr(ip("2001:db8:1::1"), ip("2001:db8::"), 32));
        assert!(!in_cidr(ip("2001:db9::1"), ip("2001:db8::"), 32));
        assert!(!in_cidr(ip("198.51.100.127"), ip("2001:db8::"), 0));
    }

    #[tokio::test]
    async fn the_panels_spf_record_covers_what_it_names_and_nothing_else() {
        // A zone written before the server had IPv6: no ip6, mail host has no AAAA.
        let dns = Table::default()
            .with("example.com", "TXT", &["v=spf1 mx ip4:198.51.100.127 -all"])
            .with("example.com", "MX", &["mail.example.com"])
            .with("mail.example.com", "A", &[V4]);
        assert_eq!(spf_check(&dns, "example.com", ip(V4)).await.result, Spf::Pass);
        let v6 = spf_check(&dns, "Example.com", ip(V6)).await;
        assert_eq!(v6.result, Spf::Fail);
        assert_eq!(v6.record.as_deref(), Some("v=spf1 mx ip4:198.51.100.127 -all"));

        // After Reconcile on a server with a published IPv6 address.
        let dns = Table::default()
            .with("example.com", "TXT", &["v=spf1 mx ip4:198.51.100.127 ip6:2001:db8:1::1 -all"])
            .with("example.com", "MX", &["mail.example.com"]);
        assert_eq!(spf_check(&dns, "example.com", ip(V6)).await.result, Spf::Pass);
        assert_eq!(spf_check(&dns, "example.com", ip("2001:db8:1::2")).await.result, Spf::Fail);

        // The mail host's AAAA alone covers the address through `mx`.
        let dns = Table::default()
            .with("example.com", "TXT", &["v=spf1 mx -all"])
            .with("example.com", "MX", &["mail.example.com"])
            .with("mail.example.com", "AAAA", &[V6]);
        assert_eq!(spf_check(&dns, "example.com", ip(V6)).await.result, Spf::Pass);
    }

    #[tokio::test]
    async fn spf_includes_redirects_and_broken_records() {
        let dns = Table::default()
            .with("a.example", "TXT", &["v=spf1 include:_spf.b.example ~all"])
            .with("_spf.b.example", "TXT", &["v=spf1 ip4:198.51.100.0/24 -all"])
            .with("r.example", "TXT", &["v=spf1 redirect=_spf.b.example"])
            .with("two.example", "TXT", &["v=spf1 -all", "v=spf1 mx -all"])
            .with("none.example", "TXT", &["some-verification=1"])
            .with("bad.example", "TXT", &["v=spf1 include:missing.example -all"])
            .with("typo.example", "TXT", &["v=spf1 ipv4:198.51.100.127 -all"])
            .with("neutral.example", "TXT", &["v=spf1 ?all"]);
        assert_eq!(spf_check(&dns, "a.example", ip(V4)).await.result, Spf::Pass);
        // The include does not match: its -all is not the answer, ~all is.
        assert_eq!(spf_check(&dns, "a.example", ip("203.0.113.9")).await.result, Spf::SoftFail);
        assert_eq!(spf_check(&dns, "r.example", ip(V4)).await.result, Spf::Pass);
        assert_eq!(spf_check(&dns, "r.example", ip(V6)).await.result, Spf::Fail);
        assert_eq!(spf_check(&dns, "two.example", ip(V4)).await.result, Spf::Multiple);
        assert_eq!(spf_check(&dns, "none.example", ip(V4)).await.result, Spf::NoRecord);
        assert_eq!(spf_check(&dns, "nowhere.example", ip(V4)).await.result, Spf::NoRecord);
        assert!(matches!(spf_check(&dns, "bad.example", ip(V4)).await.result, Spf::Broken(_)));
        assert!(matches!(spf_check(&dns, "typo.example", ip(V4)).await.result, Spf::Broken(_)));
        assert_eq!(spf_check(&dns, "neutral.example", ip(V4)).await.result, Spf::Neutral);
    }

    #[tokio::test]
    async fn spf_stops_at_ten_lookups_and_reports_failed_ones() {
        let mut dns = Table::default();
        for i in 0..12 {
            dns = dns.with(&format!("l{i}.example"), "TXT", &[&format!("v=spf1 include:l{}.example -all", i + 1)]);
        }
        assert!(matches!(spf_check(&dns, "l0.example", ip(V4)).await.result, Spf::Broken(_)));

        let dns = Table::default()
            .with("example.com", "TXT", &["v=spf1 mx -all"])
            .failing("example.com", "MX");
        assert!(matches!(spf_check(&dns, "example.com", ip(V4)).await.result, Spf::Unknown(_)));
        let dns = Table::default().failing("example.com", "TXT");
        assert!(matches!(spf_check(&dns, "example.com", ip(V4)).await.result, Spf::Unknown(_)));
    }

    /// What the IPv4 address item is told. `interfaces`: device and address;
    /// connections leave from the first.
    fn v4_addrs(
        named: &str,
        interfaces: &[(&str, &str)],
        ipv4: Option<&str>,
        source4: Ipv4Source,
        bind4: Bind4,
    ) -> MailAddrs {
        let facts4 = Ipv4Facts {
            host: "panel.example.com".into(),
            named: vec![named.to_string()],
            interfaces: interfaces
                .iter()
                .map(|(dev, ip)| (dev.to_string(), ip.to_string()))
                .collect(),
            default_source: interfaces.first().map(|(_, ip)| ip.to_string()),
        };
        MailAddrs {
            helo: "panel.example.com".into(),
            ipv4: ipv4.map(str::to_string),
            ipv6: None,
            facts: Ipv6Facts::default(),
            has_ipv4: !facts4.interfaces.is_empty(),
            facts4,
            ipv6_missing: None,
            source4,
            bind4,
            relayhost: String::new(),
            sends_ipv6: false,
            held: Vec::new(),
        }
    }

    const V4B: &str = "203.0.113.9";

    #[test]
    fn the_ipv4_address_item_says_whose_address_dns_has() {
        // The usual server.
        let it = ipv4_address_item(&v4_addrs(V4, &[("eth0", V4)], Some(V4), Ipv4Source::AsPublished, Bind4::Unset));
        assert_eq!(it.status, Status::Ok);
        assert!(it.detail.contains("is on this server"), "{}", it.detail);
        // Behind NAT the public address is on no interface.
        let it = ipv4_address_item(&v4_addrs(V4, &[("ens5", "10.0.0.5")], Some(V4), Ipv4Source::AsPublished, Bind4::Unset));
        assert_eq!(it.status, Status::Ok);
        assert!(it.detail.contains("behind NAT"), "{}", it.detail);
        // The panel hostname behind a proxy, or still pointing at the old
        // server: the panel publishes this server's own address, and says
        // whose the other one is.
        let it = ipv4_address_item(&v4_addrs("104.16.1.1", &[("eth0", V4)], Some(V4), Ipv4Source::AsPublished, Bind4::Unset));
        assert_eq!(it.status, Status::Warn);
        assert!(it.detail.starts_with(&format!("{V4} is on this server")), "{}", it.detail);
        assert!(it.detail.contains("panel.example.com resolves to 104.16.1.1, which is not an address of this server"), "{}", it.detail);
        assert!(it.detail.contains("run Reconcile on each domain"), "{}", it.detail);
        // No address at all.
        let it = ipv4_address_item(&v4_addrs(V4, &[], None, Ipv4Source::AsPublished, Bind4::Unset));
        assert_eq!(it.status, Status::Fail);
    }

    #[test]
    fn the_ipv4_address_item_says_whether_postfix_is_pinned() {
        // Two public addresses, the panel hostname on the second one.
        let two: &[(&str, &str)] = &[("eth0", V4), ("eth0", V4B)];
        let pin = || Ipv4Source::Pin { default: V4.into() };
        let item_for = |source4, bind4| ipv4_address_item(&v4_addrs(V4B, two, Some(V4B), source4, bind4));

        let it = item_for(pin(), Bind4::Ours(Some(V4B.into())));
        assert_eq!(it.status, Status::Ok);
        assert!(it.detail.contains(&format!("would send from {V4}, so Postfix is set to send from {V4B}")), "{}", it.detail);
        // Pinned by hand to the same address.
        let it = item_for(pin(), Bind4::ByHand(V4B.into()));
        assert_eq!(it.status, Status::Ok);
        assert!(it.detail.contains("set by hand"), "{}", it.detail);
        // Not pinned (yet), or pinned to the address the name had before.
        for bind4 in [Bind4::Unset, Bind4::Ours(None), Bind4::Ours(Some(V4.into()))] {
            let it = item_for(pin(), bind4);
            assert_eq!(it.status, Status::Fail);
            assert!(it.detail.contains(&format!("sends from {V4} unless told otherwise")), "{}", it.detail);
            assert!(it.detail.contains("Save the outgoing mail setting"), "{}", it.detail);
        }
        // Pinned to an address that has left the server since.
        let it = item_for(Ipv4Source::AsPublished, Bind4::Ours(Some("192.0.2.7".into())));
        assert_eq!(it.status, Status::Fail);
        assert!(it.detail.contains("still set to send from 192.0.2.7, which is no longer an address of this server"), "{}", it.detail);
        assert!(it.detail.contains("Save the outgoing mail setting"), "{}", it.detail);
        // Could not be pinned.
        let cannot = Ipv4Source::CannotPin {
            default: V4.into(),
            why: "connections from 203.0.113.9 would leave through eth0, not through eth1, the interface the address is on".into(),
        };
        let it = item_for(cannot, Bind4::Unset);
        assert_eq!(it.status, Status::Fail);
        assert!(it.detail.contains(&format!("Postfix could not be set to send from {V4B}: connections from")), "{}", it.detail);
        assert!(it.detail.contains(&format!("Point the panel hostname at {V4}")), "{}", it.detail);
        // Set by hand to another public address: the operator's to change.
        let it = item_for(pin(), Bind4::ByHand(V4.into()));
        assert_eq!(it.status, Status::Fail);
        assert!(it.detail.contains(&format!("set by hand to send from {V4}")), "{}", it.detail);
        assert!(it.detail.contains("postconf -X smtp_bind_address"), "{}", it.detail);
        // By hand to a private address: that leaves through NAT, fine.
        let it = item_for(Ipv4Source::AsPublished, Bind4::ByHand("10.0.0.5".into()));
        assert_eq!(it.status, Status::Ok);
        // The kernel's choice is not known: nothing is claimed.
        assert_eq!(item_for(Ipv4Source::Unknown, Bind4::Unset).status, Status::Ok);
        // With a relay host the address this server sends from does not matter.
        let mut relayed = v4_addrs(V4B, two, Some(V4B), pin(), Bind4::ByHand(V4.into()));
        relayed.relayhost = "[smtp.example.net]:587".into();
        assert_eq!(ipv4_address_item(&relayed).status, Status::Ok);
    }

    fn healthy_dns() -> Table {
        Table::default()
            .with(V6, "PTR", &["panel.example.com"])
            .with("panel.example.com", "AAAA", &[V6])
            .with(V4, "PTR", &["panel.example.com"])
            .with("panel.example.com", "A", &[V4])
            .with("example.com", "TXT", &["v=spf1 mx ip4:198.51.100.127 ip6:2001:db8:1::1 -all"])
            .with("example.com", "MX", &["mail.example.com"])
            .with("mail.example.com", "A", &[V4])
            .with("mail.example.com", "AAAA", &[V6])
    }

    fn address_ok() -> MailCheckItem {
        item("address", "Public IPv6 address", Status::Ok, "on this server")
    }

    #[tokio::test]
    async fn a_family_is_ready_when_every_check_passes() {
        let dns = healthy_dns();
        let domains = Ok(vec!["example.com".to_string()]);
        let check = family_check(&dns, Family::V6, Some(V6.into()), address_ok(), "panel.example.com", &domains, &[]).await;
        assert!(check.ready, "{:?}", check.checks);
        let ids: Vec<&str> = check.checks.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["address", "ptr", "fcrdns", "helo", "spf"]);
        assert!(check.checks.iter().all(|c| c.status == Status::Ok));
        let check = family_check(&dns, Family::V4, Some(V4.into()), address_ok(), "panel.example.com", &domains, &[]).await;
        assert!(check.ready);
    }

    #[tokio::test]
    async fn no_ptr_means_not_ready_and_the_message_says_what_to_do() {
        // The usual state of a new VPS: an address, no reverse DNS for it.
        let dns = Table::default()
            .with("panel.example.com", "AAAA", &[V6])
            .with("example.com", "TXT", &["v=spf1 mx ip4:198.51.100.127 ip6:2001:db8:1::1 -all"]);
        let domains = Ok(vec!["example.com".to_string()]);
        let check = family_check(&dns, Family::V6, Some(V6.into()), address_ok(), "panel.example.com", &domains, &[]).await;
        assert!(!check.ready);
        let (status, detail) = status_of(&check.checks, "ptr");
        assert_eq!(status, Status::Fail);
        assert!(detail.contains("550 5.7.25") && detail.contains("hosting provider"), "{detail}");
        assert_eq!(status_of(&check.checks, "fcrdns").0, Status::Fail);
        assert_eq!(status_of(&check.checks, "helo").0, Status::Ok);
        let msg = not_ready_message(&check);
        assert!(msg.starts_with("IPv6 is not ready to send mail from."), "{msg}");
        assert!(msg.contains("no reverse DNS record") && msg.contains("1 more"), "{msg}");
    }

    #[tokio::test]
    async fn a_ptr_that_does_not_resolve_back_fails_and_a_generic_one_warns() {
        let dns = healthy_dns()
            .with(V6, "PTR", &["static.1.0.0.0.clients.example.net"])
            .with("static.1.0.0.0.clients.example.net", "AAAA", &["2001:db8:ffff::1"]);
        let domains = Ok(vec!["example.com".to_string()]);
        let check = family_check(&dns, Family::V6, Some(V6.into()), address_ok(), "panel.example.com", &domains, &[]).await;
        assert_eq!(status_of(&check.checks, "ptr").0, Status::Warn);
        assert_eq!(status_of(&check.checks, "fcrdns").0, Status::Fail);
        assert!(!check.ready);

        // A provider name that does resolve back: accepted, with a hint.
        let dns = healthy_dns()
            .with(V6, "PTR", &["static.1.0.0.0.clients.example.net"])
            .with("static.1.0.0.0.clients.example.net", "AAAA", &[V6]);
        let check = family_check(&dns, Family::V6, Some(V6.into()), address_ok(), "panel.example.com", &domains, &[]).await;
        assert_eq!(status_of(&check.checks, "ptr").0, Status::Warn);
        assert_eq!(status_of(&check.checks, "fcrdns").0, Status::Ok);
        assert!(check.ready);
    }

    #[tokio::test]
    async fn one_domain_whose_spf_leaves_the_address_out_blocks_the_family() {
        let dns = healthy_dns()
            .with("old.example", "TXT", &["v=spf1 mx ip4:198.51.100.127 -all"])
            .with("unset.example", "TXT", &["some-verification=1"]);
        let domains = Ok(vec!["example.com".to_string(), "old.example".to_string(), "unset.example".to_string()]);
        let check = family_check(&dns, Family::V6, Some(V6.into()), address_ok(), "panel.example.com", &domains, &[]).await;
        let (status, detail) = status_of(&check.checks, "spf");
        assert_eq!(status, Status::Fail);
        assert!(detail.contains("old.example") && !detail.contains("unset.example"), "{detail}");
        assert!(!check.ready);
        // A domain with no SPF record at all is a warning, not a block.
        let domains = Ok(vec!["example.com".to_string(), "unset.example".to_string()]);
        let check = family_check(&dns, Family::V6, Some(V6.into()), address_ok(), "panel.example.com", &domains, &[]).await;
        assert_eq!(status_of(&check.checks, "spf").0, Status::Warn);
        assert!(check.ready);
    }

    #[tokio::test]
    async fn without_an_address_or_without_dns_nothing_is_ready() {
        let missing = item("address", "Public IPv6 address", Status::Fail, "no address");
        let domains = Ok(Vec::new());
        let check = family_check(&healthy_dns(), Family::V6, None, missing, "panel.example.com", &domains, &[]).await;
        assert!(!check.ready);
        assert_eq!(check.checks.len(), 1);

        // No dig, or no network: every lookup fails, and that is not "ready".
        let dns = Table::default()
            .failing(V6, "PTR")
            .failing("panel.example.com", "AAAA");
        let check = family_check(&dns, Family::V6, Some(V6.into()), address_ok(), "panel.example.com", &domains, &[]).await;
        assert!(!check.ready);
        let (status, detail) = status_of(&check.checks, "ptr");
        assert_eq!(status, Status::Fail);
        assert!(detail.contains("could not be looked up: no resolver"), "{detail}");
        assert_eq!(status_of(&check.checks, "helo").0, Status::Fail);
    }

    const KEY: &str = "MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA";

    fn ctx<'a>(zone_ns: Option<&'a [String]>, ipv6: Option<&'a str>, own: &'a [String]) -> DomainCtx<'a> {
        DomainCtx {
            domain: "example.com",
            ipv4: Some(V4),
            ipv6,
            own_ipv6s: own,
            zone_ns,
            parent_zone: None,
            dkim_key: Some(KEY),
            foreign_v4: None,
            ipv6_route: None,
        }
    }

    fn domain_dns() -> Table {
        healthy_dns()
            .with("example.com", "NS", &["ns1.example.com", "ns2.example.com"])
            .with("default._domainkey.example.com", "TXT", &[&format!("v=DKIM1; k=rsa; p={KEY}")])
            .with("_dmarc.example.com", "TXT", &["v=DMARC1; p=none; rua=mailto:postmaster@example.com"])
    }

    #[tokio::test]
    async fn a_domain_with_all_records_for_both_protocols_passes() {
        let ns = vec!["ns1.example.com".to_string(), "ns2.example.com".to_string()];
        let own = vec![V6.to_string()];
        let (managed, checks) = domain_checks(&domain_dns(), &ctx(Some(&ns), Some(V6), &own)).await;
        assert!(managed);
        let ids: Vec<&str> = checks.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["mx", "a", "aaaa", "spf", "dkim", "dmarc"]);
        assert!(checks.iter().all(|c| c.status == Status::Ok), "{checks:?}");
    }

    #[tokio::test]
    async fn a_zone_from_before_ipv6_is_told_to_reconcile() {
        // SPF without ip6, mail host without AAAA: what an old zone looks like
        // once the server gets an IPv6 address.
        let dns = Table::default()
            .with("example.com", "NS", &["ns1.example.com", "ns2.example.com"])
            .with("example.com", "MX", &["mail.example.com"])
            .with("mail.example.com", "A", &[V4])
            .with("example.com", "TXT", &["v=spf1 mx ip4:198.51.100.127 -all"])
            .with("default._domainkey.example.com", "TXT", &[&format!("v=DKIM1; k=rsa; p={KEY}")])
            .with("_dmarc.example.com", "TXT", &["v=DMARC1; p=none"]);
        let ns = vec!["ns1.example.com".to_string(), "ns2.example.com".to_string()];
        let own = vec![V6.to_string()];
        let (managed, checks) = domain_checks(&dns, &ctx(Some(&ns), Some(V6), &own)).await;
        assert!(managed);
        let (status, detail) = status_of(&checks, "spf");
        assert_eq!(status, Status::Fail);
        assert!(detail.contains("does not allow mail from 2001:db8:1::1"), "{detail}");
        assert!(detail.contains("Run Reconcile on example.com"), "{detail}");
        assert!(detail.contains("\"v=spf1 mx ip4:198.51.100.127 ip6:2001:db8:1::1 -all\""), "{detail}");
        let (status, detail) = status_of(&checks, "aaaa");
        assert_eq!(status, Status::Warn);
        assert!(detail.contains("mail.example.com AAAA 2001:db8:1::1"), "{detail}");
        assert_eq!(status_of(&checks, "a").0, Status::Ok);

        // The same zone on a server without IPv6 is fine as it is.
        let (_, checks) = domain_checks(&dns, &ctx(Some(&ns), None, &[])).await;
        assert!(checks.iter().all(|c| c.status == Status::Ok), "{checks:?}");
    }

    #[tokio::test]
    async fn a_domain_hosted_elsewhere_gets_the_exact_records_to_add() {
        let dns = Table::default().with("example.com", "NS", &["dns1.registrar.example"]);
        let own = vec![V6.to_string()];
        let (managed, checks) = domain_checks(&dns, &ctx(None, Some(V6), &own)).await;
        assert!(!managed);
        let (status, detail) = status_of(&checks, "mx");
        assert_eq!(status, Status::Fail);
        assert!(detail.contains("where the DNS of example.com is hosted: example.com MX 10 mail.example.com"), "{detail}");
        assert!(status_of(&checks, "a").1.contains("mail.example.com A 198.51.100.127"));
        assert!(status_of(&checks, "spf").1.contains("example.com TXT \"v=spf1 mx ip4:198.51.100.127 ip6:2001:db8:1::1 -all\""));
        let (status, detail) = status_of(&checks, "dkim");
        assert_eq!(status, Status::Fail);
        assert!(detail.contains(&format!("default._domainkey.example.com TXT \"v=DKIM1; k=rsa; p={KEY}\"")), "{detail}");
        assert_eq!(status_of(&checks, "dmarc").0, Status::Warn);

        // A zone in the panel that the registrar does not point at: Reconcile
        // would change records nobody sees.
        let ns = vec!["ns1.example.com".to_string(), "ns2.example.com".to_string()];
        let (managed, checks) = domain_checks(&dns, &ctx(Some(&ns), Some(V6), &own)).await;
        assert!(!managed);
        assert_eq!(status_of(&checks, "ns").0, Status::Warn);
        assert!(status_of(&checks, "mx").1.contains("where the DNS of example.com is hosted"));
    }

    #[tokio::test]
    async fn wrong_records_are_failures() {
        let ns = vec!["ns1.example.com".to_string()];
        let own = vec![V6.to_string()];
        let dns = domain_dns()
            .with("mail.example.com", "A", &["203.0.113.50"])
            .with("mail.example.com", "AAAA", &["2001:db8:ffff::1"])
            .with("example.com", "TXT", &["v=spf1 -all", "v=spf1 mx -all"])
            .with("default._domainkey.example.com", "TXT", &["v=DKIM1; k=rsa; p=SomeOtherKey"])
            .with("_dmarc.example.com", "TXT", &["v=DMARC1; p=none", "v=DMARC1; p=reject"]);
        let (_, checks) = domain_checks(&dns, &ctx(Some(&ns), Some(V6), &own)).await;
        for id in ["a", "aaaa", "spf", "dkim", "dmarc"] {
            assert_eq!(status_of(&checks, id).0, Status::Fail, "{id}");
        }
        assert!(status_of(&checks, "spf").1.contains("more than one SPF record"));

        // An AAAA left behind on a server that has no IPv6 (any more).
        let (_, checks) = domain_checks(&dns, &ctx(Some(&ns), None, &[])).await;
        let (status, detail) = status_of(&checks, "aaaa");
        assert_eq!(status, Status::Fail);
        assert!(detail.contains("Remove it under Domains > example.com > DNS records"), "{detail}");
    }

    #[tokio::test]
    async fn records_with_the_panel_hostnames_foreign_address_are_called_that() {
        // The panel hostname is behind a proxy at 104.16.1.1. The zone was
        // written while the panel took that for this server's address.
        let ns = vec!["ns1.example.com".to_string()];
        let dns = domain_dns()
            .with("mail.example.com", "A", &["104.16.1.1"])
            .with("example.com", "TXT", &["v=spf1 mx ip4:104.16.1.1 -all"]);
        let mut c = ctx(Some(&ns), None, &[]);
        c.foreign_v4 = Some("104.16.1.1");
        let (_, checks) = domain_checks(&dns, &c).await;
        let (status, detail) = status_of(&checks, "a");
        assert_eq!(status, Status::Fail);
        assert!(
            detail.contains("resolves to 104.16.1.1, the address the panel hostname has, and that is not this server (198.51.100.127)"),
            "{detail}"
        );
        assert!(detail.contains("Run Reconcile on example.com"), "{detail}");
        let (status, detail) = status_of(&checks, "spf");
        assert_eq!(status, Status::Fail);
        assert!(detail.contains("does not allow mail from 198.51.100.127"), "{detail}");
        assert!(detail.contains("It names 104.16.1.1, the address the panel hostname has"), "{detail}");
        // Any other wrong address is just that.
        let dns = domain_dns().with("mail.example.com", "A", &["203.0.113.50"]);
        let (_, checks) = domain_checks(&dns, &c).await;
        assert!(status_of(&checks, "a").1.contains("resolves to 203.0.113.50, not to this server"));
    }

    #[tokio::test]
    async fn the_domain_check_says_when_a_domain_is_held_to_ipv4_and_what_to_fix() {
        let ns = vec!["ns1.example.com".to_string(), "ns2.example.com".to_string()];
        let own = vec![V6.to_string()];
        // The SPF record lacks the IPv6 address this server sends from.
        let lacks = Table::default()
            .with("example.com", "NS", &["ns1.example.com", "ns2.example.com"])
            .with("example.com", "MX", &["mail.example.com"])
            .with("mail.example.com", "A", &[V4])
            .with("example.com", "TXT", &["v=spf1 mx ip4:198.51.100.127 -all"])
            .with("default._domainkey.example.com", "TXT", &[&format!("v=DKIM1; k=rsa; p={KEY}")])
            .with("_dmarc.example.com", "TXT", &["v=DMARC1; p=none"]);
        let route_item = |checks: &[MailCheckItem]| checks.iter().find(|c| c.id == "ipv6").cloned();

        // Held: the item says so, and the SPF item above it has the record.
        let mut c = ctx(Some(&ns), Some(V6), &own);
        c.ipv6_route = Some(Ipv6Route::Held);
        let (_, checks) = domain_checks(&lacks, &c).await;
        let ids: Vec<&str> = checks.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["mx", "a", "aaaa", "spf", "ipv6", "dkim", "dmarc"]);
        assert_eq!(status_of(&checks, "spf").0, Status::Fail);
        let held = route_item(&checks).unwrap();
        assert_eq!(held.status, Status::Warn);
        assert_eq!(held.label, "Outgoing mail over IPv6");
        assert!(
            held.detail.starts_with(&format!(
                "Mail from example.com goes out over IPv4 only, because its SPF record does not allow mail from {V6}."
            )),
            "{}",
            held.detail
        );
        assert!(held.detail.contains("then save the outgoing mail setting again (Settings > Mail)"), "{}", held.detail);

        // Not held yet (the record changed after Postfix was set up).
        c.ipv6_route = Some(Ipv6Route::Free);
        let (_, checks) = domain_checks(&lacks, &c).await;
        let free = route_item(&checks).unwrap();
        assert!(free.detail.contains("Postfix does not keep its mail on IPv4 yet"), "{}", free.detail);
        // The operator forced IPv6 for it.
        c.ipv6_route = Some(Ipv6Route::Forced);
        let (_, checks) = domain_checks(&lacks, &c).await;
        let forced = route_item(&checks).unwrap();
        assert!(forced.detail.contains("\"Use IPv6 anyway\""), "{}", forced.detail);

        // The record is right again but the hold is still in place.
        c.ipv6_route = Some(Ipv6Route::Held);
        let (_, checks) = domain_checks(&domain_dns(), &c).await;
        assert_eq!(status_of(&checks, "spf").0, Status::Ok);
        let lifted = route_item(&checks).unwrap();
        assert!(lifted.detail.contains("Nothing stands in the way now"), "{}", lifted.detail);
        // The lookup fails.
        let down = domain_dns().failing("example.com", "TXT");
        let (_, checks) = domain_checks(&down, &c).await;
        assert!(route_item(&checks).unwrap().detail.contains("could not be looked up just now"));

        // Nothing to say: the record allows the address and nothing holds
        // the domain, or Postfix does not send over IPv6 at all.
        for route in [Some(Ipv6Route::Free), Some(Ipv6Route::Forced), None] {
            c.ipv6_route = route;
            let (_, checks) = domain_checks(&domain_dns(), &c).await;
            assert!(route_item(&checks).is_none(), "{route:?}");
        }
        let mut no_v6 = ctx(Some(&ns), None, &[]);
        no_v6.ipv6_route = Some(Ipv6Route::Held);
        let (_, checks) = domain_checks(&lacks, &no_v6).await;
        assert!(route_item(&checks).is_none());
    }

    #[test]
    fn domains_held_to_ipv4_do_not_keep_ipv6_from_being_ready() {
        let domains = vec!["example.com".to_string(), "old.example".to_string(), "new.example".to_string()];
        let results = vec![
            ("example.com".to_string(), Spf::Pass),
            ("old.example".to_string(), Spf::Fail),
            ("new.example".to_string(), Spf::SoftFail),
        ];
        // Both are held: a warning that names them.
        let held = vec!["new.example".to_string(), "old.example".to_string()];
        let it = spf_sweep_item(&domains, &results, V6, &held);
        assert_eq!(it.status, Status::Warn);
        assert!(it.detail.contains("old.example, new.example"), "{}", it.detail);
        assert!(it.detail.contains("Postfix sends mail from those domains over IPv4 only"), "{}", it.detail);
        // One of them is not: its mail can leave from the address.
        let it = spf_sweep_item(&domains, &results, V6, &held[1..]);
        assert_eq!(it.status, Status::Fail);
        assert_eq!(spf_sweep_item(&domains, &results, V6, &[]).status, Status::Fail);
    }

    #[test]
    fn the_dkim_key_is_compared_without_whitespace() {
        assert_eq!(dkim_public_key("v=DKIM1; k=rsa; p=AB CD\tEF").as_deref(), Some("ABCDEF"));
        assert_eq!(dkim_public_key("k=rsa;P=abc").as_deref(), Some("abc"));
        assert_eq!(dkim_public_key("v=spf1 mx -all"), None);
    }
}
