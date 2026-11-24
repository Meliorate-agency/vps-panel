//! Operator notifications configured on /notifications (panel.db table
//! `notifications`): email through the local Postfix, JSON webhooks (with
//! `text` / `content` fields, so Slack and Discord webhooks work as is) and
//! Telegram messages from the operator's own bot.
//!
//! What is reported: failed tasks (`event_for_task`), tasks an agent restart
//! cut short, and what `monitor_loop` finds every 5 minutes (failed units,
//! required services stopped or disabled, full disks and inodes, mail stuck
//! in the queue, the kernel's OOM killer), plus the background jobs that call
//! in here (certificates close to expiry, the privacy settings, a panel
//! update). The panel raises the My Networks events through /notifications/send.
//!
//! A condition is reported once when it starts and, for services, disks and
//! the mail queue, once when it ends. What is reported is kept in
//! `notify_state` (panel.db), so an agent restart (every panel update is one)
//! does not report anything again. Delivery never holds up the caller: it
//! runs in its own task, retries a channel that is briefly unreachable, and
//! leaves its result on the channel's row for the Notifications page.

use crate::db::PanelDb;
use crate::error::{AgentError, AgentResult};
use crate::exec;
use crate::ipc::OkResponse;
use crate::state::AppState;
use axum::extract::State;
use axum::Json;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::OnceLock;
use std::time::Duration;

static HOST: OnceLock<String> = OnceLock::new();

pub fn init(panel_host: &str) {
    let _ = HOST.set(panel_host.to_string());
}

fn host() -> &'static str {
    HOST.get().map(String::as_str).unwrap_or("vps-panel")
}

/// Every event that can be sent. src/lib/notificationEvents.ts offers the
/// same ids on the Notifications page (a test below checks both lists).
pub const EVENTS: &[&str] = &[
    "ssl.fail",
    "backup.fail",
    "deploy.fail",
    "task.fail",
    "service.fail",
    "service.ok",
    "disk.full",
    "disk.ok",
    "mail.fail",
    "mail.ok",
    "memory.oom",
    "network.peer_down",
    "network.peer_up",
];

/// The /notifications event a failed task of this kind reports as.
pub fn event_for_task(kind: &str) -> &'static str {
    if kind.starts_with("ssl.") || kind.starts_with("sites.ssl") || kind.starts_with("mail.ssl") {
        "ssl.fail"
    } else if kind.starts_with("backups.") {
        "backup.fail"
    } else if kind.starts_with("apps.") {
        "deploy.fail"
    } else {
        "task.fail"
    }
}

/// A task kind as an operator reads it: "Certificate renewal for example.com".
fn describe_task(kind: &str) -> String {
    let (head, arg) = match kind.split_once(':') {
        Some((h, a)) => (h, a),
        None => (kind, ""),
    };
    match (head, arg) {
        ("ssl.issue", d) => format!("Certificate order for {d}"),
        ("ssl.renew", d) => format!("Certificate renewal for {d}"),
        ("sites.ssl", d) => format!("Certificate for {d}"),
        ("mail.ssl", d) => format!("Mail certificate for mail.{d}"),
        ("backups.run", _) => "Backup".to_string(),
        ("backups.restore", s) => format!("Restore of snapshot {s}"),
        ("apps.create", a) => format!("Deploy of the app {a}"),
        ("apps.update", a) => format!("Update of the app {a}"),
        ("apps.recover", a) => format!("Putting back the last build of the app {a}"),
        ("system", "self-update") => "Panel update".to_string(),
        ("system", "apt-upgrade") => "System package upgrade".to_string(),
        ("db", rest) if rest.starts_with("import:") => {
            format!("Database import into {}", &rest["import:".len()..])
        }
        ("quick-install", a) => format!("Install of {a}"),
        _ => format!("The task {kind}"),
    }
}

