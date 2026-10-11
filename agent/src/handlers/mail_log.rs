//! Track delivery: what Postfix did with mail to or from one address, read
//! from the journal (Ubuntu logs every Postfix daemon under postfix@-.service).
//! A message's lines share its queue ID, so the rows say who sent it and what
//! happened to each recipient: delivered to a mailbox here, sent on to another
//! server, deferred (Postfix keeps retrying), bounced, expired, or rejected
//! before it was ever queued (relay denied, spam). Read-only.

use crate::error::{AgentError, AgentResult};
use crate::exec;
use crate::handlers::mail::mailbox_parts;
use axum::{extract::Query, Json};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Rows returned, newest first.
const MAX_ROWS: usize = 200;
/// Journal lines read at most, so a busy server's week can't fill the agent's
/// memory.
const MAX_LINES: &str = "200000";

#[derive(Deserialize)]
pub struct TrackQuery {
    pub address: String,
    #[serde(default = "default_days")]
    pub days: u32,
}

fn default_days() -> u32 {
    3
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct TrackRow {
    /// Journal timestamp (short-iso) of the line the status comes from.
    pub time: String,
    pub queue_id: Option<String>,
    /// Envelope sender; empty for bounce notices (`from=<>`).
    pub from: String,
    /// Empty while nothing has been tried yet (status `queued`).
    pub to: String,
    /// The address the mail was sent to when a forwarder rewrote it.
    pub orig_to: Option<String>,
    /// delivered | sent | deferred | bounced | expired | rejected | queued
    pub status: String,
    pub detail: String,
}

#[derive(Serialize)]
pub struct TrackResp {
    pub address: String,
    pub days: u32,
    pub rows: Vec<TrackRow>,
    /// More rows matched than MAX_ROWS.
    pub truncated: bool,
}

/// GET /mail/track?address=&days=
pub async fn track(Query(q): Query<TrackQuery>) -> AgentResult<Json<TrackResp>> {
    let (address, _, _) = mailbox_parts(&q.address)
        .map_err(|_| AgentError::Invalid("enter a full email address".into()))?;
    let days = q.days.clamp(1, 14);
    let since = (chrono::Local::now() - chrono::Duration::days(i64::from(days)))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    let out = exec::run(
        "journalctl",
        [
            "-q", "--no-pager", "-o", "short-iso",
            "-u", "postfix@-.service", "-u", "postfix.service",
            "--since", since.as_str(), "-n", MAX_LINES,
        ],
    )
    .await?;
    let mut rows = track_rows(&out.stdout, &address);
    rows.reverse();
    let truncated = rows.len() > MAX_ROWS;
    rows.truncate(MAX_ROWS);
    Ok(Json(TrackResp { address, days, rows, truncated }))
}

struct LogLine<'a> {
    time: &'a str,
    /// `postfix/smtp`, `postfix/submission/smtpd`, ...
    prog: &'a str,
    qid: Option<&'a str>,
    msg: &'a str,
}

/// `<time> <host> postfix/<daemon>[pid]: [<queue id>: ]<text>`; None for
/// anything that is not a Postfix line.
fn parse_line(line: &str) -> Option<LogLine<'_>> {
    let (time, rest) = line.split_once(' ')?;
    let (_host, rest) = rest.split_once(' ')?;
    let (ident, text) = rest.split_once(": ")?;
    let prog = ident.split('[').next().unwrap_or(ident);
    if !prog.starts_with("postfix") {
        return None;
    }
    Some(match text.split_once(": ") {
        Some((q, msg)) if is_queue_id(q) => LogLine { time, prog, qid: Some(q), msg },
        _ => LogLine { time, prog, qid: None, msg: text },
    })
}

/// Short (hex) or long Postfix queue IDs. Words like NOQUEUE or warning are not.
fn is_queue_id(s: &str) -> bool {
    (6..=20).contains(&s.len())
        && s.chars().all(|c| c.is_ascii_alphanumeric())
        && (s.chars().any(|c| c.is_ascii_digit()) || s.chars().all(|c| matches!(c, 'A'..='F')))
}

/// Where `key=` starts in `msg`: at the start or after a space, so `to=` never
/// matches inside `orig_to=`.
fn key_at(msg: &str, key: &str) -> Option<usize> {
    let pat = format!("{key}=");
    let mut from = 0;
    while let Some(i) = msg[from..].find(&pat) {
        let at = from + i;
        if at == 0 || msg.as_bytes()[at - 1] == b' ' {
            return Some(at + pat.len());
        }
        from = at + pat.len();
    }
    None
}

