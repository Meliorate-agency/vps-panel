//! Operator notifications configured on /notifications (panel.db table
//! `notifications`): email through the local Postfix, JSON webhooks (with
//! `text` / `content` fields, so Slack and Discord webhooks work as is) and
//! Telegram messages from the operator's own bot. Failed tasks, systemd
//! units that enter the failed state and a nearly full root disk are
//! reported. Before this nothing read that table: the page promised alerts
//! that were never sent.

use crate::db::PanelDb;
use crate::error::{AgentError, AgentResult};
use crate::exec;
use crate::ipc::OkResponse;
use crate::state::AppState;
use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use std::collections::HashSet;
use std::sync::OnceLock;
use std::time::Duration;

static HOST: OnceLock<String> = OnceLock::new();

pub fn init(panel_host: &str) {
    let _ = HOST.set(panel_host.to_string());
}

fn host() -> &'static str {
    HOST.get().map(String::as_str).unwrap_or("vps-panel")
}

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

/// An empty event list subscribes to everything.
fn subscribes(events: &str, event: &str) -> bool {
    let mut list = events.split(',').map(str::trim).filter(|e| !e.is_empty()).peekable();
    list.peek().is_none() || list.any(|e| e == event)
}

/// (channel, target, secret) of every channel subscribed to `event`. The
/// secret is a Telegram channel's bot token, None for the others.
fn channels_for(db: &PanelDb, event: &str) -> Vec<(String, String, Option<String>)> {
    db.with(|conn| {
        let mut stmt = conn.prepare("SELECT channel, target, events, secret FROM notifications")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>()
    })
    .unwrap_or_default()
    .into_iter()
    .filter(|(_, _, events, _)| subscribes(events, event))
    .map(|(channel, target, _, secret)| (channel, target, secret))
    .collect()
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

/// Deliver one message to one channel. Email success means Postfix accepted
/// it for delivery.
async fn send(
    channel: &str,
    target: &str,
    secret: Option<&str>,
    from: Option<&str>,
    event: &str,
    subject: &str,
    body: &str,
) -> AgentResult<()> {
    let target = target.trim();
    if target.is_empty() || target.contains(['\r', '\n']) {
        return Err(AgentError::Invalid("invalid notification target".into()));
    }
    let subject = format!("[{}] {}", host(), subject.replace(['\r', '\n'], " "));
    match channel {
        "email" => {
            if !is_plain_address(target) {
                return Err(AgentError::Invalid(format!("not an email address: {target}")));
            }
            let from_line = from.map(|f| format!("From: \"{}\" <{f}>\n", host())).unwrap_or_default();
            let msg = format!(
                "{from_line}To: {target}\nSubject: {subject}\nMIME-Version: 1.0\nContent-Type: text/plain; charset=utf-8\n\n{body}\n"
            );
            let mut args = vec!["-t", "-oi"];
            if let Some(f) = from {
                args.extend(["-f", f]);
            }
            exec::run_with_input("sendmail", args, &msg).await?;
        }
        "telegram" => {
            let token = secret.unwrap_or("").trim();
            if !is_bot_token(token) {
                return Err(AgentError::Invalid("missing or malformed Telegram bot token".into()));
            }
            if !is_chat_id(target) {
                return Err(AgentError::Invalid(format!("not a Telegram user ID: {target}")));
            }
            // Telegram's limit is 4096 characters.
            let text: String = format!("{subject}\n\n{body}").chars().take(4000).collect();
            let payload = serde_json::json!({
                "chat_id": target,
                "text": text,
                "disable_web_page_preview": true,
            });
            // The token is part of the URL, so the whole request reaches curl
            // as a config on stdin: argv shows in every user's process list.
            let config = format!(
                "url = \"https://api.telegram.org/bot{token}/sendMessage\"\n\
                 header = \"Content-Type: application/json\"\n\
                 data-binary = \"{}\"\n",
                payload.to_string().replace('\\', "\\\\").replace('"', "\\\"")
            );
            let out = exec::run_with_input("curl", ["-sS", "-m", "10", "-K", "-"], &config).await?;
            // No -f: a refusal comes back as {"ok":false,"description":...},
            // and "chat not found" (the user never pressed Start in the bot)
            // is worth showing as is.
            let resp: serde_json::Value = serde_json::from_str(&out.stdout).unwrap_or_default();
            if resp["ok"] != true {
                let why = resp["description"].as_str().unwrap_or("no answer from api.telegram.org");
                return Err(AgentError::Other(anyhow::anyhow!("Telegram: {why}")));
            }
        }
        "webhook" => {
            if !(target.starts_with("https://") || target.starts_with("http://")) {
                return Err(AgentError::Invalid(format!("not an http(s) URL: {target}")));
            }
            // Discord rejects content over 2000 characters.
            let mut text = format!("{subject}\n{body}");
            if text.len() > 1900 {
                let mut cut = 1900;
                while !text.is_char_boundary(cut) {
                    cut -= 1;
                }
                text.truncate(cut);
                text.push_str("\n...");
            }
            let payload = serde_json::json!({
                "event": event,
                "host": host(),
                "subject": subject,
                "message": body,
                "text": text,
                "content": text,
            });
            exec::run_with_input(
                "curl",
                [
                    "-fsS", "-m", "10", "-X", "POST",
                    "-H", "Content-Type: application/json",
                    "--data-binary", "@-", target,
                ],
                &payload.to_string(),
            )
            .await?;
        }
        other => return Err(AgentError::Invalid(format!("unknown channel {other}"))),
    }
    Ok(())
}

/// Send `event` to every channel subscribed to it, in the background.
pub fn notify(db: &PanelDb, event: &'static str, subject: String, body: String) {
    let channels = channels_for(db, event);
    if channels.is_empty() {
        return;
    }
    let from = email_from(db);
    let Ok(rt) = tokio::runtime::Handle::try_current() else {
        return;
    };
    rt.spawn(async move {
        for (channel, target, secret) in channels {
            let sent = send(&channel, &target, secret.as_deref(), from.as_deref(), event, &subject, &body).await;
            if let Err(e) = sent {
                tracing::warn!(%channel, %target, error = %e, "notification not delivered");
            }
        }
    });
}

pub fn task_failed(db: &PanelDb, kind: &str, id: &str, log_tail: &str) {
    notify(
        db,
        event_for_task(kind),
        format!("Task failed: {kind}"),
        format!(
            "The task {kind} failed on {}.\n\nLast log lines:\n{log_tail}\n\nFull log: Tasks > {id} in the panel.",
            host()
        ),
    );
}

/// Every 5 minutes: systemd units that newly entered the failed state (a
/// crash-looping app lands there once it hits its start limit) and the root
/// filesystem crossing 90% used.
pub async fn monitor_loop(db: PanelDb) {
    // None until the first scan: units already failed when the agent starts
    // (every self-update restarts it) are not reported again.
    let mut known_failed: Option<HashSet<String>> = None;
    let mut disk_alerted = false;
    loop {
        if let Ok(out) = exec::run(
            "systemctl",
            ["list-units", "--state=failed", "--plain", "--no-legend", "--no-pager"],
        )
        .await
        {
            let failed: HashSet<String> = out
                .stdout
                .lines()
                .filter_map(|l| l.split_whitespace().next())
                .map(str::to_string)
                .collect();
            if let Some(prev) = &known_failed {
                for unit in failed.difference(prev) {
                    let journal = exec::run(
                        "journalctl",
                        ["-u", unit, "-n", "20", "--no-pager", "-o", "short-iso"],
                    )
                    .await
                    .map(|o| o.stdout)
                    .unwrap_or_default();
                    notify(
                        &db,
                        "service.fail",
                        format!("Service failed: {unit}"),
                        format!(
                            "{unit} entered the failed state on {}.\n\nLast journal lines:\n{journal}",
                            host()
                        ),
                    );
                }
            }
            known_failed = Some(failed);
        }
        if let Some(pct) = root_disk_used_pct().await {
            if pct >= 90 && !disk_alerted {
                disk_alerted = true;
                notify(
                    &db,
                    "disk.full",
                    format!("Disk {pct}% full"),
                    format!(
                        "The root filesystem on {} is {pct}% full. Databases, mail and deploys \
                         start failing when it fills up.",
                        host()
                    ),
                );
            } else if pct < 85 {
                disk_alerted = false;
            }
        }
        tokio::time::sleep(Duration::from_secs(300)).await;
    }
}

async fn root_disk_used_pct() -> Option<u32> {
    let out = exec::run("df", ["-P", "/"]).await.ok()?;
    parse_df_pct(&out.stdout)
}

fn parse_df_pct(df: &str) -> Option<u32> {
    df.lines()
        .nth(1)?
        .split_whitespace()
        .nth(4)?
        .trim_end_matches('%')
        .parse()
        .ok()
}

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
    notify(&state.panel_db, event, subject, body);
    Ok(Json(OkResponse::default()))
}