/// Task kinds a background loop starts again every day while they fail (the
/// renewal loop): reported once, until one of that kind succeeds.
fn retried_daily(kind: &str) -> bool {
    kind.starts_with("ssl.renew:") || kind.starts_with("mail.ssl:")
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// One notification, before it is put in a channel's shape.
#[derive(Clone, Debug)]
pub struct Message {
    pub event: &'static str,
    /// What happened, in one line: what a lock screen shows.
    pub subject: String,
    /// What it means and what to do, without log lines.
    pub body: String,
    /// When it happened, or was first seen.
    pub since: DateTime<Utc>,
    /// Where in the panel to look: the page's name and its path.
    pub look: Option<(String, String)>,
    /// Raw lines (a task log, a unit's journal, Postfix's reasons) under a
    /// heading. They can hold email and IP addresses, so only email (sent
    /// from this server) and the channels that opted in get them.
    pub detail: Option<(String, String)>,
}

impl Message {
    pub fn new(event: &'static str, subject: impl Into<String>, body: impl Into<String>) -> Self {
        Message {
            event,
            subject: subject.into(),
            body: body.into(),
            since: Utc::now(),
            look: None,
            detail: None,
        }
    }
    pub fn since(mut self, at: DateTime<Utc>) -> Self {
        self.since = at;
        self
    }
    pub fn look(mut self, page: &str, path: impl Into<String>) -> Self {
        self.look = Some((page.to_string(), path.into()));
        self
    }
    pub fn detail(mut self, heading: &str, lines: impl Into<String>) -> Self {
        let lines: String = lines.into();
        if !lines.trim().is_empty() {
            self.detail = Some((heading.to_string(), lines.trim_end().to_string()));
        }
        self
    }
}

fn fmt_time(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M UTC").to_string()
}

fn panel_url(path: &str) -> String {
    format!("https://{}{path}", host())
}

/// `[host] subject`, on one line.
fn subject_line(msg: &Message) -> String {
    let s: String = msg.subject.replace(['\r', '\n'], " ").chars().take(200).collect();
    format!("[{}] {s}", host())
}

/// The body, the facts every message carries, and the detail lines when
/// `with_detail`; without them, a line saying they were left out.
fn body_head(msg: &Message, with_detail: bool) -> String {
    let mut t = msg.body.trim_end().to_string();
    t.push_str(&format!("\n\nServer: {}\nWhen: {}", host(), fmt_time(msg.since)));
    if let Some((page, path)) = &msg.look {
        t.push_str(&format!("\nIn the panel: {page}, {}", panel_url(path)));
    }
    if msg.detail.is_some() && !with_detail {
        t.push_str("\n\nLog lines are left out on this channel; the panel has them.");
    }
    t
}

fn render_text(msg: &Message, with_detail: bool) -> String {
    let head = body_head(msg, with_detail);
    match (&msg.detail, with_detail) {
        (Some((heading, lines)), true) => format!("{head}\n\n{heading}:\n{lines}"),
        _ => head,
    }
}

/// `head` and as many of the LAST detail lines as fit in `max` (as
/// `measure` counts): the end of a log is where the error is.
fn fit(head: &str, detail: Option<&(String, String)>, max: usize, measure: fn(&str) -> usize) -> String {
    if let Some((heading, lines)) = detail {
        let full = format!("{head}\n\n{heading}:\n{lines}");
        if measure(&full) <= max {
            return full;
        }
        let prefix = format!("{head}\n\n{heading} (the last ones):\n");
        if measure(&prefix) < max {
            let budget = max - measure(&prefix);
            let mut used = 0;
            let mut kept: Vec<&str> = Vec::new();
            for line in lines.lines().rev() {
                let cost = measure(line) + 1;
                if used + cost > budget {
                    break;
                }
                used += cost;
                kept.push(line);
            }
            if kept.is_empty() {
                let last = lines.lines().last().unwrap_or("");
                return format!("{prefix}{}", cut_end(last, budget, measure));
            }
            kept.reverse();
            return format!("{prefix}{}", kept.join("\n"));
        }
    }
    cut_end(head, max, measure)
}

/// `s` cut to `max`, marked when it was.
fn cut_end(s: &str, max: usize, measure: fn(&str) -> usize) -> String {
    const MARK: &str = " [...]";
    if measure(s) <= max {
        return s.to_string();
    }
    let room = max.saturating_sub(measure(MARK));
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let mut buf = [0u8; 4];
        let n = measure(c.encode_utf8(&mut buf));
        if used + n > room {
            break;
        }
        used += n;
        out.push(c);
    }
    out.push_str(MARK);
    out
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

fn byte_len(s: &str) -> usize {
    s.len()
}

/// Telegram counts a message's 4096 characters in UTF-16 units: an emoji is
/// two. Plain text (no parse_mode), so nothing in it needs escaping.
const TELEGRAM_MAX: usize = 4000;

fn telegram_text(msg: &Message, with_detail: bool) -> String {
    let head = format!("{}\n\n{}", subject_line(msg), body_head(msg, with_detail));
    let detail = if with_detail { msg.detail.as_ref() } else { None };
    fit(&head, detail, TELEGRAM_MAX, utf16_len)
}

/// Discord rejects content over 2000 characters.
const WEBHOOK_TEXT_MAX: usize = 1900;

fn webhook_payload(msg: &Message, with_detail: bool) -> serde_json::Value {
    let head = format!("{}\n{}", subject_line(msg), body_head(msg, with_detail));
    let detail = if with_detail { msg.detail.as_ref() } else { None };
    let text = fit(&head, detail, WEBHOOK_TEXT_MAX, byte_len);
    serde_json::json!({
        "event": msg.event,
        "host": host(),
        "subject": subject_line(msg),
        "message": render_text(msg, with_detail),
        "since": msg.since.to_rfc3339(),
        "link": msg.look.as_ref().map(|(_, path)| panel_url(path)),
        "text": text,
        "content": text,
    })
}

/// A header value as RFC 2047 encoded words when it is not plain ASCII.
fn encode_header(value: &str) -> String {
    use base64::Engine;
    if value.chars().all(|c| (' '..='~').contains(&c)) {
        return value.to_string();
    }
    let mut words = Vec::new();
    let mut chunk = String::new();
    for c in value.chars() {
        if chunk.len() + c.len_utf8() > 45 {
            words.push(std::mem::take(&mut chunk));
        }
        chunk.push(c);
    }
    if !chunk.is_empty() {
        words.push(chunk);
    }
    words
        .iter()
        .map(|w| format!("=?UTF-8?B?{}?=", base64::engine::general_purpose::STANDARD.encode(w)))
        .collect::<Vec<_>>()
        .join("\n ")
}

/// A line of an email body stays under SMTP's 998 octets.
const EMAIL_LINE_MAX: usize = 900;

/// The message as `sendmail -t` reads it.
fn email_text(msg: &Message, from: Option<&str>, to: &str) -> String {
    let mut out = String::new();
    if let Some(f) = from {
        out.push_str(&format!("From: \"{}\" <{f}>\n", host()));
    }
    out.push_str(&format!("To: {to}\n"));
    out.push_str(&format!("Subject: {}\n", encode_header(&subject_line(msg))));
    out.push_str("MIME-Version: 1.0\nContent-Type: text/plain; charset=utf-8\nContent-Transfer-Encoding: 8bit\n");
    // Out-of-office replies and the like are not sent back to an automatic
    // message (RFC 3834).
    out.push_str("Auto-Submitted: auto-generated\n");
    out.push_str(&format!("X-Panel-Event: {}\n\n", msg.event));
    for line in render_text(msg, true).lines() {
        out.push_str(&cut_end(line, EMAIL_LINE_MAX, byte_len));
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------------
// Channels
// ---------------------------------------------------------------------------

/// One row of `notifications`. `id` is None for a test of a channel that is
/// not saved.
#[derive(Clone, Debug)]
struct Channel {
    id: Option<i64>,
    channel: String,
    target: String,
    events: String,
    /// A Telegram channel's bot token.
    secret: Option<String>,
    /// The channel gets log lines. Email always does: it is sent from this
    /// server, as the logs are kept on it.
    log_lines: bool,
}

impl Channel {
    fn with_detail(&self) -> bool {
        self.channel == "email" || self.log_lines
    }
}

/// An empty event list subscribes to everything, events added by later
/// versions included.
fn subscribes(events: &str, event: &str) -> bool {
    let mut list = events.split(',').map(str::trim).filter(|e| !e.is_empty()).peekable();
    list.peek().is_none() || list.any(|e| e == event)
}

fn read_channels(conn: &Connection, id: Option<i64>) -> rusqlite::Result<Vec<Channel>> {
    let row = |r: &rusqlite::Row<'_>| {
        Ok(Channel {
            id: Some(r.get(0)?),
            channel: r.get(1)?,
            target: r.get(2)?,
            events: r.get(3)?,
            secret: r.get(4)?,
            log_lines: r.get::<_, Option<i64>>(5)?.unwrap_or(1) != 0,
        })
    };
    let filter = if id.is_some() { " WHERE id = ?1" } else { "" };
    // log_lines comes with the panel's migration; until the panel has run it
    // every channel gets them, as before.
    let with = format!("SELECT id, channel, target, events, secret, log_lines FROM notifications{filter}");
    let without = format!("SELECT id, channel, target, events, secret, 1 FROM notifications{filter}");
    let mut stmt = match conn.prepare(&with) {
        Ok(s) => s,
        Err(_) => conn.prepare(&without)?,
    };
    let rows = match id {
        Some(id) => stmt.query_map([id], row)?.collect(),
        None => stmt.query_map([], row)?.collect(),
    };
    rows
}

fn is_plain_address(a: &str) -> bool {
    a.contains('@') && !a.contains([',', ' ', '<', '>', '"', '\r', '\n'])
}

/// `notifications.email_from`: the mailbox notification email is sent as
/// (From header and envelope sender), so the tenant domain's SPF and DKIM
/// cover it and bounces land in that mailbox. Unset, Postfix sends as
/// root@<myhostname>: no SPF record covers that name and its bounces are
/// lost.
fn email_from(db: &PanelDb) -> Option<String> {
    db.with(|conn| {
        conn.query_row(
            "SELECT value FROM settings WHERE key = 'notifications.email_from'",
            [],
            |r| r.get::<_, String>(0),
        )
    })
    .ok()
    .map(|v| v.trim().to_string())
    .filter(|v| is_plain_address(v))
}

/// `<digits>:<key>`, the form @BotFather hands out.
fn is_bot_token(t: &str) -> bool {
    t.split_once(':').is_some_and(|(id, key)| {
        !id.is_empty()
            && id.bytes().all(|b| b.is_ascii_digit())
            && key.len() >= 20
            && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    })
}

/// A numeric Telegram user ID (negative for a group).
fn is_chat_id(t: &str) -> bool {
    let digits = t.strip_prefix('-').unwrap_or(t);
    (1..=20).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit())
}

/// Why a channel cannot be sent to as configured.
fn check_channel(ch: &Channel) -> Result<(), String> {
    let target = ch.target.trim();
    if target.is_empty() || target.contains(['\r', '\n']) {
        return Err("invalid notification target".into());
    }
    match ch.channel.as_str() {
        "email" if !is_plain_address(target) => Err(format!("not an email address: {target}")),
        "email" => Ok(()),
        "telegram" => {
            if !is_bot_token(ch.secret.as_deref().unwrap_or("").trim()) {
                Err("missing or malformed Telegram bot token".into())
            } else if !is_chat_id(target) {
                Err(format!("not a Telegram user ID: {target}"))
            } else {
                Ok(())
            }
        }
        "webhook" => {
            if !(target.starts_with("https://") || target.starts_with("http://")) {
                Err(format!("not an http(s) URL: {target}"))
            } else if target.chars().any(|c| c.is_control()) {
                // The URL is written into a curl config file, one option per
                // line: a line break in it would start a second option.
                Err("the webhook URL must not contain line breaks or control characters".into())
            } else {
                Ok(())
            }
        }
        other => Err(format!("unknown channel {other}")),
    }
}

// ---------------------------------------------------------------------------
// Delivery
// ---------------------------------------------------------------------------

/// A delivery that did not work. `retry`: worth another try after at least
/// this long (the far end was unreachable or busy); None when trying again
/// cannot help (a refused token, a wrong address, a redirect).
#[derive(Debug, PartialEq)]
struct Fail {
    why: String,
    retry: Option<Duration>,
}

impl Fail {
    fn permanent(why: impl Into<String>) -> Self {
        Fail { why: why.into(), retry: None }
    }
    fn transient(why: impl Into<String>) -> Self {
        Fail { why: why.into(), retry: Some(Duration::ZERO) }
    }
}

/// Delivered, with a note when it is not on its way yet.
type Attempt = Result<Option<String>, Fail>;

/// The waits between tries of one delivery: three tries in about 6 minutes.
const RETRY_DELAYS: &[Duration] = &[Duration::from_secs(30), Duration::from_secs(300)];
/// The longest a channel's own "retry after" is waited for.
const RETRY_MAX: Duration = Duration::from_secs(600);
/// One try, at most.
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(60);

/// curl's exit codes for a network that did not get through: resolve,
/// connect, timeout, TLS handshake, empty reply, send, receive.
fn curl_transient(code: i32) -> bool {
    matches!(code, 5 | 6 | 7 | 28 | 35 | 52 | 55 | 56)
}

fn exec_fail(e: AgentError) -> Fail {
    match e {
        AgentError::Subprocess { cmd, code, stderr } => {
            let why = format!("{cmd}: {}", stderr.trim());
            if cmd == "curl" && !curl_transient(code) {
                Fail::permanent(why)
            } else {
                Fail::transient(why)
            }
        }
        other => Fail::transient(other.to_string()),
    }
}

async fn attempt(ch: &Channel, from: Option<&str>, msg: &Message) -> Attempt {
    check_channel(ch).map_err(Fail::permanent)?;
    let run = async {
        match ch.channel.as_str() {
            "email" => send_email(ch.target.trim(), from, msg).await,
            "telegram" => send_telegram(ch, msg).await,
            _ => send_webhook(ch, msg).await,
        }
    };
    match tokio::time::timeout(ATTEMPT_TIMEOUT, run).await {
        Ok(r) => r,
        Err(_) => Err(Fail::transient(format!("no answer within {} s", ATTEMPT_TIMEOUT.as_secs()))),
    }
}

/// Email success means Postfix took the message; it delivers (and retries)
/// on its own. With Postfix stopped, `sendmail` still leaves the message in
/// its queue, where it waits until Postfix runs again: said in the note.
async fn send_email(to: &str, from: Option<&str>, msg: &Message) -> Attempt {
    let text = email_text(msg, from, to);
    let mut args = vec!["-t", "-oi"];
    if let Some(f) = from {
        args.extend(["-f", f]);
    }
    exec::run_with_input("sendmail", args, &text).await.map_err(exec_fail)?;
    if exec::run("postfix", ["status"]).await.is_err() {
        return Ok(Some(
            "handed to Postfix, which is not running: it goes out once Postfix runs again".into(),
        ));
    }
    Ok(None)
}

async fn send_telegram(ch: &Channel, msg: &Message) -> Attempt {
    let token = ch.secret.as_deref().unwrap_or("").trim();
    let payload = serde_json::json!({
        "chat_id": ch.target.trim(),
        "text": telegram_text(msg, ch.with_detail()),
        "disable_web_page_preview": true,
    });
    // The token is part of the URL, so the whole request reaches curl as a
    // config on stdin: argv shows in every user's process list.
    let config = format!(
        "url = \"https://api.telegram.org/bot{token}/sendMessage\"\n\
         header = \"Content-Type: application/json\"\n\
         data-binary = \"{}\"\n",
        curl_quote(&payload.to_string())
    );
    // No -f: a refusal comes back as {"ok":false,"description":...}, and
    // "chat not found" (the user never pressed Start in the bot) is worth
    // showing as is.
    let out = exec::run_with_input("curl", ["-sS", "-m", "15", "-K", "-"], &config)
        .await
        .map_err(exec_fail)?;
    telegram_answer(&out.stdout)
}

fn telegram_answer(body: &str) -> Attempt {
    let resp: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    if resp["ok"] == true {
        return Ok(None);
    }
    let why = format!(
        "Telegram: {}",
        resp["description"].as_str().unwrap_or("no answer from api.telegram.org")
    );
    let code = resp["error_code"].as_i64().unwrap_or(0);
    if code == 429 {
        let after = resp["parameters"]["retry_after"].as_u64().unwrap_or(30);
        return Err(Fail { why, retry: Some(Duration::from_secs(after)) });
    }
    if code >= 500 || code == 0 {
        return Err(Fail::transient(why));
    }
    Err(Fail::permanent(why))
}

async fn send_webhook(ch: &Channel, msg: &Message) -> Attempt {
    let payload = webhook_payload(msg, ch.with_detail());
    // A Slack or Discord webhook URL is the whole credential: whoever has it
    // can post to the channel. Like the Telegram token it stays off argv
    // (the process list); curl reads it from a root-only config file, and
    // the payload from stdin. -g: the URL is used as typed, where curl took
    // {a,b} in it as a list of URLs. No -f and no -L: the answer's status is
    // read below, and a redirect is not followed with the message.
    let url_file = curl_url_file(ch.target.trim()).map_err(|e| Fail::transient(e.to_string()))?;
    let url_config = url_file.path().to_string_lossy().into_owned();
    let out = exec::run_with_input(
        "curl",
        [
            "-sS", "-g", "-m", "15", "-X", "POST",
            "-H", "Content-Type: application/json",
            "--data-binary", "@-", "-K", url_config.as_str(),
            "-w", "\n%{http_code} %{redirect_url}",
        ],
        &payload.to_string(),
    )
    .await
    .map_err(exec_fail)?;
    webhook_answer(&out.stdout)
}

/// The answer to a webhook POST: curl's output ending in a line with the
/// status code and the redirect target.
fn webhook_answer(out: &str) -> Attempt {
    let (body, status) = out.rsplit_once('\n').unwrap_or(("", out));
    let mut parts = status.trim().splitn(2, ' ');
    let code: u16 = parts.next().unwrap_or("").parse().unwrap_or(0);
    let location = parts.next().unwrap_or("").trim();
    let said: String = body.trim().chars().take(200).collect();
    let said = if said.is_empty() { String::new() } else { format!(": {said}") };
    match code {
        200..=299 => Ok(None),
        300..=399 => Err(Fail::permanent(format!(
            "the webhook answered {code} (moved{}); save the address it points to",
            if location.is_empty() { String::new() } else { format!(" to {location}") }
        ))),
        429 | 500..=599 => Err(Fail::transient(format!("the webhook answered {code}{said}"))),
        0 => Err(Fail::transient("no answer from the webhook".to_string())),
        _ => Err(Fail::permanent(format!("the webhook refused the message ({code}){said}"))),
    }
}

/// `s` inside a double-quoted value of a curl config file.
fn curl_quote(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// A curl config (`-K`) that holds only the URL, in a file only root reads.
/// /run is RAM; the file goes when the returned handle drops.
fn curl_url_file(url: &str) -> std::io::Result<tempfile::NamedTempFile> {
    use std::io::Write;
    let dir = std::path::Path::new("/run/vps-panel");
    let mut b = tempfile::Builder::new();
    b.prefix("notify-url-");
    // 0600 by default.
    let mut f = if dir.is_dir() { b.tempfile_in(dir)? } else { b.tempfile()? };
    writeln!(f, "url = \"{}\"", curl_quote(url))?;
    f.flush()?;
    Ok(f)
}

/// How long to wait before try number `tried + 1`, or None to give up.
fn next_wait(tried: usize, fail: &Fail, delays: &[Duration]) -> Option<Duration> {
    let asked = fail.retry?;
    let ours = *delays.get(tried.checked_sub(1)?)?;
    Some(ours.max(asked.min(RETRY_MAX)))
}

/// Deliver `msg` to `ch`, trying again after `delays` while the failure is
/// one that can pass.
async fn deliver(ch: &Channel, from: Option<&str>, msg: &Message, delays: &[Duration]) -> Result<Option<String>, String> {
    let mut tried = 0;
    loop {
        tried += 1;
        match attempt(ch, from, msg).await {
            Ok(note) => return Ok(note),
            Err(fail) => match next_wait(tried, &fail, delays) {
                Some(wait) => tokio::time::sleep(wait).await,
                None => {
                    let tries = if tried > 1 { format!(" (after {tried} tries)") } else { String::new() };
                    return Err(format!("{}{tries}", fail.why));
                }
            },
        }
    }
}

/// Leave the outcome of a delivery on the channel's row, for the
/// Notifications page. Nothing is sent about a failed delivery: a channel
/// that cannot be reached would only report itself to itself.
fn record(conn: &Connection, id: i64, event: &str, result: &Result<Option<String>, String>) {
    let (ok, detail) = match result {
        Ok(note) => (1, note.clone()),
        Err(why) => (0, Some(why.clone())),
    };
    // The columns come with the panel's migration; before it this fails and
    // only the page's line is missing.
    let _ = conn.execute(
        "UPDATE notifications SET last_at = ?1, last_event = ?2, last_ok = ?3, last_detail = ?4 WHERE id = ?5",
        params![Utc::now().to_rfc3339(), event, ok, detail, id],
    );
}

/// Send `msg` to every channel subscribed to its event, in the background:
/// the caller does not wait, and a slow channel does not hold up another.
pub fn notify(db: &PanelDb, msg: Message) {
    debug_assert!(EVENTS.contains(&msg.event), "{} is not in EVENTS", msg.event);
    let Ok(rt) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let db = db.clone();
    rt.spawn(async move {
        let channels: Vec<Channel> = db
            .with(|conn| read_channels(conn, None))
            .unwrap_or_default()
            .into_iter()
            .filter(|c| subscribes(&c.events, msg.event))
            .collect();
        if channels.is_empty() {
            return;
        }
        let from = email_from(&db);
        for ch in channels {
            let (db, msg, from) = (db.clone(), msg.clone(), from.clone());
            tokio::spawn(async move {
                let result = deliver(&ch, from.as_deref(), &msg, RETRY_DELAYS).await;
                if let Err(e) = &result {
                    tracing::warn!(channel = %ch.channel, event = msg.event, error = %e, "notification not delivered");
                }
                if let Some(id) = ch.id {
                    let _ = db.with(|conn| {
                        record(conn, id, msg.event, &result);
                        Ok(())
                    });
                }
            });
        }
    });
}

// ---------------------------------------------------------------------------
// What was reported (notify_state)
// ---------------------------------------------------------------------------

/// The table behind "once per occurrence". `reported`: 0 seen but not yet
/// reported (waiting for a second look), 1 reported, 2 already so when the
/// agent first looked (never reported, so its end is not either). `value`
/// holds a scan's position (the OOM watch).
pub fn ensure_schema(db: &PanelDb) -> AgentResult<()> {
    db.with(|conn| {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS notify_state (
                key       TEXT PRIMARY KEY,
                since     TEXT NOT NULL,
                seen      INTEGER NOT NULL DEFAULT 1,
                reported  INTEGER NOT NULL DEFAULT 0,
                value     TEXT
            );",
        )
    })
}

fn parse_time(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).map(|d| d.with_timezone(&Utc)).unwrap_or_else(|_| Utc::now())
}