/// `key=<value>` (from, to, orig_to).
fn angle<'a>(msg: &'a str, key: &str) -> Option<&'a str> {
    let start = key_at(msg, key)?;
    let rest = msg[start..].strip_prefix('<')?;
    Some(&rest[..rest.find('>')?])
}

/// `key=value` up to the next comma or space.
fn plain<'a>(msg: &'a str, key: &str) -> Option<&'a str> {
    let start = key_at(msg, key)?;
    let rest = &msg[start..];
    Some(&rest[..rest.find(|c| c == ',' || c == ' ').unwrap_or(rest.len())])
}

/// The remote side's answer: the parenthesised text after `status=...`.
fn status_reason(msg: &str) -> &str {
    let Some(i) = msg.find("status=") else { return "" };
    let rest = &msg[i..];
    match (rest.find('('), rest.rfind(')')) {
        (Some(a), Some(b)) if b > a => &rest[a + 1..b],
        _ => "",
    }
}

fn mentions(msg: &str, address: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    lower.contains(&format!("=<{address}>")) || plain(&lower, "sasl_username") == Some(address)
}

/// `NOQUEUE: reject: RCPT from h[ip]: 554 5.7.1 <a@b>: Relay access denied; from=<..> to=<..> ...`
/// and `milter-reject: END-OF-MESSAGE from h[ip]: 5.7.1 Spam message rejected; from=<..> ...`.
fn reject_row(l: &LogLine<'_>) -> TrackRow {
    let body = l.msg.split_once("; from=").map_or(l.msg, |(b, _)| b);
    let reason = body.split_once("]: ").map_or(body, |(_, r)| r);
    TrackRow {
        time: l.time.to_string(),
        queue_id: l.qid.map(str::to_string),
        from: angle(l.msg, "from").unwrap_or("").to_ascii_lowercase(),
        to: angle(l.msg, "to").unwrap_or("").to_ascii_lowercase(),
        orig_to: None,
        status: "rejected".into(),
        detail: reason.trim().to_string(),
    }
}

#[derive(Default)]
struct Msg {
    time: String,
    from: Option<String>,
    sasl: Option<String>,
    expired: bool,
    rcpts: Vec<TrackRow>,
    rejected: Vec<TrackRow>,
}