#[derive(Deserialize)]
pub struct TestReq {
    pub channel: String,
    pub target: String,
    /// A Telegram channel's bot token.
    #[serde(default)]
    pub secret: Option<String>,
}

/// POST /notifications/test - send a test message right away and report
/// whether the channel accepted it.
pub async fn test(
    State(state): State<AppState>,
    Json(req): Json<TestReq>,
) -> AgentResult<Json<OkResponse>> {
    send(
        &req.channel,
        &req.target,
        req.secret.as_deref(),
        email_from(&state.panel_db).as_deref(),
        "test",
        "Test notification",
        &format!(
            "This is a test from the vps-panel on {}. If you can read it, this channel works.",
            host()
        ),
    )
    .await?;
    Ok(Json(OkResponse::default()))
}

#[cfg(test)]
mod tests {
    use super::{event_for_task, is_bot_token, is_chat_id, parse_df_pct, subscribes};

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
        assert_eq!(event_for_task("sites.ssl:example.com"), "ssl.fail");
        assert_eq!(event_for_task("mail.ssl:example.com"), "ssl.fail");
        assert_eq!(event_for_task("backups.run"), "backup.fail");
        assert_eq!(event_for_task("apps.update:web"), "deploy.fail");
        assert_eq!(event_for_task("quick-install:n8n"), "task.fail");
    }

    #[test]
    fn empty_event_list_means_all() {
        assert!(subscribes("", "ssl.fail"));
        assert!(subscribes(" , ", "disk.full"));
        assert!(subscribes("backup.fail, ssl.fail", "ssl.fail"));
        assert!(!subscribes("backup.fail", "ssl.fail"));
    }

    #[test]
    fn df_percentage() {
        let df = "Filesystem     1024-blocks     Used Available Capacity Mounted on\n\
                  /dev/sda1         38905184 35014665   3874135      91% /\n";
        assert_eq!(parse_df_pct(df), Some(91));
        assert_eq!(parse_df_pct(""), None);
    }
}