/// Record `key` as reported. True the first time, false while it stands.
fn mark_once(conn: &Connection, key: &str, now: DateTime<Utc>) -> rusqlite::Result<bool> {
    Ok(conn.execute(
        "INSERT OR IGNORE INTO notify_state (key, since, reported) VALUES (?1, ?2, 1)",
        params![key, now.to_rfc3339()],
    )? == 1)
}

/// Forget `key`; returns when it was first reported, if it was.
fn unmark(conn: &Connection, key: &str) -> rusqlite::Result<Option<DateTime<Utc>>> {
    let since: Option<String> = conn
        .query_row("SELECT since FROM notify_state WHERE key = ?1", [key], |r| r.get(0))
        .optional()?;
    conn.execute("DELETE FROM notify_state WHERE key = ?1", [key])?;
    Ok(since.map(|s| parse_time(&s)))
}

/// A condition seen by one scan. `raise`: it is bad enough to report (a
/// disk at 90%); without it, it only keeps a reported condition from
/// clearing (a disk still at 87% after a report at 90%).
#[derive(Debug, Clone)]
struct Seen {
    key: String,
    raise: bool,
}

#[derive(Debug, Default, PartialEq)]
struct Changes {
    /// Conditions to report now, with when they were first seen.
    raised: Vec<(String, DateTime<Utc>)>,
    /// Reported conditions that ended, with when they started.
    cleared: Vec<(String, DateTime<Utc>)>,
}

/// Compare one scan of a family of conditions (keys starting with
/// `family`) with what is recorded. A condition is reported once it has been
/// seen in `debounce` scans in a row, and once more when it is gone, if it
/// was reported. The first scan of a family ever records what it finds
/// without reporting it: what was already so before this existed.
fn reconcile(
    conn: &Connection,
    family: &str,
    seen: &[Seen],
    debounce: i64,
    now: DateTime<Utc>,
) -> rusqlite::Result<Changes> {
    let first_look = conn.execute(
        "INSERT OR IGNORE INTO notify_state (key, since, reported) VALUES (?1, ?2, 2)",
        params![format!("baseline:{family}"), now.to_rfc3339()],
    )? == 1;
    let mut known: HashMap<String, (String, i64, i64)> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT key, since, seen, reported FROM notify_state WHERE substr(key, 1, ?1) = ?2",
        )?;
        let rows = stmt.query_map(params![family.len() as i64, family], |r| {
            Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?, r.get(3)?)))
        })?;
        for row in rows {
            let (k, v) = row?;
            known.insert(k, v);
        }
    }
    let mut changes = Changes::default();
    let mut done: HashSet<&str> = HashSet::new();
    for s in seen {
        if !s.key.starts_with(family) || !done.insert(s.key.as_str()) {
            continue;
        }
        match known.remove(&s.key) {
            Some((since, n, reported)) => {
                let n = n + 1;
                let raise = reported == 0 && s.raise && n >= debounce;
                conn.execute(
                    "UPDATE notify_state SET seen = ?1, reported = ?2 WHERE key = ?3",
                    params![n, if raise { 1 } else { reported }, s.key],
                )?;
                if raise {
                    changes.raised.push((s.key.clone(), parse_time(&since)));
                }
            }
            None if s.raise => {
                let reported = if first_look {
                    2
                } else if debounce <= 1 {
                    1
                } else {
                    0
                };
                conn.execute(
                    "INSERT INTO notify_state (key, since, seen, reported) VALUES (?1, ?2, 1, ?3)",
                    params![s.key, now.to_rfc3339(), reported],
                )?;
                if reported == 1 {
                    changes.raised.push((s.key.clone(), now));
                }
            }
            None => {}
        }
    }
    for (key, (since, _, reported)) in known {
        conn.execute("DELETE FROM notify_state WHERE key = ?1", [&key])?;
        if reported == 1 {
            changes.cleared.push((key, parse_time(&since)));
        }
    }
    changes.raised.sort();
    changes.cleared.sort();
    Ok(changes)
}

fn reconcile_db(db: &PanelDb, family: &str, seen: &[Seen], debounce: i64) -> Option<Changes> {
    match db.with(|conn| reconcile(conn, family, seen, debounce, Utc::now())) {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!(family, error = %e, "notification state not readable");
            None
        }
    }
}

/// Report `msg` once for `key`, until `resolved(key)`.
pub fn problem(db: &PanelDb, key: &str, msg: Message) {
    if let Ok(true) = db.with(|conn| mark_once(conn, &format!("problem:{key}"), msg.since)) {
        notify(db, msg);
    }
}

/// The condition reported under `key` (if it was) has ended. Silent: the
/// ones whose end is worth a message have their own event.
pub fn resolved(db: &PanelDb, key: &str) {
    let _ = db.with(|conn| unmark(conn, &format!("problem:{key}")));
}

// ---------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------

fn task_message(kind: &str, id: &str, log_tail: &str) -> Message {
    let what = describe_task(kind);
    let mut body = format!("{what} failed on {}.", host());
    if retried_daily(kind) {
        body.push_str(
            " The panel tries again every day; you hear about it again once it has worked \
             and fails anew, or when the certificate is close to expiry.",
        );
    }
    Message::new(event_for_task(kind), format!("{what} failed"), body)
        .look("Tasks", format!("/tasks/{id}"))
        .detail("Last log lines", log_tail)
}

/// A task ended failed (ipc/tasks.rs).
pub fn task_failed(db: &PanelDb, kind: &str, id: &str, log_tail: &str) {
    if retried_daily(kind) {
        match db.with(|conn| mark_once(conn, &format!("task:{kind}"), Utc::now())) {
            Ok(false) => return,
            Ok(true) | Err(_) => {}
        }
    }
    notify(db, task_message(kind, id, log_tail));
}

/// A task ended well: a daily retry that failed before is reported again
/// the next time it fails.
pub fn task_succeeded(db: &PanelDb, kind: &str) {
    if retried_daily(kind) {
        let _ = db.with(|conn| unmark(conn, &format!("task:{kind}")));
    }
}

/// At agent start, before the tasks a previous agent left running are
/// marked failed (TaskTracker::sweep_interrupted): one message per event
/// naming them. Panel updates are left out: they restart the agent on
/// purpose and are settled by system::finalize_self_update.
pub fn interrupted_tasks(db: &PanelDb) {
    let rows: Vec<(String, String)> = db
        .with(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, kind FROM tasks WHERE status IN ('pending', 'running')
                   AND kind NOT LIKE 'system:self-update%' ORDER BY started_at",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect()
        })
        .unwrap_or_default();
    for msg in interrupted_messages(&rows) {
        notify(db, msg);
    }
}