/// One row per recipient of every message that mentions `address` (as sender,
/// login, recipient or forwarder source), plus mail rejected before it was
/// queued. Oldest first.
pub(crate) fn track_rows(journal: &str, address: &str) -> Vec<TrackRow> {
    let lines: Vec<LogLine<'_>> = journal.lines().filter_map(parse_line).collect();
    let mut rows = Vec::new();
    let mut wanted: HashSet<&str> = HashSet::new();
    for l in &lines {
        if !mentions(l.msg, address) {
            continue;
        }
        match l.qid {
            Some(q) => {
                wanted.insert(q);
            }
            None if l.msg.starts_with("NOQUEUE: reject:") => rows.push(reject_row(l)),
            None => {}
        }
    }

    let mut order: Vec<&str> = Vec::new();
    let mut msgs: HashMap<&str, Msg> = HashMap::new();
    for l in &lines {
        let Some(q) = l.qid.filter(|q| wanted.contains(q)) else { continue };
        let m = msgs.entry(q).or_insert_with(|| {
            order.push(q);
            Msg { time: l.time.to_string(), ..Msg::default() }
        });
        if l.msg.starts_with("client=") {
            if let Some(u) = plain(l.msg, "sasl_username") {
                m.sasl = Some(u.to_ascii_lowercase());
            }
        } else if l.msg.starts_with("from=<") {
            m.from = angle(l.msg, "from").map(str::to_ascii_lowercase);
            m.expired |= l.msg.contains("status=expired");
        } else if l.msg.starts_with("milter-reject:") || l.msg.starts_with("reject:") {
            m.rejected.push(reject_row(l));
        } else if l.msg.starts_with("to=<") {
            let to = angle(l.msg, "to").unwrap_or("").to_ascii_lowercase();
            let status = plain(l.msg, "status").unwrap_or("");
            let reason = status_reason(l.msg);
            let relay = plain(l.msg, "relay").unwrap_or("none");
            let local = ["/lmtp", "/local", "/virtual"].iter().any(|d| l.prog.ends_with(d));
            let (status, detail) = match status {
                "sent" if local => ("delivered".to_string(), reason.to_string()),
                _ if relay == "none" || local => (status.to_string(), reason.to_string()),
                _ => (status.to_string(), format!("{relay}: {reason}")),
            };
            let row = TrackRow {
                time: l.time.to_string(),
                queue_id: Some(q.to_string()),
                from: String::new(),
                to: to.clone(),
                orig_to: angle(l.msg, "orig_to").map(str::to_ascii_lowercase),
                status,
                detail,
            };
            // A retried recipient keeps only its latest attempt.
            match m.rcpts.iter_mut().find(|r| r.to == to) {
                Some(r) => *r = row,
                None => m.rcpts.push(row),
            }
        }
    }

    for q in order {
        let Some(m) = msgs.remove(q) else { continue };
        let from = m.from.clone().unwrap_or_default();
        let sender_side = m.from.as_deref() == Some(address) || m.sasl.as_deref() == Some(address);
        if m.rcpts.is_empty() && m.rejected.is_empty() {
            rows.push(TrackRow {
                time: m.time.clone(),
                queue_id: Some(q.to_string()),
                from: from.clone(),
                to: String::new(),
                orig_to: None,
                status: "queued".into(),
                detail: "accepted, no delivery attempt logged yet".into(),
            });
        }
        for mut r in m.rcpts {
            if !(sender_side || r.to == address || r.orig_to.as_deref() == Some(address)) {
                continue;
            }
            r.from = from.clone();
            if m.expired && r.status == "deferred" {
                r.status = "expired".into();
            }
            rows.push(r);
        }
        rows.extend(m.rejected);
    }
    rows.sort_by(|a, b| a.time.cmp(&b.time));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "\
2026-10-11T10:00:00+0200 vps postfix/smtpd[1000]: connect from mail-ed1.google.com[209.85.208.1]
2026-10-11T10:00:01+0200 vps postfix/smtpd[1001]: 4AB12C3D4E: client=mail-ed1.google.com[209.85.208.1]
2026-10-11T10:00:01+0200 vps postfix/cleanup[1002]: 4AB12C3D4E: message-id=<abc@mail.gmail.com>
2026-10-11T10:00:02+0200 vps postfix/qmgr[900]: 4AB12C3D4E: from=<Friend@gmail.com>, size=4321, nrcpt=1 (queue active)
2026-10-11T10:00:02+0200 vps postfix/lmtp[1003]: 4AB12C3D4E: to=<jane@example.com>, orig_to=<sales@example.com>, relay=vps[private/dovecot-lmtp], delay=0.2, delays=0.1/0/0/0.1, dsn=2.0.0, status=sent (250 2.0.0 <jane@example.com> Xyz Saved)
2026-10-11T10:00:02+0200 vps postfix/qmgr[900]: 4AB12C3D4E: removed
2026-10-11T10:30:00+0200 vps postfix/qmgr[900]: 7EF00A1B2C: from=<friend@gmail.com>, size=100, nrcpt=1 (queue active)
2026-10-11T10:30:00+0200 vps postfix/lmtp[1004]: 7EF00A1B2C: to=<bjane@example.com>, relay=vps[private/dovecot-lmtp], delay=0.1, dsn=2.0.0, status=sent (250 2.0.0 <bjane@example.com> Saved)
2026-10-11T11:00:00+0200 vps postfix/submission/smtpd[2001]: 5BC23D4E5F: client=unknown[203.0.113.7], sasl_method=PLAIN, sasl_username=jane@example.com
2026-10-11T11:00:01+0200 vps postfix/qmgr[900]: 5BC23D4E5F: from=<jane@example.com>, size=900, nrcpt=2 (queue active)
2026-10-11T11:00:02+0200 vps postfix/smtp[2003]: 5BC23D4E5F: to=<a@gmail.com>, relay=gmail-smtp-in.l.google.com[142.250.1.27]:25, delay=1.4, delays=0.1/0/0.5/0.8, dsn=2.0.0, status=sent (250 2.0.0 OK 1700000000 x1si)
2026-10-11T11:00:03+0200 vps postfix/smtp[2003]: 5BC23D4E5F: to=<b@nowhere.example>, relay=none, delay=2, delays=0.1/0/1.9/0, dsn=4.4.1, status=deferred (connect to nowhere.example[198.51.100.9]:25: Connection timed out)
2026-10-11T11:30:03+0200 vps postfix/smtp[2010]: 5BC23D4E5F: to=<b@nowhere.example>, relay=mx.nowhere.example[198.51.100.9]:25, delay=1800, dsn=5.1.1, status=bounced (host mx.nowhere.example[198.51.100.9] said: 550 5.1.1 No such user (in reply to RCPT TO command))
2026-10-11T12:00:00+0200 vps postfix/smtpd[3001]: NOQUEUE: reject: RCPT from bad.example[192.0.2.66]: 554 5.7.1 <jane@example.com>: Relay access denied; from=<spam@bad.example> to=<jane@example.com> proto=ESMTP helo=<bad.example>
2026-10-11T12:05:00+0200 vps postfix/cleanup[3002]: 6CD34E5F6A: milter-reject: END-OF-MESSAGE from bad.example[192.0.2.66]: 5.7.1 Spam message rejected; from=<spam@bad.example> to=<jane@example.com> proto=ESMTP helo=<bad.example>
2026-10-11T12:10:00+0200 vps postfix/smtpd[3003]: warning: hostname x.example does not resolve to address 192.0.2.1
2026-10-11T12:10:00+0200 vps dovecot[77]: lmtp(jane@example.com): saved mail to INBOX
";

    fn statuses(rows: &[TrackRow]) -> Vec<(&str, &str, &str)> {
        rows.iter().map(|r| (r.to.as_str(), r.status.as_str(), r.from.as_str())).collect()
    }

    #[test]
    fn a_mailbox_sees_its_mail_in_and_out_and_nothing_else() {
        let rows = track_rows(LOG, "jane@example.com");
        assert_eq!(
            statuses(&rows),
            vec![
                ("jane@example.com", "delivered", "friend@gmail.com"),
                ("a@gmail.com", "sent", "jane@example.com"),
                ("b@nowhere.example", "bounced", "jane@example.com"),
                ("jane@example.com", "rejected", "spam@bad.example"),
                ("jane@example.com", "rejected", "spam@bad.example"),
            ]
        );
        assert_eq!(rows[0].orig_to.as_deref(), Some("sales@example.com"));
        assert_eq!(rows[0].detail, "250 2.0.0 <jane@example.com> Xyz Saved");
        assert_eq!(
            rows[1].detail,
            "gmail-smtp-in.l.google.com[142.250.1.27]:25: 250 2.0.0 OK 1700000000 x1si"
        );
        // The retried recipient shows only its last attempt.
        assert_eq!(rows[2].time, "2026-10-11T11:30:03+0200");
        assert_eq!(rows[3].detail, "554 5.7.1 <jane@example.com>: Relay access denied");
        assert_eq!(rows[4].detail, "5.7.1 Spam message rejected");
        assert_eq!(rows[4].queue_id.as_deref(), Some("6CD34E5F6A"));
    }

    #[test]
    fn a_forwarder_address_finds_what_it_forwarded() {
        let rows = track_rows(LOG, "sales@example.com");
        assert_eq!(statuses(&rows), vec![("jane@example.com", "delivered", "friend@gmail.com")]);
    }

    #[test]
    fn a_recipient_of_a_multi_recipient_message_sees_only_its_own_row() {
        let rows = track_rows(LOG, "a@gmail.com");
        assert_eq!(statuses(&rows), vec![("a@gmail.com", "sent", "jane@example.com")]);
    }

    #[test]
    fn deferred_then_expired_and_queued_messages() {
        let log = "\
2026-10-12T09:00:00+0200 vps postfix/qmgr[1]: 1A2B3C4D5E: from=<jane@example.com>, size=10, nrcpt=1 (queue active)
2026-10-12T09:00:01+0200 vps postfix/smtp[2]: 1A2B3C4D5E: to=<x@slow.example>, relay=none, delay=1, dsn=4.4.1, status=deferred (connect to slow.example[198.51.100.2]:25: Connection refused)
2026-10-17T09:00:00+0200 vps postfix/qmgr[1]: 1A2B3C4D5E: from=<jane@example.com>, status=expired, returned to sender
2026-10-17T09:05:00+0200 vps postfix/qmgr[1]: 9F8E7D6C5B: from=<jane@example.com>, size=10, nrcpt=1 (queue active)
";
        let rows = track_rows(log, "jane@example.com");
        assert_eq!(statuses(&rows), vec![
            ("x@slow.example", "expired", "jane@example.com"),
            ("", "queued", "jane@example.com"),
        ]);
        assert_eq!(rows[0].detail, "connect to slow.example[198.51.100.2]:25: Connection refused");
    }

    #[test]
    fn queue_ids_and_fields() {
        assert!(is_queue_id("4AB12C3D4E"));
        assert!(is_queue_id("62479782"));
        assert!(is_queue_id("4Lhwr02jXFz1hJ5"));
        assert!(!is_queue_id("NOQUEUE"));
        assert!(!is_queue_id("warning"));
        let msg = "to=<b@x.example>, orig_to=<a@x.example>, relay=none, status=deferred (later)";
        assert_eq!(angle(msg, "to"), Some("b@x.example"));
        assert_eq!(angle(msg, "orig_to"), Some("a@x.example"));
        assert_eq!(plain(msg, "relay"), Some("none"));
        assert_eq!(plain(msg, "status"), Some("deferred"));
        assert_eq!(status_reason(msg), "later");
        assert!(parse_line("2026-10-11T12:10:00+0200 vps dovecot[77]: lmtp(x): saved").is_none());
    }
}