fn interrupted_messages(rows: &[(String, String)]) -> Vec<Message> {
    let mut by_event: BTreeMap<&'static str, Vec<&(String, String)>> = BTreeMap::new();
    for row in rows {
        by_event.entry(event_for_task(&row.1)).or_default().push(row);
    }
    by_event
        .into_iter()
        .map(|(event, rows)| {
            let names: Vec<String> = rows.iter().map(|(_, kind)| describe_task(kind)).collect();
            let subject = if rows.len() == 1 {
                format!("{} was cut short by an agent restart", names[0])
            } else {
                format!("{} tasks were cut short by an agent restart", rows.len())
            };
            let body = format!(
                "The panel's agent on {} restarted (a crash, a reboot or a restart by hand) while \
                 this was running, so it did not finish and is marked failed:\n{}\n\nRun it again \
                 from the panel. An app whose build was cut short gets its last working build back \
                 by itself.",
                host(),
                names.iter().map(|n| format!("- {n}")).collect::<Vec<_>>().join("\n")
            );
            let path = if rows.len() == 1 { format!("/tasks/{}", rows[0].0) } else { "/tasks".to_string() };
            Message::new(event, subject, body).look("Tasks", path)
        })
        .collect()
}

/// The last `n` lines of `s`.
pub fn last_lines(s: &str, n: usize) -> &str {
    let s = s.trim_end();
    match s.rmatch_indices('\n').nth(n.saturating_sub(1)) {
        Some((i, _)) if n > 0 => &s[i + 1..],
        _ if n == 0 => "",
        _ => s,
    }
}

/// A panel update that update.sh reported failed, once the agent it
/// restarted has settled the task (system::finalize_self_update). `log` is
/// what that appends to the task.
#[allow(dead_code)] // called from handlers/system.rs
pub fn self_update_failed(db: &PanelDb, task_id: &str, log: &str) {
    let (subject, body) = if log.contains("the update was rolled back: the previous version is running again") {
        (
            "Panel update failed and was rolled back",
            "The panel update did not come up, so update.sh put the previous version back; it runs again.",
        )
    } else if log.contains("the update was rolled back") {
        (
            "Panel update failed; the rollback did not bring everything back up",
            "The panel update did not come up and update.sh put the previous version back, but not \
             every service started again. Check them on the Services page.",
        )
    } else {
        ("Panel update failed", "update.sh did not finish the panel update.")
    };
    notify(
        db,
        Message::new("task.fail", subject, format!("{body} Server: {}.", host()))
            .look("Tasks", format!("/tasks/{task_id}"))
            .detail("Last lines of the update log", last_lines(log, 15)),
    );
}

// ---------------------------------------------------------------------------
// Background jobs that report here (one-line calls in their handlers)
// ---------------------------------------------------------------------------

/// A certificate in use, from the renewal loop (handlers/ssl.rs): reported
/// when it is 14 days from expiry or closer, again only after a renewal
/// brought it back over 30.
pub fn cert_expiry(db: &PanelDb, domain: &str, days: i64) {
    let key = format!("cert.expiry:{domain}");
    if days >= 30 {
        resolved(db, &key);
    } else if days <= 14 {
        let when = if days < 0 {
            format!("expired {} days ago", -days)
        } else {
            format!("expires in {days} days")
        };
        problem(
            db,
            &key,
            Message::new(
                "ssl.fail",
                format!("Certificate for {domain} {when}"),
                format!(
                    "The certificate for {domain} on {} {when}, and automatic renewal has not \
                     replaced it. Browsers and mail clients warn about it once it has expired. \
                     The last SSL task on the Tasks page says why renewal failed; Renew on the \
                     Domains page tries again.",
                    host()
                ),
            )
            .look("Domains", "/domains"),
        );
    }
}

/// The renewal loop could not start the order of a mail certificate
/// (handlers/ssl.rs): reported once, until it starts.
pub fn cert_cannot_start(db: &PanelDb, tenant: &str, why: &str) {
    problem(
        db,
        &format!("cert.start:{tenant}"),
        Message::new(
            "ssl.fail",
            format!("Mail certificate for mail.{tenant} cannot be renewed"),
            format!(
                "The certificate mail.{tenant} uses is close to expiry, and its renewal could not \
                 start on {}: the web server setup it needs failed. The panel tries again every day.",
                host()
            ),
        )
        .look("Domains", "/domains")
        .detail("Error", why),
    );
}

pub fn cert_started(db: &PanelDb, tenant: &str) {
    resolved(db, &format!("cert.start:{tenant}"));
}

/// The result of applying Settings > Privacy (at agent start and daily,
/// handlers/privacy.rs): what is not in effect, reported once.
pub fn privacy_applied(db: &PanelDb, problems: Vec<String>) {
    if problems.is_empty() {
        resolved(db, "privacy");
        return;
    }
    problem(
        db,
        "privacy",
        Message::new(
            "task.fail",
            "Privacy settings not fully in effect",
            format!(
                "Applying Settings > Privacy (log format and how long logs are kept) did not \
                 fully work on {}. Settings > Privacy shows which part.",
                host()
            ),
        )
        .look("Settings > Privacy", "/settings")
        .detail("What failed", problems.join("\n")),
    );
}

/// Postfix could not be brought in line with the mail IP settings at agent
/// start (handlers/mail_ip.rs); the change was undone.
#[allow(dead_code)] // called from handlers/mail_ip.rs
pub fn mail_settings_failed(db: &PanelDb, why: &str) {
    problem(
        db,
        "mail.settings",
        Message::new(
            "mail.fail",
            "Outgoing mail settings could not be applied",
            format!(
                "Postfix on {} could not be brought in line with the outgoing mail settings \
                 (which address mail leaves from). What was changed was put back, so mail goes \
                 out as before. Save the setting on Mailboxes again to see the error.",
                host()
            ),
        )
        .look("Mailboxes", "/mailboxes")
        .detail("Error", why),
    );
}

#[allow(dead_code)] // called from handlers/mail_ip.rs
pub fn mail_settings_applied(db: &PanelDb) {
    resolved(db, "mail.settings");
}

/// Outgoing mail sent over IPv6 and, at agent start, no longer can (the
/// address is gone): it leaves over IPv4 now (handlers/mail_ip.rs).
#[allow(dead_code)] // called from handlers/mail_ip.rs
pub fn mail_ipv6_dropped(db: &PanelDb, address: &str) {
    problem(
        db,
        "mail.ipv6",
        Message::new(
            "mail.fail",
            "Outgoing mail switched to IPv4 only",
            format!(
                "Mail on {} was sent over IPv6 from {address}, which is no longer usable here, so \
                 Postfix now sends over IPv4 only. Mail still goes out. If the server's IPv6 \
                 address changed, check the DNS records that publish it.",
                host()
            ),
        )
        .look("Mailboxes", "/mailboxes"),
    );
}

#[allow(dead_code)] // called from handlers/mail_ip.rs
pub fn mail_ipv6_back(db: &PanelDb) {
    resolved(db, "mail.ipv6");
}

// ---------------------------------------------------------------------------
// The monitor
// ---------------------------------------------------------------------------

/// Every 5 minutes: the conditions below. Each is reported once when it
/// starts; services, disks and the mail queue once more when they are fine
/// again.
pub async fn monitor_loop(db: PanelDb) {
    loop {
        check_failed_units(&db).await;
        check_required_services(&db).await;
        check_disks(&db).await;
        check_mail_queue(&db).await;
        check_oom(&db).await;
        tokio::time::sleep(Duration::from_secs(300)).await;
    }
}

fn url_param(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

async fn active_state(unit: &str) -> String {
    exec::run("systemctl", ["show", "-p", "ActiveState", "--value", "--", unit])
        .await
        .map(|o| o.stdout.trim().to_string())
        .unwrap_or_default()
}

const FAILED: &str = "unit.failed:";

/// systemd units in the failed state (a crash-looping app lands there once
/// it hits its start limit).
async fn check_failed_units(db: &PanelDb) {
    let Ok(out) = exec::run(
        "systemctl",
        ["list-units", "--state=failed", "--plain", "--no-legend", "--no-pager"],
    )
    .await
    else {
        return;
    };
    let seen: Vec<Seen> = out
        .stdout
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .map(|unit| Seen { key: format!("{FAILED}{unit}"), raise: true })
        .collect();
    let Some(changes) = reconcile_db(db, FAILED, &seen, 1) else {
        return;
    };
    for (key, since) in changes.raised {
        let unit = &key[FAILED.len()..];
        let journal = exec::run("journalctl", ["-u", unit, "-n", "20", "--no-pager", "-o", "short-iso"])
            .await
            .map(|o| o.stdout)
            .unwrap_or_default();
        notify(
            db,
            Message::new(
                "service.fail",
                format!("{unit} failed"),
                format!(
                    "{unit} stopped in the failed state on {}: systemd does not start it again by \
                     itself. Start it on the Services page once the cause is fixed; the Logs page \
                     has its journal.",
                    host()
                ),
            )
            .since(since)
            .look("Logs", format!("/logs?unit={}", url_param(unit)))
            .detail("Last journal lines", journal),
        );
    }
    for (key, since) in changes.cleared {
        let unit = &key[FAILED.len()..];
        let now = match active_state(unit).await.as_str() {
            "active" | "reloading" => "it runs again".to_string(),
            "activating" => "it is starting".to_string(),
            "" => "it is gone".to_string(),
            other => format!("it is {other} now"),
        };
        notify(
            db,
            Message::new(
                "service.ok",
                format!("{unit} is no longer failed"),
                format!("{unit} on {} left the failed state it entered at {}: {now}.", host(), fmt_time(since)),
            )
            .look("Services", "/services"),
        );
    }
}

/// The services the panel needs: (name, the unit that runs, the unit that
/// is enabled). Ubuntu runs Postfix as postfix@-.service under a
/// postfix.service that only groups it and reads "active" either way.
const REQUIRED: &[(&str, &str, &str)] = &[
    ("nginx", "nginx.service", "nginx.service"),
    ("Postfix", "postfix@-.service", "postfix.service"),
    ("Dovecot", "dovecot.service", "dovecot.service"),
    ("MariaDB", "mariadb.service", "mariadb.service"),
    ("PowerDNS", "pdns.service", "pdns.service"),
    ("Rspamd", "rspamd.service", "rspamd.service"),
    ("fail2ban", "fail2ban.service", "fail2ban.service"),
    ("The panel", "vps-panel.service", "vps-panel.service"),
];

const DOWN: &str = "svc.down:";

#[derive(Debug, Default, Clone, PartialEq)]
struct UnitState {
    load: String,
    active: String,
    sub: String,
    file: String,
}

/// `systemctl show` blocks, one per unit asked for, in the order asked.
fn parse_show(out: &str) -> Vec<UnitState> {
    out.split("\n\n")
        .filter(|b| !b.trim().is_empty())
        .map(|block| {
            let field = |k: &str| {
                block
                    .lines()
                    .find_map(|l| l.strip_prefix(k).and_then(|v| v.strip_prefix('=')))
                    .unwrap_or("")
                    .trim()
                    .to_string()
            };
            UnitState {
                load: field("LoadState"),
                active: field("ActiveState"),
                sub: field("SubState"),
                file: field("UnitFileState"),
            }
        })
        .collect()
}

/// How a required service is not as it should be, as a phrase, and whether
/// that is reason to report it. None: it is fine, or not installed. Failed
/// is the failed-units check's to report; here it only keeps a report of
/// "stopped" from reading as fixed.
fn service_trouble(run: &UnitState, enable: &UnitState) -> Option<(String, bool)> {
    if run.load == "not-found" || run.load.is_empty() {
        return None;
    }
    let disabled = matches!(enable.file.as_str(), "disabled" | "masked") || run.load == "masked";
    let (running, raise, what) = match (run.active.as_str(), run.sub.as_str()) {
        ("active" | "reloading", _) => (true, true, ""),
        ("activating", "auto-restart") => (false, true, "keeps crashing and restarting"),
        ("activating" | "deactivating", _) => (false, false, "is starting or stopping"),
        ("failed", _) => (false, false, "has failed"),
        _ => (false, true, "is stopped"),
    };
    match (running, disabled) {
        (true, false) => None,
        (true, true) => Some(("is disabled: it does not start again after a reboot".to_string(), true)),
        (false, true) => Some((format!("{what} and disabled"), raise)),
        (false, false) => Some((what.to_string(), raise)),
    }
}

/// The services the panel needs, stopped (not failed: a clean stop, or
/// crashing over and over) or disabled. Seen twice in a row before it is
/// reported, so a restart (a panel update restarts the panel) is not.
async fn check_required_services(db: &PanelDb) {
    let mut units: Vec<&str> = Vec::new();
    for (_, run, enable) in REQUIRED {
        units.push(run);
        units.push(enable);
    }
    let mut args = vec!["show", "--no-pager", "-p", "LoadState,ActiveState,SubState,UnitFileState", "--"];
    args.extend(units.iter().copied());
    let Ok(out) = exec::run("systemctl", args).await else {
        return;
    };
    let states = parse_show(&out.stdout);
    if states.len() != units.len() {
        return;
    }
    let mut trouble: HashMap<String, (&str, String)> = HashMap::new();
    let mut seen = Vec::new();
    let mut installed: HashMap<String, bool> = HashMap::new();
    for (i, (name, run, _)) in REQUIRED.iter().enumerate() {
        let (run_state, enable_state) = (&states[2 * i], &states[2 * i + 1]);
        let key = format!("{DOWN}{run}");
        installed.insert(key.clone(), run_state.load != "not-found");
        if let Some((what, raise)) = service_trouble(run_state, enable_state) {
            seen.push(Seen { key: key.clone(), raise });
            trouble.insert(key, (name, what));
        }
    }
    let Some(changes) = reconcile_db(db, DOWN, &seen, 2) else {
        return;
    };
    for (key, since) in changes.raised {
        let Some((name, what)) = trouble.get(&key) else { continue };
        let unit = &key[DOWN.len()..];
        notify(
            db,
            Message::new(
                "service.fail",
                format!("{name} {}", what.split(':').next().unwrap_or(what)),
                format!(
                    "{name} ({unit}) on {} {what}. The panel needs it running. Start or enable it \
                     on the Services page; the Logs page has its journal.",
                    host()
                ),
            )
            .since(since)
            .look("Services", "/services"),
        );
    }
    for (key, since) in changes.cleared {
        // Uninstalled since: nothing to say.
        if !installed.get(&key).copied().unwrap_or(false) {
            continue;
        }
        let unit = &key[DOWN.len()..];
        let name = REQUIRED.iter().find(|r| r.1 == unit).map(|r| r.0).unwrap_or(unit);
        notify(
            db,
            Message::new(
                "service.ok",
                format!("{name} runs again"),
                format!(
                    "{name} ({unit}) on {} runs and is enabled again (reported at {}).",
                    host(),
                    fmt_time(since)
                ),
            )
            .look("Services", "/services"),
        );
    }
}

const DISK: &str = "disk:";
const DISK_RAISE: u32 = 90;
const DISK_CLEAR: u32 = 85;

/// One filesystem from `df --output=source,fstype,pcent,ipcent,target`.
#[derive(Debug, PartialEq)]
struct Fs {
    mount: String,
    used: Option<u32>,
    inodes: Option<u32>,
}

/// The real filesystems, each once (a bind mount of one is the same disk).
fn parse_df(out: &str) -> Vec<Fs> {
    let mut by_source: BTreeMap<String, Fs> = BTreeMap::new();
    for line in out.lines().skip(1) {
        let mut parts = line.split_whitespace();
        let (Some(source), Some(_fstype), Some(pcent), Some(ipcent)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let mount = parts.collect::<Vec<_>>().join(" ");
        if mount.is_empty() || !mount.starts_with('/') {
            continue;
        }
        let pct = |s: &str| s.trim_end_matches('%').parse::<u32>().ok();
        let fs = Fs { mount, used: pct(pcent), inodes: pct(ipcent) };
        match by_source.get(source) {
            Some(have) if have.mount.len() <= fs.mount.len() => {}
            _ => {
                by_source.insert(source.to_string(), fs);
            }
        }
    }
    let mut list: Vec<Fs> = by_source.into_values().collect();
    list.sort_by(|a, b| a.mount.cmp(&b.mount));
    list
}

/// Disks of the kinds a server writes to: not RAM, not snap images or
/// container layers (always full or a copy of a real disk).
const DF_ARGS: &[&str] = &[
    "--output=source,fstype,pcent,ipcent,target",
    "-l",
    "-x", "tmpfs", "-x", "devtmpfs", "-x", "squashfs", "-x", "overlay",
    "-x", "efivarfs", "-x", "ramfs", "-x", "iso9660", "-x", "nsfs",
];

/// Every disk (the root one, /var, a mail volume) at 90% of its space or
/// of its inodes, and back under 85%.
async fn check_disks(db: &PanelDb) {
    let out = match exec::run("df", DF_ARGS).await {
        Ok(o) => o.stdout,
        // One unreadable mount fails the whole listing: the root disk alone.
        Err(_) => match exec::run("df", ["--output=source,fstype,pcent,ipcent,target", "/"]).await {
            Ok(o) => o.stdout,
            Err(_) => return,
        },
    };
    let disks = parse_df(&out);
    let mut seen = Vec::new();
    let mut now_pct: HashMap<String, u32> = HashMap::new();
    for fs in &disks {
        for (what, pct) in [("space", fs.used), ("inodes", fs.inodes)] {
            let Some(pct) = pct else { continue };
            let key = format!("{DISK}{what}:{}", fs.mount);
            now_pct.insert(key.clone(), pct);
            if pct >= DISK_CLEAR {
                seen.push(Seen { key, raise: pct >= DISK_RAISE });
            }
        }
    }
    let Some(changes) = reconcile_db(db, DISK, &seen, 1) else {
        return;
    };
    for (key, since) in changes.raised {
        let pct = now_pct.get(&key).copied().unwrap_or(0);
        let Some((what, mount)) = key[DISK.len()..].split_once(':') else { continue };
        let msg = if what == "space" {
            Message::new(
                "disk.full",
                format!("Disk {mount} is {pct}% full"),
                format!(
                    "The filesystem at {mount} on {} is {pct}% full. Databases, mail and deploys \
                     start failing when it fills up. The Dashboard shows what takes the space.",
                    host()
                ),
            )
        } else {
            Message::new(
                "disk.full",
                format!("Disk {mount}: {pct}% of its inodes used"),
                format!(
                    "The filesystem at {mount} on {} has used {pct}% of its inodes, the number of \
                     files it can hold. When they run out no file can be created, with space \
                     free: usually a cache, session or mail folder with very many small files.",
                    host()
                ),
            )
        };
        notify(db, msg.since(since).look("Dashboard", "/dashboard"));
    }
    for (key, since) in changes.cleared {
        let Some((what, mount)) = key[DISK.len()..].split_once(':') else { continue };
        let now = match now_pct.get(&key) {
            Some(p) if what == "space" => format!("{p}% full"),
            Some(p) => format!("{p}% of its inodes used"),
            None => "no longer mounted".to_string(),
        };
        notify(
            db,
            Message::new(
                "disk.ok",
                format!("Disk {mount} is {now}"),
                format!("The filesystem at {mount} on {} is {now} (reported at {}).", host(), fmt_time(since)),
            )
            .look("Dashboard", "/dashboard"),
        );
    }
}

const MAILQ: &str = "mail.queue";

/// What `postqueue -j` says about the queue.
#[derive(Debug, Default, PartialEq)]
struct Queue {
    /// Messages waiting (not the ones put on hold by hand).
    waiting: usize,
    /// Deferred for longer than MAILQ_OLD.
    old: usize,
    oldest: Option<i64>,
    /// Postfix's reasons, most frequent first, with how many recipients.
    reasons: Vec<(String, usize)>,
}

const MAILQ_OLD: i64 = 6 * 3600;

fn parse_queue(out: &str, now: i64) -> Queue {
    let mut q = Queue::default();
    let mut reasons: HashMap<String, usize> = HashMap::new();
    for line in out.lines() {
        let Ok(m) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let queue = m["queue_name"].as_str().unwrap_or("");
        if queue == "hold" {
            continue;
        }
        q.waiting += 1;
        let arrival = m["arrival_time"].as_i64();
        if let Some(t) = arrival {
            q.oldest = Some(q.oldest.map_or(t, |o| o.min(t)));
        }
        if queue == "deferred" && arrival.is_some_and(|t| now - t > MAILQ_OLD) {
            q.old += 1;
        }
        for r in m["recipients"].as_array().into_iter().flatten() {
            if let Some(why) = r["delay_reason"].as_str().filter(|w| !w.is_empty()) {
                *reasons.entry(why.to_string()).or_default() += 1;
            }
        }
    }
    let mut reasons: Vec<(String, usize)> = reasons.into_iter().collect();
    reasons.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    reasons.truncate(5);
    q.reasons = reasons;
    q
}

/// Reported at 50 messages waiting or 3 deferred for over 6 hours; over
/// once under 25 waiting with at most one that old.
fn queue_seen(q: &Queue) -> Option<Seen> {
    let raise = q.waiting >= 50 || q.old >= 3;
    let keep = q.waiting >= 25 || q.old >= 2;
    (raise || keep).then(|| Seen { key: format!("{MAILQ}:postfix"), raise })
}

/// Outgoing mail piling up in Postfix's queue or deferred for hours.
async fn check_mail_queue(db: &PanelDb) {
    // Not installed or not running: nothing to read (a stopped Postfix is
    // the services check's to report), and what was reported stands.
    let Ok(out) = exec::run("postqueue", ["-j"]).await else {
        return;
    };
    let q = parse_queue(&out.stdout, Utc::now().timestamp());
    let seen: Vec<Seen> = queue_seen(&q).into_iter().collect();
    let Some(changes) = reconcile_db(db, MAILQ, &seen, 1) else {
        return;
    };
    for (_, since) in changes.raised {
        let oldest = q
            .oldest
            .and_then(|t| DateTime::from_timestamp(t, 0))
            .map(|t| format!(" The oldest has waited since {}.", fmt_time(t)))
            .unwrap_or_default();
        let reasons: Vec<String> = q.reasons.iter().map(|(why, n)| format!("{n} x {why}")).collect();
        notify(
            db,
            Message::new(
                "mail.fail",
                format!("Mail is stuck: {} messages in the queue", q.waiting),
                format!(
                    "{} messages wait in Postfix's queue on {}, {} of them deferred for over 6 \
                     hours: mail to those recipients is not getting through.{oldest} Track delivery \
                     on Mailboxes shows a receiver's answer; the Logs page has Postfix's journal \
                     (postfix@-.service).",
                    q.waiting,
                    host(),
                    q.old
                ),
            )
            .since(since)
            .look("Mailboxes", "/mailboxes")
            .detail("Postfix's reasons", reasons.join("\n")),
        );
    }
    for (_, since) in changes.cleared {
        notify(
            db,
            Message::new(
                "mail.ok",
                "The mail queue has cleared",
                format!(
                    "Postfix's queue on {} is down to {} messages, none stuck for hours (reported at {}).",
                    host(),
                    q.waiting,
                    fmt_time(since)
                ),
            )
            .look("Mailboxes", "/mailboxes"),
        );
    }
}

const OOM_CURSOR: &str = "oom.cursor";
const OOM_PENDING: &str = "oom.pending";
const OOM_SENT: &str = "oom.sent";
/// At most one OOM message an hour; kills in between go in the next one.
const OOM_EVERY: i64 = 3600;

/// The processes the kernel's OOM killer killed, from `journalctl -o json`
/// lines after `after_us`, and the newest entry's time.
fn parse_oom(out: &str, after_us: i64) -> (Vec<String>, i64) {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"(?:Out of memory|Memory cgroup out of memory): Killed process \d+ \(([^)]*)\)").unwrap()
    });
    let mut killed = Vec::new();
    let mut newest = after_us;
    for line in out.lines() {
        let Ok(e) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let Some(ts) = e["__REALTIME_TIMESTAMP"].as_str().and_then(|s| s.parse::<i64>().ok()) else {
            continue;
        };
        if ts <= after_us {
            continue;
        }
        newest = newest.max(ts);
        if let Some(c) = e["MESSAGE"].as_str().and_then(|m| re.captures(m)) {
            killed.push(c[1].to_string());
        }
    }
    (killed, newest)
}

/// The kernel killed processes for want of memory. Read from the kernel's
/// journal entries since the last look (kept in notify_state, so the kill of
/// the agent itself is found by the next one); the first look ever starts
/// from now.
async fn check_oom(db: &PanelDb) {
    let now = Utc::now();
    let get = |key: &str| -> Option<(String, Option<String>)> {
        db.with(|c| {
            c.query_row("SELECT since, value FROM notify_state WHERE key = ?1", [key], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()
        })
        .ok()
        .flatten()
    };
    let put = |key: &str, since: DateTime<Utc>, value: &str| {
        let _ = db.with(|c| {
            c.execute(
                "INSERT INTO notify_state (key, since, reported, value) VALUES (?1, ?2, 1, ?3)
                 ON CONFLICT(key) DO UPDATE SET since = excluded.since, value = excluded.value",
                params![key, since.to_rfc3339(), value],
            )
        });
    };
    let Some(cursor) = get(OOM_CURSOR).and_then(|(_, v)| v?.parse::<i64>().ok()) else {
        put(OOM_CURSOR, now, &now.timestamp_micros().to_string());
        return;
    };
    let since = format!("@{}", cursor / 1_000_000);
    let Ok(out) = exec::run(
        "journalctl",
        ["_TRANSPORT=kernel", "--since", since.as_str(), "-o", "json", "--no-pager", "-q"],
    )
    .await
    else {
        return;
    };
    let (killed, newest) = parse_oom(&out.stdout, cursor);
    if newest != cursor {
        put(OOM_CURSOR, now, &newest.to_string());
    }
    // Kills not reported yet: name -> count, and the first one's time.
    let (mut first, mut pending): (DateTime<Utc>, BTreeMap<String, u32>) = match get(OOM_PENDING) {
        Some((since, Some(v))) => (parse_time(&since), serde_json::from_str(&v).unwrap_or_default()),
        _ => (now, BTreeMap::new()),
    };
    if pending.is_empty() {
        first = now;
    }
    for name in killed {
        *pending.entry(name).or_default() += 1;
    }
    if pending.is_empty() {
        return;
    }
    let last_sent = get(OOM_SENT).map(|(s, _)| parse_time(&s));
    if last_sent.is_some_and(|t| (now - t).num_seconds() < OOM_EVERY) {
        put(OOM_PENDING, first, &serde_json::to_string(&pending).unwrap_or_default());
        return;
    }
    let total: u32 = pending.values().sum();
    let list: Vec<String> = pending.iter().map(|(n, c)| if *c > 1 { format!("{n} ({c}x)") } else { n.clone() }).collect();
    notify(
        db,
        Message::new(
            "memory.oom",
            format!("Out of memory: {} killed", list.join(", ")),
            format!(
                "{} ran out of memory and the kernel killed {total} process{} to free some: {}. \
                 A service whose main process was killed restarts if it is set up to, or shows up \
                 as failed. The Dashboard shows memory use; Settings has swap.",
                host(),
                if total == 1 { "" } else { "es" },
                list.join(", ")
            ),
        )
        .since(first)
        .look("Dashboard", "/dashboard"),
    );
    put(OOM_SENT, now, "");
    let _ = db.with(|c| c.execute("DELETE FROM notify_state WHERE key = ?1", [OOM_PENDING]));
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

/// Events the panel raises itself (My Networks: a server in a network stops
/// answering, or answers again). Delivery stays here, with the channels.
const PANEL_EVENTS: &[&str] = &["network.peer_down", "network.peer_up"];

#[derive(Deserialize)]
pub struct SendReq {
    pub event: String,
    pub subject: String,
    pub body: String,
}

/// POST /notifications/send - deliver a panel-raised event to the channels
/// subscribed to it, in the background.
pub async fn send_event(
    State(state): State<AppState>,
    Json(req): Json<SendReq>,
) -> AgentResult<Json<OkResponse>> {
    let event = PANEL_EVENTS
        .iter()
        .copied()
        .find(|e| *e == req.event)
        .ok_or_else(|| AgentError::Invalid(format!("unknown event {}", req.event)))?;
    let subject: String = req.subject.chars().take(200).collect();
    let body: String = req.body.chars().take(4000).collect();
    notify(&state.panel_db, Message::new(event, subject, body).look("My Networks", "/networks"));
    Ok(Json(OkResponse::default()))
}

#[derive(Deserialize)]
pub struct TestReq {
    /// The saved channel: it is read from panel.db, as a real event reads
    /// it, and the result is recorded on it.
    #[serde(default)]
    pub id: Option<i64>,
    pub channel: String,
    pub target: String,
    /// A Telegram channel's bot token.
    #[serde(default)]
    pub secret: Option<String>,
}

fn test_message() -> Message {
    Message::new(
        "test",
        "Test notification",
        format!(
            "This is a test from the panel on {}. It went out the way a real notification does; \
             if you can read it, this channel works.",
            host()
        ),
    )
    .look("Notifications", "/notifications")
    .detail(
        "Log lines",
        "A real notification puts the last lines of the task log or the service's journal here.",
    )
}

/// POST /notifications/test - send a test message right away, through the
/// same code a real event takes (one try: the operator is waiting), and
/// report whether the channel accepted it.
pub async fn test(
    State(state): State<AppState>,
    Json(req): Json<TestReq>,
) -> AgentResult<Json<OkResponse>> {
    let saved = match req.id {
        Some(id) => state.panel_db.with(|conn| read_channels(conn, Some(id)))?.into_iter().next(),
        None => None,
    };
    let ch = saved.unwrap_or(Channel {
        id: None,
        channel: req.channel,
        target: req.target,
        events: String::new(),
        secret: req.secret,
        log_lines: false,
    });
    check_channel(&ch).map_err(AgentError::Invalid)?;
    let msg = test_message();
    let result = deliver(&ch, email_from(&state.panel_db).as_deref(), &msg, &[]).await;
    if let Some(id) = ch.id {
        state.panel_db.with(|conn| {
            record(conn, id, msg.event, &result);
            Ok(())
        })?;
    }
    result.map_err(|why| AgentError::Other(anyhow::anyhow!(why)))?;
    Ok(Json(OkResponse::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE notify_state (key TEXT PRIMARY KEY, since TEXT NOT NULL,
             seen INTEGER NOT NULL DEFAULT 1, reported INTEGER NOT NULL DEFAULT 0, value TEXT);",
        )
        .unwrap();
        c
    }

    fn seen(keys: &[(&str, bool)]) -> Vec<Seen> {
        keys.iter().map(|(k, r)| Seen { key: k.to_string(), raise: *r }).collect()
    }

    fn keys(v: &[(String, DateTime<Utc>)]) -> Vec<&str> {
        v.iter().map(|(k, _)| k.as_str()).collect()
    }

    fn msg() -> Message {
        Message::new("task.fail", "Backup failed", "Backup failed on the server.")
            .since(DateTime::parse_from_rfc3339("2026-11-24T10:03:00Z").unwrap().with_timezone(&Utc))
            .look("Tasks", "/tasks/abc")
            .detail("Last log lines", "connect to mx.example.net[192.0.2.7]:25: timed out\nuser@example.com refused")
    }

    #[test]
    fn telegram_token_and_chat_id() {
        // Shaped unlike a real token so secret scanners leave it alone.
        assert!(is_bot_token("123:example_key-example_key"));
        assert!(!is_bot_token("123456789"));
        assert!(!is_bot_token("abc:example_key-example_key"));
        assert!(!is_bot_token("123:short"));
        assert!(!is_bot_token("123:example_key\"example_key"));
        assert!(is_chat_id("123456789"));
        assert!(is_chat_id("-1001234567890"));
        assert!(!is_chat_id("@someone"));
        assert!(!is_chat_id("-"));
    }

    #[test]
    fn task_kinds_map_to_events() {
        assert_eq!(event_for_task("ssl.renew:example.com"), "ssl.fail");
        assert_eq!(event_for_task("ssl.issue:example.com"), "ssl.fail");
        assert_eq!(event_for_task("sites.ssl:example.com"), "ssl.fail");
        assert_eq!(event_for_task("mail.ssl:example.com"), "ssl.fail");
        assert_eq!(event_for_task("backups.run"), "backup.fail");
        assert_eq!(event_for_task("backups.restore:abc"), "backup.fail");
        assert_eq!(event_for_task("apps.update:web"), "deploy.fail");
        assert_eq!(event_for_task("apps.recover:web"), "deploy.fail");
        assert_eq!(event_for_task("quick-install:n8n"), "task.fail");
        assert_eq!(event_for_task("system:self-update"), "task.fail");
        assert_eq!(event_for_task("db:import:shop"), "task.fail");
        for kind in ["ssl.renew:x", "backups.run", "apps.create:x", "system:swap"] {
            assert!(EVENTS.contains(&event_for_task(kind)));
        }
        assert_eq!(describe_task("ssl.renew:example.com"), "Certificate renewal for example.com");
        assert_eq!(describe_task("db:import:shop"), "Database import into shop");
        assert_eq!(describe_task("system:self-update"), "Panel update");
        assert_eq!(describe_task("tailscale:up"), "The task tailscale:up");
    }

    /// The page offers what the agent sends, no more and no less.
    #[test]
    fn the_panel_lists_the_same_events() {
        let ts = include_str!("../../src/lib/notificationEvents.ts");
        let offered: Vec<&str> = ts
            .split("id: \"")
            .skip(1)
            .filter_map(|s| s.split('"').next())
            .collect();
        let mut a = offered.clone();
        a.sort();
        let mut b: Vec<&str> = EVENTS.to_vec();
        b.sort();
        assert_eq!(a, b);
    }

    #[test]
    fn empty_event_list_means_all() {
        assert!(subscribes("", "ssl.fail"));
        assert!(subscribes(" , ", "disk.full"));
        assert!(subscribes("", "memory.oom"));
        assert!(subscribes("backup.fail, ssl.fail", "ssl.fail"));
        assert!(!subscribes("backup.fail", "ssl.fail"));
    }

    #[test]
    fn a_condition_is_reported_once_and_its_end_once() {
        let c = conn();
        let t = Utc::now();
        // First look ever: what is so already is recorded, not reported.
        let ch = reconcile(&c, "disk:", &seen(&[("disk:space:/", true)]), 1, t).unwrap();
        assert_eq!(ch, Changes::default());
        // Still so: nothing. Its end: nothing either, it was never reported.
        assert_eq!(reconcile(&c, "disk:", &seen(&[("disk:space:/", true)]), 1, t).unwrap(), Changes::default());
        assert_eq!(reconcile(&c, "disk:", &[], 1, t).unwrap(), Changes::default());
        // Now new: reported once.
        let ch = reconcile(&c, "disk:", &seen(&[("disk:space:/var", true)]), 1, t).unwrap();
        assert_eq!(keys(&ch.raised), ["disk:space:/var"]);
        let ch = reconcile(&c, "disk:", &seen(&[("disk:space:/var", true)]), 1, t).unwrap();
        assert!(ch.raised.is_empty());
        // Under the raise threshold but over the clear one: still on.
        let ch = reconcile(&c, "disk:", &seen(&[("disk:space:/var", false)]), 1, t).unwrap();
        assert_eq!(ch, Changes::default());
        // Gone: its end, once.
        let ch = reconcile(&c, "disk:", &[], 1, t).unwrap();
        assert_eq!(keys(&ch.cleared), ["disk:space:/var"]);
        assert_eq!(reconcile(&c, "disk:", &[], 1, t).unwrap(), Changes::default());
        // Between the thresholds without a report first: nothing.
        assert_eq!(reconcile(&c, "disk:", &seen(&[("disk:space:/var", false)]), 1, t).unwrap(), Changes::default());
        // And again: a new occurrence, reported again.
        let ch = reconcile(&c, "disk:", &seen(&[("disk:space:/var", true)]), 1, t).unwrap();
        assert_eq!(keys(&ch.raised), ["disk:space:/var"]);
        // Another family is not touched.
        assert_eq!(reconcile(&c, "svc.down:", &[], 2, t).unwrap(), Changes::default());
        let ch = reconcile(&c, "disk:", &seen(&[("disk:space:/var", true)]), 1, t).unwrap();
        assert!(ch.raised.is_empty() && ch.cleared.is_empty());
    }

    #[test]
    fn a_stopped_service_is_reported_on_the_second_look() {
        let c = conn();
        let t0 = Utc::now();
        reconcile(&c, "svc.down:", &[], 2, t0).unwrap();
        let nginx = seen(&[("svc.down:nginx.service", true)]);
        // A restart seen once is not reported, and its end is not either.
        assert_eq!(reconcile(&c, "svc.down:", &nginx, 2, t0).unwrap(), Changes::default());
        assert_eq!(reconcile(&c, "svc.down:", &[], 2, t0).unwrap(), Changes::default());
        // Twice in a row is, with the time it was first seen.
        reconcile(&c, "svc.down:", &nginx, 2, t0).unwrap();
        let t1 = t0 + chrono::Duration::minutes(5);
        let ch = reconcile(&c, "svc.down:", &nginx, 2, t1).unwrap();
        assert_eq!(keys(&ch.raised), ["svc.down:nginx.service"]);
        assert_eq!(ch.raised[0].1.timestamp(), t0.timestamp());
        assert!(reconcile(&c, "svc.down:", &nginx, 2, t1).unwrap().raised.is_empty());
        // Failed now (keep only): still reported, no "runs again".
        let failed = seen(&[("svc.down:nginx.service", false)]);
        assert_eq!(reconcile(&c, "svc.down:", &failed, 2, t1).unwrap(), Changes::default());
        let ch = reconcile(&c, "svc.down:", &[], 2, t1).unwrap();
        assert_eq!(keys(&ch.cleared), ["svc.down:nginx.service"]);
    }

    /// An agent restart (a new connection to the same panel.db) reports
    /// nothing again.
    #[test]
    fn a_restart_does_not_report_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("panel.db");
        let t = Utc::now();
        {
            let db = PanelDb::open(&path).unwrap();
            ensure_schema(&db).unwrap();
            db.with(|c| reconcile(c, "unit.failed:", &[], 1, t)).unwrap();
            let ch = db.with(|c| reconcile(c, "unit.failed:", &seen(&[("unit.failed:a.service", true)]), 1, t)).unwrap();
            assert_eq!(keys(&ch.raised), ["unit.failed:a.service"]);
            assert!(db.with(|c| mark_once(c, "task:ssl.renew:example.com", t)).unwrap());
        }
        let db = PanelDb::open(&path).unwrap();
        ensure_schema(&db).unwrap();
        let ch = db
            .with(|c| reconcile(c, "unit.failed:", &seen(&[("unit.failed:a.service", true), ("unit.failed:b.service", true)]), 1, t))
            .unwrap();
        // b failed while the agent was away: that is new.
        assert_eq!(keys(&ch.raised), ["unit.failed:b.service"]);
        assert!(!db.with(|c| mark_once(c, "task:ssl.renew:example.com", t)).unwrap());
        assert!(db.with(|c| unmark(c, "task:ssl.renew:example.com")).unwrap().is_some());
        assert!(db.with(|c| mark_once(c, "task:ssl.renew:example.com", t)).unwrap());
    }

    #[test]
    fn required_service_states() {
        let st = |load: &str, active: &str, sub: &str, file: &str| UnitState {
            load: load.into(),
            active: active.into(),
            sub: sub.into(),
            file: file.into(),
        };
        let enabled = st("loaded", "active", "exited", "enabled");
        assert_eq!(service_trouble(&st("loaded", "active", "running", "enabled"), &enabled), None);
        assert_eq!(service_trouble(&st("not-found", "inactive", "dead", ""), &enabled), None);
        assert_eq!(
            service_trouble(&st("loaded", "inactive", "dead", "enabled"), &enabled),
            Some(("is stopped".into(), true))
        );
        assert_eq!(
            service_trouble(&st("loaded", "activating", "auto-restart", "enabled"), &enabled),
            Some(("keeps crashing and restarting".into(), true))
        );
        assert_eq!(service_trouble(&st("loaded", "failed", "failed", "enabled"), &enabled).unwrap().1, false);
        assert_eq!(service_trouble(&st("loaded", "activating", "start", "enabled"), &enabled).unwrap().1, false);
        let disabled = st("loaded", "inactive", "dead", "disabled");
        assert_eq!(
            service_trouble(&st("loaded", "inactive", "dead", "disabled"), &disabled),
            Some(("is stopped and disabled".into(), true))
        );
        assert!(service_trouble(&st("loaded", "active", "running", "disabled"), &disabled)
            .unwrap()
            .0
            .starts_with("is disabled"));

        let show = "LoadState=loaded\nActiveState=active\nSubState=running\nUnitFileState=enabled\n\n\
                    LoadState=not-found\nActiveState=inactive\nSubState=dead\nUnitFileState=\n";
        let parsed = parse_show(show);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].active, "active");
        assert_eq!(parsed[1].load, "not-found");
    }

    #[test]
    fn disks_and_inodes() {
        let df = "Filesystem     Type Use% IUse% Mounted on\n\
                  /dev/sda1      ext4  91%    3% /\n\
                  /dev/sdb1      xfs   40%   92% /var/vmail\n\
                  /dev/sda1      ext4  91%    3% /srv/bind of root\n\
                  /dev/sdc1      btrfs 12%     - /data\n";
        let fs = parse_df(df);
        assert_eq!(
            fs,
            vec![
                Fs { mount: "/".into(), used: Some(91), inodes: Some(3) },
                Fs { mount: "/data".into(), used: Some(12), inodes: None },
                Fs { mount: "/var/vmail".into(), used: Some(40), inodes: Some(92) },
            ]
        );
        assert!(parse_df("").is_empty());
    }

    #[test]
    fn the_mail_queue() {
        let now = 1_800_000_000;
        let old = now - 7 * 3600;
        let line = |q: &str, t: i64, why: &str| {
            serde_json::json!({"queue_name": q, "queue_id": "A1", "arrival_time": t,
                "recipients": [{"address": "x@example.net", "delay_reason": why}]})
            .to_string()
        };
        let out = [
            line("deferred", old, "connect to mx.example.net[192.0.2.1]:25: Connection timed out"),
            line("deferred", old - 10, "connect to mx.example.net[192.0.2.1]:25: Connection timed out"),
            line("deferred", now - 60, "host said: 451 try later"),
            line("hold", old, ""),
            line("active", now, ""),
        ]
        .join("\n");
        let q = parse_queue(&out, now);
        assert_eq!((q.waiting, q.old, q.oldest), (4, 2, Some(old - 10)));
        assert_eq!(q.reasons[0].1, 2);
        // Two stuck for hours: not yet. A third: yes. Then it holds at two.
        assert!(queue_seen(&q).is_some_and(|s| !s.raise));
        let q3 = Queue { old: 3, ..Queue::default() };
        assert!(queue_seen(&q3).unwrap().raise);
        assert!(queue_seen(&Queue { waiting: 60, ..Queue::default() }).unwrap().raise);
        assert!(queue_seen(&Queue { waiting: 24, old: 1, ..Queue::default() }).is_none());
    }

    #[test]
    fn oom_kills_from_the_kernel_journal() {
        let e = |ts: i64, m: &str| serde_json::json!({"__REALTIME_TIMESTAMP": ts.to_string(), "MESSAGE": m}).to_string();
        let out = [
            e(100, "Out of memory: Killed process 77 (node) total-vm:1kB"),
            e(200, "Out of memory: Killed process 78 (mysqld) total-vm:1kB, anon-rss:2kB"),
            e(300, "Memory cgroup out of memory: Killed process 79 (php-fpm8.3) total-vm:1kB"),
            e(400, "eth0: link up"),
            "not json".to_string(),
        ]
        .join("\n");
        let (killed, newest) = parse_oom(&out, 100);
        assert_eq!(killed, ["mysqld", "php-fpm8.3"]);
        assert_eq!(newest, 400);
        assert_eq!(parse_oom("", 5), (vec![], 5));
    }

    #[test]
    fn log_lines_only_where_the_channel_takes_them() {
        let m = msg();
        let with = render_text(&m, true);
        let without = render_text(&m, false);
        assert!(with.contains("user@example.com") && with.contains("192.0.2.7"));
        assert!(!without.contains("user@example.com") && !without.contains("192.0.2.7"));
        assert!(without.contains("Log lines are left out"));
        for text in [&with, &without] {
            assert!(text.contains("Server: "));
            assert!(text.contains("When: 2026-11-24 10:03 UTC"));
            assert!(text.contains("In the panel: Tasks, https://"));
        }
        let tg = telegram_text(&m, false);
        assert!(tg.starts_with("[") && tg.contains("] Backup failed\n\nBackup failed on the server."));
        assert!(!tg.contains("192.0.2.7"));
        let hook = webhook_payload(&m, false);
        for field in ["message", "text", "content"] {
            assert!(!hook[field].as_str().unwrap().contains("user@example.com"), "{field}");
        }
        assert!(webhook_payload(&m, true)["message"].as_str().unwrap().contains("user@example.com"));
        assert!(hook["link"].as_str().unwrap().ends_with("/tasks/abc"));

        let email = Channel {
            id: None,
            channel: "email".into(),
            target: "ops@example.com".into(),
            events: String::new(),
            secret: None,
            log_lines: false,
        };
        assert!(email.with_detail());
        assert!(!Channel { channel: "telegram".into(), ..email.clone() }.with_detail());
        assert!(Channel { channel: "webhook".into(), log_lines: true, ..email }.with_detail());
    }

    #[test]
    fn long_messages_keep_their_head_and_the_last_log_lines() {
        let lines: Vec<String> = (0..2000).map(|i| format!("line {i} 😀")).collect();
        let m = msg().detail("Last log lines", lines.join("\n"));
        let tg = telegram_text(&m, true);
        assert!(utf16_len(&tg) <= TELEGRAM_MAX, "{}", utf16_len(&tg));
        assert!(tg.contains("Backup failed on the server."));
        assert!(tg.contains("(the last ones)"));
        assert!(tg.ends_with("line 1999 😀"));
        // A message of emoji only is cut by Telegram's count, not by chars.
        let wide = Message::new("task.fail", "x", "😀".repeat(3000));
        assert!(utf16_len(&telegram_text(&wide, false)) <= TELEGRAM_MAX);
        let text = webhook_payload(&m, true)["text"].as_str().unwrap().to_string();
        assert!(text.len() <= WEBHOOK_TEXT_MAX);
        assert!(text.ends_with("line 1999 😀"));
        // One very long last line is cut, not dropped.
        let one = msg().detail("Last log lines", "y".repeat(10_000));
        let tg = telegram_text(&one, true);
        assert!(utf16_len(&tg) <= TELEGRAM_MAX && tg.ends_with(" [...]"));
    }

    #[test]
    fn email_headers() {
        assert_eq!(encode_header("plain subject"), "plain subject");
        let enc = encode_header("[h] Сервер упал: ошибка при обновлении всех пакетов");
        assert!(enc.starts_with("=?UTF-8?B?") && enc.is_ascii());
        for word in enc.split("\n ") {
            assert!(word.len() <= 75, "{word}");
        }
        let long = msg().detail("Last log lines", "z".repeat(5000));
        let text = email_text(&long, Some("alerts@example.com"), "ops@example.com");
        let (head, body) = text.split_once("\n\n").unwrap();
        assert!(head.contains("From: \""));
        assert!(head.contains("\nTo: ops@example.com\nSubject: ["));
        assert!(head.contains("Auto-Submitted: auto-generated"));
        assert!(body.contains("zzzz [...]"), "email keeps the log lines, each cut short");
        assert!(text.lines().all(|l| l.len() <= 998));
        assert!(email_text(&msg(), None, "ops@example.com").contains("user@example.com"));
        assert!(!email_text(&msg(), None, "ops@example.com").contains("From:"));
    }

    #[test]
    fn delivery_answers() {
        assert_eq!(webhook_answer("ok\n200 "), Ok(None));
        assert_eq!(webhook_answer("\n204 "), Ok(None));
        let moved = webhook_answer("\n301 https://hooks.example.com/new").unwrap_err();
        assert!(moved.retry.is_none() && moved.why.contains("https://hooks.example.com/new"), "{}", moved.why);
        assert!(webhook_answer("no_service\n404 ").unwrap_err().retry.is_none());
        assert!(webhook_answer("busy\n503 ").unwrap_err().retry.is_some());
        assert!(webhook_answer("\n429 ").unwrap_err().retry.is_some());
        assert_eq!(telegram_answer(r#"{"ok":true,"result":{}}"#), Ok(None));
        let slow = telegram_answer(r#"{"ok":false,"error_code":429,"description":"Too Many Requests","parameters":{"retry_after":40}}"#)
            .unwrap_err();
        assert_eq!(slow.retry, Some(Duration::from_secs(40)));
        let gone = telegram_answer(r#"{"ok":false,"error_code":400,"description":"Bad Request: chat not found"}"#).unwrap_err();
        assert_eq!(gone, Fail::permanent("Telegram: Bad Request: chat not found"));
        // Waits: ours, or the channel's own when longer (capped).
        let d = [Duration::from_secs(30), Duration::from_secs(300)];
        assert_eq!(next_wait(1, &Fail::transient("x"), &d), Some(Duration::from_secs(30)));
        assert_eq!(next_wait(2, &slow, &d), Some(Duration::from_secs(300)));
        let long = Fail { why: "x".into(), retry: Some(Duration::from_secs(9999)) };
        assert_eq!(next_wait(1, &long, &d), Some(RETRY_MAX));
        assert_eq!(next_wait(3, &Fail::transient("x"), &d), None);
        assert_eq!(next_wait(1, &Fail::permanent("x"), &d), None);
    }

    #[test]
    fn last_lines_of_a_log() {
        assert_eq!(last_lines("a\nb\nc\n", 2), "b\nc");
        assert_eq!(last_lines("a\nb", 5), "a\nb");
        assert_eq!(last_lines("a\nb", 0), "");
    }

    #[test]
    fn interrupted_tasks_are_one_message_per_event() {
        let rows = vec![
            ("1".to_string(), "backups.run".to_string()),
            ("2".to_string(), "apps.update:web".to_string()),
            ("3".to_string(), "apps.create:shop".to_string()),
        ];
        let msgs = interrupted_messages(&rows);
        assert_eq!(msgs.len(), 2);
        let deploy = msgs.iter().find(|m| m.event == "deploy.fail").unwrap();
        assert_eq!(deploy.subject, "2 tasks were cut short by an agent restart");
        assert!(deploy.body.contains("- Update of the app web\n- Deploy of the app shop"));
        let backup = msgs.iter().find(|m| m.event == "backup.fail").unwrap();
        assert_eq!(backup.subject, "Backup was cut short by an agent restart");
        assert_eq!(backup.look.as_ref().unwrap().1, "/tasks/1");
    }

    #[test]
    fn channels_read_before_and_after_the_panel_migration() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE notifications (id INTEGER PRIMARY KEY AUTOINCREMENT, channel TEXT NOT NULL,
             target TEXT NOT NULL, events TEXT NOT NULL, secret TEXT);
             INSERT INTO notifications (channel, target, events) VALUES ('webhook', 'https://h.example/x', '');",
        )
        .unwrap();
        let before = read_channels(&c, None).unwrap();
        assert!(before[0].log_lines, "before the migration every channel got log lines");
        // Recording a result is a no-op without the columns, not an error.
        record(&c, 1, "test", &Ok(None));
        c.execute_batch(
            "ALTER TABLE notifications ADD COLUMN log_lines INTEGER NOT NULL DEFAULT 1;
             ALTER TABLE notifications ADD COLUMN last_at TEXT;
             ALTER TABLE notifications ADD COLUMN last_event TEXT;
             ALTER TABLE notifications ADD COLUMN last_ok INTEGER;
             ALTER TABLE notifications ADD COLUMN last_detail TEXT;
             INSERT INTO notifications (channel, target, events, log_lines) VALUES ('webhook', 'https://h.example/y', 'ssl.fail', 0);",
        )
        .unwrap();
        let after = read_channels(&c, None).unwrap();
        assert!(after[0].log_lines && !after[1].log_lines);
        assert_eq!(read_channels(&c, Some(2)).unwrap()[0].target, "https://h.example/y");
        record(&c, 2, "ssl.fail", &Err("the webhook answered 500".into()));
        let (ok, detail): (i64, String) = c
            .query_row("SELECT last_ok, last_detail FROM notifications WHERE id = 2", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!((ok, detail.as_str()), (0, "the webhook answered 500"));
    }

    #[test]
    fn webhook_url_goes_into_a_private_curl_config() {
        let f = curl_url_file(r#"https://hooks.example.com/services/T0/B0/se"cr\et"#).unwrap();
        assert_eq!(
            std::fs::read_to_string(f.path()).unwrap(),
            "url = \"https://hooks.example.com/services/T0/B0/se\\\"cr\\\\et\"\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(f.path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "only its owner reads it");
        }
        let path = f.path().to_path_buf();
        drop(f);
        assert!(!path.exists());
    }

    fn hook(url: &str) -> Channel {
        Channel {
            id: None,
            channel: "webhook".into(),
            target: url.into(),
            events: String::new(),
            secret: None,
            log_lines: false,
        }
    }

    #[tokio::test]
    async fn a_webhook_url_with_a_line_break_is_refused() {
        let url = "https://hooks.example.com/x\noutput = \"/tmp/y\"";
        assert!(deliver(&hook(url), None, &msg(), &[]).await.is_err());
    }

    /// A local listener that answers each request with the next of
    /// `answers`, and hands back the requests it got.
    fn listen(answers: Vec<&'static str>) -> (u16, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let mut got = Vec::new();
            for answer in answers {
                let (mut s, _) = listener.accept().unwrap();
                let mut req = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let n = s.read(&mut chunk).unwrap();
                    req.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&req).into_owned();
                    if let Some((head, body)) = text.split_once("\r\n\r\n") {
                        let len: usize = head
                            .lines()
                            .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap()))
                            .unwrap_or(0);
                        if body.len() >= len {
                            break;
                        }
                    }
                    if n == 0 {
                        break;
                    }
                }
                s.write_all(answer.as_bytes()).unwrap();
                got.push(String::from_utf8_lossy(&req).into_owned());
            }
            got
        });
        (port, server)
    }

    fn have_curl() -> bool {
        std::process::Command::new("curl").arg("--version").output().is_ok()
    }

    /// The whole webhook path through the real curl, against a local listener.
    #[tokio::test]
    async fn webhook_posts_the_payload_to_the_url_as_typed() {
        if !have_curl() {
            return;
        }
        let (port, server) = listen(vec!["HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"]);
        // Braces and brackets are what curl would expand without -g.
        let url = format!("http://127.0.0.1:{port}/hook/{{a,b}}?ids[]=1");
        let m = Message::new("test", "Subject", "Body \"quoted\"\nline 2").detail("Log", "secret@example.com");
        deliver(&hook(&url), None, &m, &[]).await.unwrap();
        let req = server.join().unwrap().remove(0);
        assert!(req.starts_with("POST /hook/{a,b}?ids[]=1 HTTP/1.1\r\n"), "{req}");
        assert!(req.to_ascii_lowercase().contains("content-type: application/json"), "{req}");
        let body = req.split_once("\r\n\r\n").unwrap().1;
        let v: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(v["event"], "test");
        assert!(v["message"].as_str().unwrap().starts_with("Body \"quoted\"\nline 2\n\nServer: "));
        assert!(v["text"].as_str().unwrap().contains("] Subject\nBody \"quoted\"\nline 2"));
        // Log lines stay off a channel that did not opt in.
        assert!(!body.contains("secret@example.com"));
    }

    /// A 5xx is tried again; a redirect is not followed and fails.
    #[tokio::test]
    async fn webhook_retries_a_server_error_and_refuses_a_redirect() {
        if !have_curl() {
            return;
        }
        let (port, server) = listen(vec![
            "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 4\r\nConnection: close\r\n\r\nbusy",
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ]);
        let url = format!("http://127.0.0.1:{port}/hook");
        let zero = [Duration::ZERO, Duration::ZERO];
        assert_eq!(deliver(&hook(&url), None, &msg(), &zero).await, Ok(None));
        assert_eq!(server.join().unwrap().len(), 2);

        let (port, server) = listen(vec![
            "HTTP/1.1 301 Moved Permanently\r\nLocation: https://example.com/new\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ]);
        let url = format!("http://127.0.0.1:{port}/hook");
        let err = deliver(&hook(&url), None, &msg(), &zero).await.unwrap_err();
        assert!(err.contains("301") && err.contains("https://example.com/new"), "{err}");
        assert_eq!(server.join().unwrap().len(), 1);

        // Nobody listening: tried as often as allowed, then given up.
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let err = deliver(&hook(&format!("http://127.0.0.1:{port}/")), None, &msg(), &zero).await.unwrap_err();
        assert!(err.ends_with("(after 3 tries)"), "{err}");
    }
}
