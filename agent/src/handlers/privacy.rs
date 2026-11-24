//! Settings > Privacy, the system side: what nginx writes into its access
//! log (masked visitor addresses, no query strings) and for how long logs
//! with personal data are kept (the journal, nginx's rotated files, and the
//! text files rsyslog, ufw and fail2ban keep next to the journal).
//!
//! The panel stores the settings in panel.db and calls POST /privacy/apply;
//! the agent applies them again when it starts and once a day. With every
//! setting at its default nothing here writes anything: nginx, journald and
//! logrotate are exactly as the system set them up, and a setting that is
//! switched off again takes out what it put in.
//!
//! GET /privacy/status reads the system (nginx's own dump of its
//! configuration, journald's merged configuration, the files on disk); it
//! does not repeat the settings.

use crate::db::PanelDb;
use crate::error::{AgentError, AgentResult};
use crate::exec;
use crate::fs_atomic;
use crate::handlers::nginx;
use crate::state::AppState;
use axum::extract::State;
use axum::Json;
use rusqlite::OptionalExtension;
use serde::Serialize;
use std::net::{IpAddr, Ipv6Addr};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DAY: u64 = 86_400;

/// One apply at a time: the start-up run and the panel's can meet.
static APPLY_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// The three settings the agent acts on. The default is today's behaviour.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Settings {
    /// `privacy.log_ip = masked`
    pub mask_ip: bool,
    /// `privacy.log_query = strip`
    pub strip_query: bool,
    /// `privacy.retention_days`, 7..=3650
    pub retention_days: Option<u32>,
}

/// A value that is not one of the documented ones counts as the default:
/// the panel checks what it saves, but not a row something else wrote.
fn parse_settings(log_ip: Option<&str>, log_query: Option<&str>, retention: Option<&str>) -> Settings {
    Settings {
        mask_ip: log_ip.map(str::trim) == Some("masked"),
        strip_query: log_query.map(str::trim) == Some("strip"),
        retention_days: retention
            .and_then(|r| r.trim().parse::<u32>().ok())
            .filter(|d| (7..=3650).contains(d)),
    }
}

fn read_settings(db: &PanelDb) -> AgentResult<Settings> {
    db.with(|conn| {
        // A database the panel has not set up yet holds no settings.
        let has_table: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'settings')",
            [],
            |row| row.get(0),
        )?;
        if !has_table {
            return Ok(Settings::default());
        }
        let get = |key: &str| -> rusqlite::Result<Option<String>> {
            conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| row.get(0))
                .optional()
        };
        Ok(parse_settings(
            get("privacy.log_ip")?.as_deref(),
            get("privacy.log_query")?.as_deref(),
            get("privacy.retention_days")?.as_deref(),
        ))
    })
}

/// The masked form of an address, the same everywhere (nginx's access log
/// through nginx::ADDR_MAP, the panel, the personal-data tools): IPv4 with
/// the last octet zeroed; IPv6 with its last five groups zeroed, written the
/// shortest way (`2001:db8:85a3:8d3::7` is `2001:db8:85a3::`, `2001:db8::1`
/// is `2001:db8::`, `::ffff:203.0.113.87` is `::`). None for anything that
/// is not one address.
#[cfg_attr(not(test), allow(dead_code))] // the definition nginx::ADDR_MAP is tested against
pub fn mask_ip(raw: &str) -> Option<String> {
    match raw.trim().parse::<IpAddr>().ok()? {
        IpAddr::V4(a) => {
            let o = a.octets();
            Some(format!("{}.{}.{}.0", o[0], o[1], o[2]))
        }
        IpAddr::V6(a) => {
            let s = a.segments();
            Some(Ipv6Addr::new(s[0], s[1], s[2], 0, 0, 0, 0, 0).to_string())
        }
    }
}

// ---------------------------------------------------------------------------
// What the endpoints answer
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, PartialEq)]
pub struct PrivacyPart {
    pub id: &'static str,
    pub label: &'static str,
    /// What the settings ask for.
    pub wanted: String,
    /// What the system does now.
    pub actual: String,
    pub ok: bool,
    /// When not ok: why, and what to do.
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct PrivacyStatus {
    pub log_ip: &'static str,
    pub log_query: &'static str,
    pub retention_days: Option<u32>,
    pub parts: Vec<PrivacyPart>,
    pub notes: Vec<String>,
}

/// Why a part could not be applied; None for a part that went through.
#[derive(Debug, Default)]
struct Failures {
    nginx: Option<String>,
    journal: Option<String>,
    system_files: Vec<String>,
    nginx_files: Option<String>,
}

/// GET /privacy/status
pub async fn status(State(state): State<AppState>) -> AgentResult<Json<PrivacyStatus>> {
    let settings = read_settings(&state.panel_db)?;
    Ok(Json(read_status(settings, Failures::default()).await))
}

/// POST /privacy/apply: bring the system to the stored settings, then answer
/// what holds. A part that could not be applied comes back `ok: false` with
/// the reason; only unreadable settings fail the call, and then nothing is
/// touched (applying defaults instead would switch the protection off).
pub async fn apply(State(state): State<AppState>) -> AgentResult<Json<PrivacyStatus>> {
    let settings = read_settings(&state.panel_db)?;
    let _guard = APPLY_LOCK.lock().await;
    let failures = apply_settings(settings).await;
    Ok(Json(read_status(settings, failures).await))
}

/// Spawned when the agent starts: a fresh install, an update and a restored
/// panel.db all reach the stored settings without anyone pressing a button.
/// Once a day after that, which also deletes rotated files that have passed
/// the retention on a server too quiet for logrotate to turn them over, and
/// puts the log format back after a package upgrade replaced nginx.conf.
pub async fn ensure_at_startup(state: AppState) {
    loop {
        match read_settings(&state.panel_db) {
            Ok(settings) => {
                let _guard = APPLY_LOCK.lock().await;
                let failures = apply_settings(settings).await;
                for why in [&failures.nginx, &failures.journal, &failures.nginx_files]
                    .into_iter()
                    .flatten()
                    .chain(failures.system_files.iter())
                {
                    tracing::warn!("privacy settings not fully applied: {why}");
                }
            }
            Err(e) => tracing::warn!(error = %e, "privacy settings not readable; logs left as they are"),
        }
        tokio::time::sleep(Duration::from_secs(DAY)).await;
    }
}

async fn apply_settings(s: Settings) -> Failures {
    let nginx_result = nginx::apply_log_privacy(s.mask_ip, s.strip_query).await;
    if nginx_result == Ok(true) {
        wait_for_nginx_reload().await;
    }
    let mut failures = Failures {
        nginx: nginx_result.err(),
        journal: apply_journal(s.retention_days).await.err(),
        nginx_files: apply_rotate(&NGINX_FILES, s.retention_days).await.err(),
        system_files: Vec::new(),
    };
    for target in &SYSTEM_FILES {
        if let Err(e) = apply_rotate(target, s.retention_days).await {
            failures.system_files.push(e);
        }
    }
    failures
}

async fn read_status(s: Settings, failures: Failures) -> PrivacyStatus {
    let nginx_reading = read_nginx(s).await;
    let journal_conf = match exec::run("systemd-analyze", ["cat-config", "systemd/journald.conf"]).await {
        Ok(out) => Ok(journald_conf(&out.stdout)),
        Err(e) => Err(why(&e)),
    };
    let days = s.retention_days;
    let (journal, files) = tokio::task::spawn_blocking(move || {
        let journal = JournalReading {
            conf: journal_conf,
            read_by_journald: journald_has_read(Path::new(JOURNALD_DROPIN)),
            our_file: Path::new(JOURNALD_DROPIN).exists(),
            oldest_file_days: oldest_journal_file(Path::new(JOURNAL_DIR)).map(|t| age_secs(t) / DAY),
        };
        let system: Vec<RotateReading> = SYSTEM_FILES.iter().filter_map(|t| read_rotate(t, days)).collect();
        (journal_part(days, &journal, &system), read_rotate(&NGINX_FILES, days))
    })
    .await
    .unwrap_or_else(|e| {
        let unread = PrivacyPart {
            id: "journal",
            label: JOURNAL_LABEL,
            wanted: String::new(),
            actual: "Could not be read.".into(),
            ok: false,
            detail: e.to_string(),
        };
        (unread, None)
    });

    let mut parts = vec![nginx_part(s, &nginx_reading), journal, nginx_files_part(days, files.as_ref())];
    let mut failed = vec![failures.nginx, None, failures.nginx_files];
    failed[1] = failures.journal.into_iter().chain(failures.system_files).reduce(|a, b| format!("{a} {b}"));
    for (part, failure) in parts.iter_mut().zip(failed) {
        if let Some(text) = failure {
            part.ok = false;
            part.detail = if part.detail.is_empty() { text } else { format!("{text} {}", part.detail) };
        }
    }
    PrivacyStatus {
        log_ip: if s.mask_ip { "masked" } else { "full" },
        log_query: if s.strip_query { "strip" } else { "keep" },
        retention_days: s.retention_days,
        parts,
        notes: notes(),
    }
}

fn notes() -> Vec<String> {
    vec![
        "nginx's error log always holds full visitor addresses: nginx cannot change its format. \
         It is rotated and deleted together with the access log."
            .into(),
        "Masking and leaving out query strings apply to lines written from now on. Lines already \
         in the logs stay as they are until the log retention deletes them."
            .into(),
        "Mail and SSH log full addresses to the system journal (and to mail.log and auth.log \
         where rsyslog is installed), fail2ban logs the addresses it bans. These are not \
         masked; the log retention deletes them."
            .into(),
        "The journal is deleted in whole files. When a limit is first set or lowered, a journal \
         file that holds entries older than the limit goes with the newer entries in it, which \
         on a quiet server can be most of the journal."
            .into(),
        "Sites and apps behind nginx still receive the visitor's full address and may log it \
         themselves."
            .into(),
    ]
}

/// An error as a sentence fragment for `detail`: the command's own words.
fn why(e: &AgentError) -> String {
    match e {
        AgentError::Subprocess { cmd, stderr, .. } => format!("{cmd}: {}", stderr.trim()),
        AgentError::Io(e) if e.kind() == std::io::ErrorKind::NotFound => "command not found".into(),
        other => other.to_string(),
    }
}

fn age_secs(t: SystemTime) -> u64 {
    SystemTime::now().duration_since(t).map(|d| d.as_secs()).unwrap_or(0)
}

fn epoch_secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

// ---------------------------------------------------------------------------
// Running processes: has a daemon started since its configuration changed?
// ---------------------------------------------------------------------------

/// `comm` and start time (clock ticks since boot) from /proc/<pid>/stat.
/// The name sits in parentheses and may itself hold spaces or parentheses.
fn parse_proc_stat(stat: &str) -> Option<(&str, u64)> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let comm = stat.get(open + 1..close)?;
    // After the name: state, ppid, ...; starttime is field 22 of the line.
    let start = stat.get(close + 1..)?.split_whitespace().nth(19)?.parse().ok()?;
    Some((comm, start))
}

/// /proc counts a process's start in USER_HZ ticks, 100 a second on every
/// architecture Ubuntu runs on.
const PROC_TICKS_PER_SEC: u64 = 100;

/// When the newest process called `comm` started, in seconds since 1970.
/// nginx starts new workers on every reload, so for nginx this is the time
/// of its last start or reload. (In a container whose /proc/stat shows its
/// own boot time the result lies in the future, and the checks built on it
/// find nothing to complain about.)
fn newest_process_start(comm: &str) -> Option<u64> {
    let boot: u64 = std::fs::read_to_string("/proc/stat")
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("btime "))?
        .trim()
        .parse()
        .ok()?;
    let mut newest: Option<u64> = None;
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        if let Some((name, start)) = parse_proc_stat(&stat) {
            if name == comm {
                newest = newest.max(Some(boot + start / PROC_TICKS_PER_SEC));
            }
        }
    }
    newest
}

/// The start time is rounded down to the second twice over (boot time and
/// ticks), so a daemon restarted right after a write can look a moment older
/// than the file.
const START_SLACK: u64 = 2;

// ---------------------------------------------------------------------------
// nginx's access log
// ---------------------------------------------------------------------------

enum NginxReading {
    NotInstalled,
    /// nginx's configuration does not pass its own test.
    Broken(String),
    State {
        state: nginx::LogState,
        running: bool,
        /// nginx has started or reloaded since its configuration changed.
        reloaded: bool,
        /// An apply would still change nginx's files.
        pending: bool,
    },
}

/// (nginx is running, it has started or reloaded since its configuration
/// last changed).
fn nginx_processes() -> (bool, bool) {
    let started = newest_process_start("nginx");
    let changed = [nginx::NGINX_CONF, nginx::LOG_PRIVACY_CONF]
        .iter()
        .filter_map(|p| mtime(Path::new(p)))
        .map(epoch_secs)
        .max()
        .unwrap_or(0);
    (started.is_some(), started.is_some_and(|s| s + START_SLACK >= changed))
}

/// A reload returns at once and nginx starts its new workers a moment
/// later. Until then requests are logged the old way, and the status read
/// right after an apply would say so.
async fn wait_for_nginx_reload() {
    for _ in 0..30 {
        let (running, reloaded) = tokio::task::spawn_blocking(nginx_processes).await.unwrap_or((false, false));
        if !running || reloaded {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn read_nginx(s: Settings) -> NginxReading {
    let dump = match nginx::dump_config().await {
        Ok(d) => d,
        Err(AgentError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => return NginxReading::NotInstalled,
        Err(e) => return NginxReading::Broken(why(&e)),
    };
    let state = nginx::log_state(&dump);
    let (running, reloaded) = tokio::task::spawn_blocking(nginx_processes).await.unwrap_or((false, false));
    let pending = nginx::log_privacy_pending(s.mask_ip, s.strip_query).await;
    NginxReading::State { state, running, reloaded, pending }
}

fn log_words(mask: bool, strip: bool) -> &'static str {
    match (mask, strip) {
        (false, false) => "Full visitor addresses and full URLs (nginx's standard format).",
        (true, false) => "Visitor addresses masked; URLs in full.",
        (false, true) => "Full visitor addresses; query strings left out of URLs and referrers.",
        (true, true) => "Visitor addresses masked; query strings left out of URLs and referrers.",
    }
}

fn places(list: &[nginx::AccessLogAt]) -> String {
    let mut shown: Vec<String> = list.iter().take(5).map(|a| format!("{} line {}", a.file, a.line)).collect();
    if list.len() > 5 {
        shown.push(format!("{} more", list.len() - 5));
    }
    shown.join(", ")
}

fn nginx_part(s: Settings, reading: &NginxReading) -> PrivacyPart {
    let wanted_on = s.mask_ip || s.strip_query;
    let (actual, ok, detail) = match reading {
        NginxReading::NotInstalled => (
            "nginx is not installed.".to_string(),
            !wanted_on,
            String::new(),
        ),
        NginxReading::Broken(e) => (
            "nginx's configuration does not pass its own test, so what it logs could not be read.".to_string(),
            false,
            format!("Fix the configuration, then apply again. {e}"),
        ),
        NginxReading::State { state, running, reloaded, pending } => {
            let mut detail: Vec<String> = Vec::new();
            let format_name = nginx::LOG_FORMAT_NAME;
            let (mut actual, mut ok) = if state.http_off {
                (format!("nginx writes no access log (access_log off in {}).", nginx::NGINX_CONF), true)
            } else if state.http_ours > 0 && state.http_full.is_empty() {
                (
                    log_words(state.masks, state.strips).to_string(),
                    state.masks == s.mask_ip && state.strips == s.strip_query,
                )
            } else if state.http_ours > 0 {
                detail.push(format!(
                    "Remove or change the access_log at {}, then apply again.",
                    places(&state.http_full)
                ));
                ("Every request is logged twice, one of the two in full.".to_string(), false)
            } else {
                if wanted_on && !pending && !state.http_full.is_empty() {
                    detail.push(format!(
                        "The access_log at {} is written in a way the panel does not rewrite (its own \
                         format, or not on a line of its own). Change it to `access_log <path>;` and \
                         apply again, or give it the format name {format_name} yourself.",
                        places(&state.http_full)
                    ));
                }
                // In the operator's own format the panel cannot tell what is logged.
                match state.http_full.iter().find_map(|a| a.format.as_deref()) {
                    Some(name) => (
                        format!("{} writes the access log in a format of its own (`{name}`).", nginx::NGINX_CONF),
                        !wanted_on,
                    ),
                    None => (log_words(false, false).to_string(), !wanted_on),
                }
            };
            if wanted_on && !state.elsewhere.is_empty() {
                ok = false;
                detail.push(format!(
                    "These set an access_log of their own, which the panel does not rewrite, so their \
                     requests are logged in full: {}. Add the format name to each \
                     (`access_log <path> {format_name};`) or remove the line, then reload nginx.",
                    places(&state.elsewhere)
                ));
            }
            if !running {
                actual.push_str(" nginx is not running.");
            } else if wanted_on && !pending && !reloaded {
                ok = false;
                actual = "nginx has not reloaded since its configuration last changed; it still logs the old way."
                    .to_string();
                detail.push("Reload nginx.".to_string());
            }
            if !ok && (*pending || detail.is_empty()) {
                detail.insert(0, "The settings are saved but not applied to nginx; apply them again.".to_string());
            }
            (actual, ok, detail.join(" "))
        }
    };
    PrivacyPart {
        id: "nginx_logs",
        label: "Web server access log",
        wanted: log_words(s.mask_ip, s.strip_query).to_string(),
        actual,
        ok,
        detail,
    }
}

// ---------------------------------------------------------------------------
// The systemd journal
// ---------------------------------------------------------------------------

const JOURNAL_LABEL: &str = "System logs (mail, SSH, services)";
const JOURNALD_DROPIN_DIR: &str = "/etc/systemd/journald.conf.d";
/// Sorted late, so it wins over the distribution's own drop-ins. The size
/// limit the installer puts in journald.conf is another key and stays.
const JOURNALD_DROPIN: &str = "/etc/systemd/journald.conf.d/90-vps-panel-retention.conf";
const JOURNAL_DIR: &str = "/var/log/journal";
/// journald's MaxFileSec when nothing sets it.
const DEFAULT_FILE_SECS: u64 = 2_629_800;

/// (MaxRetentionSec, MaxFileSec) in days for "keep N days". journald deletes
/// by time only whole archived files, and goes by a file's FIRST entry
/// (journal-vacuum.c compares the timestamp in the file's name). With a new
/// file every S days and a limit of N-S days, the oldest entry left is never
/// older than N days. S is one day up to 30 days and grows after that, so
/// the number of files stays far below journald's limit of 100
/// (SystemMaxFiles), which would otherwise cut a long retention short.
fn journal_limits(days: u32) -> (u32, u32) {
    let file = days.div_ceil(30);
    (days - file, file)
}

fn journald_dropin(days: u32) -> String {
    let (retention, file) = journal_limits(days);
    format!(
        "# Managed by vps-panel (Settings > Privacy): keep logs for {days} days.\n\
         # journald deletes a journal file whole once its first entry is older than\n\
         # MaxRetentionSec, and starts a new file every MaxFileSec, so no entry is\n\
         # kept longer than {days} days. The size limit in journald.conf still applies.\n\
         [Journal]\n\
         MaxRetentionSec={retention}day\n\
         MaxFileSec={file}day\n"
    )
}

/// Whether journald started after `dropin` was last written. None when
/// either cannot be told (no such file, no journald).
fn journald_has_read(dropin: &Path) -> Option<bool> {
    let written = epoch_secs(mtime(dropin)?);
    let started = newest_process_start("systemd-journal")?;
    Some(started + START_SLACK >= written)
}

async fn apply_journal(days: Option<u32>) -> Result<(), String> {
    let path = Path::new(JOURNALD_DROPIN);
    let current = tokio::fs::read_to_string(path).await.ok();
    let desired = days.map(journald_dropin);
    if current != desired {
        let written = match &desired {
            Some(body) => match fs_atomic::ensure_dir(Path::new(JOURNALD_DROPIN_DIR), 0o755).await {
                Ok(()) => fs_atomic::write(path, body.as_bytes(), 0o644).await,
                Err(e) => Err(e),
            },
            None => {
                let removed = fs_atomic::remove_if_exists(path).await;
                // The directory too, when it is left empty.
                let _ = tokio::fs::remove_dir(JOURNALD_DROPIN_DIR).await;
                removed
            }
        };
        written.map_err(|e| format!("{JOURNALD_DROPIN} could not be written: {e}."))?;
    } else {
        // Nothing to change. A restart that failed last time is tried again.
        let pending = desired.is_some()
            && tokio::task::spawn_blocking(|| journald_has_read(Path::new(JOURNALD_DROPIN)))
                .await
                .ok()
                .flatten()
                == Some(false);
        if !pending {
            return Ok(());
        }
    }
    // journald reads its configuration only when it starts.
    exec::run("systemctl", ["restart", "systemd-journald"]).await.map_err(|e| {
        let what = match days {
            Some(_) => "The journal's time limit is saved, but journald could not be restarted",
            None => "The journal's time limit is taken out, but journald could not be restarted",
        };
        format!("{what} ({}); the change applies from journald's next start.", why(&e))
    })?;
    if let Some(days) = days {
        // journald deletes by time only when it rotates a file. Rotate and
        // vacuum now, so what is past the limit goes at once.
        let limit = format!("--vacuum-time={}d", journal_limits(days).0);
        exec::run("journalctl", ["--rotate", limit.as_str()]).await.map_err(|e| {
            format!(
                "The journal's time limit is set, but older entries could not be deleted right away \
                 ({}); journald deletes them at its next rotation.",
                why(&e)
            )
        })?;
    }
    tracing::info!(?days, "journal retention updated");
    Ok(())
}

/// A systemd time span ("6day", "1month", "2h 30min", "90") in seconds.
fn parse_timespan(raw: &str) -> Option<u64> {
    let mut total = 0f64;
    let mut rest = raw.trim();
    if rest.is_empty() {
        return None;
    }
    while !rest.is_empty() {
        let digits = rest.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(rest.len());
        let number: f64 = rest[..digits].parse().ok()?;
        rest = rest[digits..].trim_start();
        let letters = rest.find(|c: char| !c.is_alphabetic()).unwrap_or(rest.len());
        let unit = match &rest[..letters] {
            "" | "s" | "sec" | "second" | "seconds" => 1.0,
            "us" | "usec" | "μs" | "µs" => 1e-6,
            "ms" | "msec" => 1e-3,
            "m" | "min" | "minute" | "minutes" => 60.0,
            "h" | "hr" | "hour" | "hours" => 3600.0,
            "d" | "day" | "days" => 86_400.0,
            "w" | "week" | "weeks" => 604_800.0,
            "M" | "month" | "months" => 2_629_800.0,
            "y" | "year" | "years" => 31_557_600.0,
            _ => return None,
        };
        total += number * unit;
        rest = rest[letters..].trim_start();
    }
    Some(total as u64)
}

/// What journald's configuration says once the main file and every drop-in
/// are merged (the output of `systemd-analyze cat-config`: the files in the
/// order journald reads them, the last assignment of a key wins).
#[derive(Debug, Default, PartialEq)]
struct JournaldConf {
    /// MaxRetentionSec in seconds; None when off.
    retention: Option<u64>,
    /// MaxFileSec in seconds; None when no file sets it, Some(0) when off.
    file: Option<u64>,
    max_use: Option<String>,
}

fn journald_conf(cat_config: &str) -> JournaldConf {
    let mut conf = JournaldConf::default();
    for line in cat_config.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "MaxRetentionSec" => conf.retention = parse_timespan(value).filter(|s| *s > 0),
            "MaxFileSec" => conf.file = parse_timespan(value),
            "SystemMaxUse" => conf.max_use = (!value.is_empty()).then(|| value.to_string()),
            _ => {}
        }
    }
    conf
}

impl JournaldConf {
    /// The age the oldest entry can reach: a file is deleted when its first
    /// entry passes the limit, and may by then hold a whole file span more.
    /// None when nothing limits it by time.
    fn longest_kept(&self) -> Option<u64> {
        let retention = self.retention?;
        match self.file.unwrap_or(DEFAULT_FILE_SECS) {
            0 => None,
            span => Some(retention + span),
        }
    }
}

/// When an archived journal file begins: journald puts the time of the
/// file's first entry at the end of its name,
/// `system@<id>-<sequence>-<microseconds, hex>.journal`.
fn journal_file_start(name: &str) -> Option<SystemTime> {
    let stem = name.strip_suffix(".journal")?;
    let (_, at) = stem.split_once('@')?;
    let mut fields = at.rsplit('-');
    let micros = fields.next().filter(|f| f.len() == 16)?;
    fields.next().filter(|f| f.len() == 16)?;
    let micros = u64::from_str_radix(micros, 16).ok()?;
    Some(UNIX_EPOCH + Duration::from_micros(micros))
}

/// The start of the oldest archived journal file of this machine. The files
/// still written to are not counted; with one file per day they are young.
fn oldest_journal_file(dir: &Path) -> Option<SystemTime> {
    let mut oldest: Option<SystemTime> = None;
    for machine in std::fs::read_dir(dir).ok()?.flatten() {
        let Ok(files) = std::fs::read_dir(machine.path()) else {
            continue;
        };
        for file in files.flatten() {
            if let Some(start) = journal_file_start(&file.file_name().to_string_lossy()) {
                oldest = Some(oldest.map_or(start, |o| o.min(start)));
            }
        }
    }
    oldest
}

struct JournalReading {
    conf: Result<JournaldConf, String>,
    read_by_journald: Option<bool>,
    our_file: bool,
    oldest_file_days: Option<u64>,
}

fn days_up(secs: u64) -> u64 {
    secs.div_ceil(DAY)
}

fn journal_part(days: Option<u32>, journal: &JournalReading, files: &[RotateReading]) -> PrivacyPart {
    let mut detail: Vec<String> = Vec::new();
    let mut ok = true;
    let mut unread = false;
    let mut actual = match &journal.conf {
        Err(e) => {
            ok = days.is_none() && !journal.our_file;
            if !ok {
                detail.push(format!("journald's configuration could not be read ({e})."));
            }
            "The journal's settings could not be read.".to_string()
        }
        Ok(conf) => {
            let kept = conf.longest_kept();
            match days {
                None => {
                    if journal.our_file {
                        ok = false;
                        detail.push(format!("{JOURNALD_DROPIN} is still there; apply again."));
                    }
                }
                Some(n) => {
                    if !kept.is_some_and(|k| k <= u64::from(n) * DAY) {
                        ok = false;
                        detail.push(if journal.our_file {
                            format!(
                                "Another file sets MaxRetentionSec or MaxFileSec after {JOURNALD_DROPIN} \
                                 (see `systemd-analyze cat-config systemd/journald.conf`); remove that setting."
                            )
                        } else {
                            "The time limit is not set; apply again.".to_string()
                        });
                    } else if journal.read_by_journald == Some(false) {
                        ok = false;
                        unread = true;
                        detail.push(
                            "journald has not been restarted since the limit was saved: \
                             `systemctl restart systemd-journald`, or apply again."
                                .to_string(),
                        );
                    } else if journal.oldest_file_days.is_some_and(|d| d > u64::from(n)) {
                        ok = false;
                        detail.push(format!(
                            "The oldest journal file starts {} days ago; `journalctl --rotate --vacuum-time={}d` deletes it.",
                            journal.oldest_file_days.unwrap_or(0),
                            journal_limits(n).0
                        ));
                    }
                }
            }
            match kept {
                Some(_) if unread => "The limit is saved, but journald still runs with its earlier settings.".to_string(),
                Some(k) => format!("No journal entry is kept longer than {} days.", days_up(k)),
                None => match &conf.max_use {
                    Some(size) => format!("The journal has no time limit; it is kept up to {size}."),
                    None => "The journal has no time limit, only journald's size limit.".to_string(),
                },
            }
        }
    };
    if !files.is_empty() {
        let list: Vec<String> = files.iter().map(|f| format!("{} {}", f.target.what, f.kept)).collect();
        actual.push_str(&format!(" Text logs in /var/log: {}.", list.join("; ")));
        for f in files {
            ok &= f.ok;
            detail.extend(f.problems.iter().cloned());
        }
    }
    PrivacyPart {
        id: "journal",
        label: JOURNAL_LABEL,
        wanted: match days {
            Some(n) => format!("Deleted after {n} days."),
            None => "No time limit: kept as the system sets it.".to_string(),
        },
        actual,
        ok,
        detail: detail.join(" "),
    }
}

// ---------------------------------------------------------------------------
// Log files logrotate turns over
// ---------------------------------------------------------------------------

/// A package's logrotate file and the copy of it the panel puts in front.
/// Two logrotate entries for one log are an error that stops the later one,
/// and editing the package's file (a conffile) would make its next upgrade
/// ask what to keep. So the package's file is never touched: `over` sorts
/// before it in /etc/logrotate.d and says `ignoreduplicates`, which makes
/// logrotate use the first entry for a log and pass over the later one.
struct RotateTarget {
    /// For the status line.
    what: &'static str,
    package: &'static str,
    over: &'static str,
}

static NGINX_FILES: RotateTarget = RotateTarget {
    what: "nginx's access and error logs",
    package: "/etc/logrotate.d/nginx",
    over: "/etc/logrotate.d/0-vps-panel-nginx",
};

/// Text files that hold the same lines as the journal (rsyslog writes
/// auth.log and mail.log from it) or addresses of their own. Ubuntu keeps
/// four weeks of each, which a shorter limit on the journal alone would not
/// touch. A file whose package is not installed is skipped.
static SYSTEM_FILES: [RotateTarget; 3] = [
    RotateTarget {
        what: "syslog, auth.log and mail.log",
        package: "/etc/logrotate.d/rsyslog",
        over: "/etc/logrotate.d/0-vps-panel-rsyslog",
    },
    RotateTarget {
        what: "ufw.log",
        package: "/etc/logrotate.d/ufw",
        over: "/etc/logrotate.d/0-vps-panel-ufw",
    },
    RotateTarget {
        what: "fail2ban.log",
        package: "/etc/logrotate.d/fail2ban",
        over: "/etc/logrotate.d/0-vps-panel-fail2ban",
    },
];

const LOGROTATE_CONF: &str = "/etc/logrotate.conf";

/// One `paths { ... }` block of a logrotate file.
#[derive(Debug, PartialEq)]
struct Stanza {
    paths: Vec<String>,
    /// Days between rotations, when the block says.
    period: Option<u32>,
    rotate: Option<u32>,
    /// A directive that makes "N old files" mean something else than N
    /// periods; such a block is left alone.
    blocker: Option<String>,
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum Line {
    /// Copied as it is.
    Keep,
    /// The `{` line of stanza n.
    Open(usize),
    /// How often stanza n rotates.
    Frequency(usize),
    /// `rotate`, `maxage`, `ignoreduplicates`: ours replace them.
    Replaced,
}

struct Scan {
    stanzas: Vec<Stanza>,
    /// One entry per line of the text.
    lines: Vec<Line>,
    /// Outside any block: how often and how many, for blocks that do not say.
    period: Option<u32>,
    rotate: Option<u32>,
}

fn period_days(word: &str) -> Option<u32> {
    match word {
        "daily" => Some(1),
        "weekly" => Some(7),
        "monthly" => Some(31),
        "yearly" | "annually" => Some(366),
        _ => None,
    }
}

/// logrotate's files are read a line at a time: the paths, `{`, one
/// directive per line, `}`. A file laid out another way is refused rather
/// than guessed at. `global` allows directives outside a block
/// (/etc/logrotate.conf); in a package's file they would change the defaults
/// of every file read after our copy, so there they are refused.
fn scan_logrotate(text: &str, global: bool) -> Result<Scan, String> {
    #[derive(PartialEq)]
    enum At {
        Outside,
        Block,
        Script,
    }
    let mut scan = Scan { stanzas: Vec::new(), lines: Vec::new(), period: None, rotate: None };
    let mut at = At::Outside;
    let mut paths: Vec<String> = Vec::new();
    let is_path = |w: &str| w.trim_matches(|c| c == '"' || c == '\'').starts_with('/');
    for raw in text.lines() {
        let line = raw.trim();
        let mut words = line.split_whitespace();
        let first = words.next().unwrap_or("");
        let mut kind = Line::Keep;
        match at {
            At::Outside if line.is_empty() || line.starts_with('#') => {}
            At::Outside => {
                let (head, opens) = match line.strip_suffix('{') {
                    Some(head) => (head, true),
                    None => (line, false),
                };
                if head.split_whitespace().all(is_path) && (opens || !head.is_empty()) {
                    paths.extend(head.split_whitespace().map(|w| w.trim_matches(|c| c == '"' || c == '\'').to_string()));
                    if opens {
                        if paths.is_empty() {
                            return Err("a block without a log file".into());
                        }
                        kind = Line::Open(scan.stanzas.len());
                        scan.stanzas.push(Stanza {
                            paths: std::mem::take(&mut paths),
                            period: None,
                            rotate: None,
                            blocker: None,
                        });
                        at = At::Block;
                    }
                } else if global && !line.contains('{') && paths.is_empty() {
                    if let Some(p) = period_days(first) {
                        scan.period = Some(p);
                    } else if first == "rotate" {
                        scan.rotate = words.next().and_then(|n| n.parse().ok());
                    }
                } else {
                    return Err(format!("`{line}` outside a log block"));
                }
            }
            At::Block if line.is_empty() || line.starts_with('#') => {}
            At::Block => {
                let n = scan.stanzas.len() - 1;
                let stanza = &mut scan.stanzas[n];
                match first {
                    "}" if line == "}" => at = At::Outside,
                    "prerotate" | "postrotate" | "firstaction" | "lastaction" | "preremove" => at = At::Script,
                    "rotate" => {
                        stanza.rotate = words.next().and_then(|n| n.parse().ok());
                        kind = Line::Replaced;
                    }
                    "maxage" | "ignoreduplicates" => kind = Line::Replaced,
                    "hourly" | "size" | "minsize" | "maxsize" | "minage" | "dateext" | "dateyesterday"
                    | "datehourago" | "dateformat" | "olddir" | "start" | "extension" | "addextension"
                    | "compressext" => {
                        stanza.blocker.get_or_insert_with(|| first.to_string());
                    }
                    _ if period_days(first).is_some() => {
                        stanza.period = period_days(first);
                        kind = Line::Frequency(n);
                    }
                    _ if line.contains('{') || line.contains('}') => {
                        return Err(format!("`{line}` inside a log block"));
                    }
                    _ => {}
                }
            }
            At::Script => {
                if line == "endscript" {
                    at = At::Block;
                }
            }
        }
        scan.lines.push(kind);
    }
    if at != At::Outside || !paths.is_empty() {
        return Err("a log block that does not end".into());
    }
    Ok(scan)
}

/// How often and how many old files, for blocks that do not say: what
/// /etc/logrotate.conf sets (Ubuntu: weekly, 4).
fn logrotate_defaults() -> (Option<u32>, Option<u32>) {
    std::fs::read_to_string(LOGROTATE_CONF)
        .ok()
        .and_then(|text| scan_logrotate(&text, true).ok())
        .map(|scan| (scan.period, scan.rotate))
        .unwrap_or((None, None))
}

/// The copy of a package's logrotate file that keeps nothing older than
/// `days`. Every block keeps its own rhythm and gets `rotate` set to the
/// number of whole periods that fit, less the one still being written
/// (nginx, daily, 7 days: the live file and 6 old ones). A block that
/// rotates less often than every `days` days is made daily.
fn render_override(target: &RotateTarget, package_text: &str, days: u32, default_period: Option<u32>) -> Result<String, String> {
    let package = target.package;
    let scan = scan_logrotate(package_text, false)
        .map_err(|e| format!("{package} is laid out in a way the panel does not rewrite ({e}); set its `rotate` by hand."))?;
    if scan.stanzas.is_empty() {
        return Err(format!("{package} names no log files."));
    }
    if let Some(word) = scan.stanzas.iter().find_map(|s| s.blocker.as_deref()) {
        return Err(format!(
            "{package} uses `{word}`, which the panel does not rewrite; set how long these logs are kept there by hand."
        ));
    }
    let mut out = format!(
        "# Managed by vps-panel (Settings > Privacy): keep logs for {days} days.\n\
         # A copy of {package} with \"rotate\" set so that no file holds lines older\n\
         # than that. \"ignoreduplicates\" makes logrotate use this copy and pass over\n\
         # the same logs in {package}, which stays as the package installed it.\n\
         # Clearing the setting removes this file.\n"
    );
    for (text, kind) in package_text.lines().zip(&scan.lines) {
        match *kind {
            Line::Replaced => continue,
            Line::Frequency(n) if kept_period(&scan.stanzas[n], days, default_period).is_none() => continue,
            _ => {}
        }
        out.push_str(text);
        out.push('\n');
        if let Line::Open(n) = *kind {
            let period = match kept_period(&scan.stanzas[n], days, default_period) {
                Some(p) => p,
                None => {
                    out.push_str("\tdaily\n");
                    1
                }
            };
            out.push_str(&format!("\trotate {}\n\tmaxage {days}\n\tignoreduplicates\n", days / period - 1));
        }
    }
    Ok(out)
}

/// The block's own period, when it fits into `days` at least once.
fn kept_period(stanza: &Stanza, days: u32, default_period: Option<u32>) -> Option<u32> {
    stanza.period.or(default_period).filter(|p| *p <= days)
}

/// `*` and `?` as a shell matches them in a file name.
fn wildcard(pattern: &[u8], name: &[u8]) -> bool {
    match (pattern.first(), name.first()) {
        (None, None) => true,
        (Some(b'*'), _) => wildcard(&pattern[1..], name) || (!name.is_empty() && wildcard(pattern, &name[1..])),
        (Some(b'?'), Some(_)) => wildcard(&pattern[1..], &name[1..]),
        (Some(p), Some(n)) if p == n => wildcard(&pattern[1..], &name[1..]),
        _ => false,
    }
}

/// The log a rotated file belongs to: `access.log.3.gz` and `access.log.1`
/// give `access.log`. None for a name logrotate's numbering does not make.
fn rotated_base(name: &str) -> Option<&str> {
    let stem = [".gz", ".bz2", ".xz", ".zst", ".lz4", ".Z"]
        .iter()
        .find_map(|ext| name.strip_suffix(ext))
        .unwrap_or(name);
    let (base, number) = stem.rsplit_once('.')?;
    let numbered = !number.is_empty() && number.len() <= 4 && number.bytes().all(|b| b.is_ascii_digit());
    (numbered && !base.is_empty()).then_some(base)
}

/// The rotated copies of the logs `pattern` names (a path, wildcards in its
/// last part only), each with the time it was last written to.
fn rotated_files(pattern: &str) -> Vec<(PathBuf, SystemTime)> {
    let path = Path::new(pattern);
    let (Some(dir), Some(file)) = (path.parent(), path.file_name().and_then(|f| f.to_str())) else {
        return Vec::new();
    };
    if dir.to_string_lossy().contains(['*', '?', '[']) || file.contains('[') {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(base) = name.to_str().and_then(rotated_base) else {
            continue;
        };
        if !wildcard(file.as_bytes(), base.as_bytes()) {
            continue;
        }
        // A link or a directory with such a name is not a log file.
        match std::fs::symlink_metadata(entry.path()) {
            Ok(m) if m.file_type().is_file() => {
                if let Ok(modified) = m.modified() {
                    out.push((entry.path(), modified));
                }
            }
            _ => {}
        }
    }
    out
}

/// Seconds after its last line at which a rotated file is past the limit:
/// its first line is one period older than its last.
fn rotated_limit(days: u32, period: u32) -> u64 {
    u64::from(days.saturating_sub(period)) * DAY
}

/// Delete the rotated files that hold lines older than `days`: the ones a
/// lowered limit leaves behind (logrotate only ever removes the one file
/// past its count), and the ones of a log too quiet to be rotated.
fn sweep(scan: &Scan, days: u32, default_period: Option<u32>) -> usize {
    let mut deleted = 0;
    for stanza in &scan.stanzas {
        let limit = rotated_limit(days, stanza.period.or(default_period).unwrap_or(1));
        for pattern in &stanza.paths {
            for (path, modified) in rotated_files(pattern) {
                if age_secs(modified) > limit && std::fs::remove_file(&path).is_ok() {
                    deleted += 1;
                }
            }
        }
    }
    deleted
}

/// The error lines of a failed `logrotate -d` that are about our file or the
/// one it stands in front of.
fn logrotate_errors(stderr: &str, target: &RotateTarget) -> Vec<String> {
    let base = |p: &'static str| p.rsplit('/').next().unwrap_or(p);
    let (over, package) = (base(target.over), base(target.package));
    stderr
        .lines()
        .filter(|l| l.starts_with("error:"))
        .filter(|l| [over, package].iter().any(|f| l.contains(&format!(" {f}:")) || l.contains(&format!(" {f},"))))
        .map(str::to_string)
        .collect()
}

async fn apply_rotate(target: &'static RotateTarget, days: Option<u32>) -> Result<(), String> {
    let over = Path::new(target.over);
    let current = tokio::fs::read_to_string(over).await.ok();
    let package = tokio::fs::read_to_string(target.package).await.ok();
    let default_period = tokio::task::spawn_blocking(logrotate_defaults).await.map(|d| d.0).unwrap_or(None);
    let desired = match (days, &package) {
        (Some(days), Some(text)) => match render_override(target, text, days, default_period) {
            Ok(body) => Some(body),
            Err(e) => {
                // Not ours to rewrite: a copy from an earlier layout goes.
                let _ = fs_atomic::remove_if_exists(over).await;
                return Err(e);
            }
        },
        // The setting is off, or the package is not installed.
        _ => None,
    };
    if current != desired {
        match &desired {
            None => fs_atomic::remove_if_exists(over)
                .await
                .map_err(|e| format!("{} could not be removed: {e}.", target.over))?,
            Some(body) => {
                fs_atomic::write(over, body.as_bytes(), 0o644)
                    .await
                    .map_err(|e| format!("{} could not be written: {e}.", target.over))?;
                // logrotate reads the whole directory in one run. -d reads
                // it the same way and changes nothing.
                let refused = match exec::run("logrotate", ["-d", LOGROTATE_CONF]).await {
                    Ok(_) => None,
                    Err(AgentError::Subprocess { stderr, .. }) => {
                        let errors = logrotate_errors(&stderr, target);
                        (!errors.is_empty()).then(|| errors.join(" "))
                    }
                    Err(e) => Some(format!("logrotate could not be run ({})", why(&e))),
                };
                if let Some(e) = refused {
                    let _ = match &current {
                        Some(old) => fs_atomic::write(over, old.as_bytes(), 0o644).await,
                        None => fs_atomic::remove_if_exists(over).await,
                    };
                    return Err(format!("logrotate did not accept {}, so it was put back: {e}.", target.over));
                }
                tracing::info!(days, "wrote {}", target.over);
            }
        }
    }
    if let (Some(days), Some(body)) = (days, desired) {
        let deleted = tokio::task::spawn_blocking(move || {
            scan_logrotate(&body, false).map(|scan| sweep(&scan, days, default_period)).unwrap_or(0)
        })
        .await
        .unwrap_or(0);
        if deleted > 0 {
            tracing::info!(deleted, days, "deleted rotated copies of {} past the log retention", target.what);
        }
    }
    Ok(())
}

/// What logrotate does with a target's logs now.
struct RotateReading {
    target: &'static RotateTarget,
    /// "at most 7 days" and the like.
    kept: String,
    ok: bool,
    problems: Vec<String>,
}

/// None when the package's file is not there (not installed).
fn read_rotate(target: &'static RotateTarget, days: Option<u32>) -> Option<RotateReading> {
    let package = std::fs::read_to_string(target.package).ok()?;
    let over = std::fs::read_to_string(target.over).ok();
    let (default_period, default_rotate) = logrotate_defaults();
    let mut problems: Vec<String> = Vec::new();
    // The copy stands in front of the package's file while it exists.
    let governing = over.as_deref().unwrap_or(&package);
    let scan = scan_logrotate(governing, false).ok();
    let longest = scan.as_ref().and_then(|scan| {
        scan.stanzas
            .iter()
            .map(|s| {
                let period = s.period.or(default_period)?;
                Some(u64::from(period) * (u64::from(s.rotate.or(default_rotate)?) + 1))
            })
            .collect::<Option<Vec<u64>>>()
            .and_then(|v| v.into_iter().max())
    });
    match days {
        None => {
            if over.is_some() {
                problems.push(format!("{} is still there; apply again.", target.over));
            }
        }
        Some(n) => match render_override(target, &package, n, default_period) {
            Err(e) => problems.push(e),
            Ok(expected) if over.as_deref() != Some(expected.as_str()) => {
                problems.push(format!("{} is missing or out of date; apply again.", target.over));
            }
            Ok(_) => {
                // A day of slack: the files turn over once a day.
                let stale = scan.iter().flat_map(|s| &s.stanzas).any(|stanza| {
                    let limit = rotated_limit(n, stanza.period.or(default_period).unwrap_or(1)) + DAY;
                    stanza.paths.iter().flat_map(|p| rotated_files(p)).any(|(_, modified)| age_secs(modified) > limit)
                });
                if stale {
                    problems.push(format!("Rotated copies of {} older than {n} days are still there; apply again.", target.what));
                }
            }
        },
    }
    Some(RotateReading {
        target,
        kept: match longest {
            Some(d) => format!("at most {d} days"),
            None => "for a time the panel could not work out".to_string(),
        },
        ok: problems.is_empty(),
        problems,
    })
}

fn nginx_files_part(days: Option<u32>, reading: Option<&RotateReading>) -> PrivacyPart {
    let (actual, ok, detail) = match reading {
        None if !Path::new("/etc/nginx").exists() => ("nginx is not installed.".to_string(), true, String::new()),
        None => (
            format!("{} is not there: nothing rotates nginx's log files.", NGINX_FILES.package),
            days.is_none(),
            match days {
                Some(_) => "Put nginx's logrotate file back (it comes with the nginx-common package), then \
                            apply again; until then delete old files in /var/log/nginx yourself."
                    .to_string(),
                None => String::new(),
            },
        ),
        Some(r) => (
            format!("logrotate keeps {} {}.", r.target.what, r.kept),
            r.ok,
            r.problems.join(" "),
        ),
    };
    PrivacyPart {
        id: "nginx_log_files",
        label: "Web server log files",
        wanted: match days {
            Some(n) => format!("Deleted after {n} days."),
            None => "No change: kept as nginx's package sets it.".to_string(),
        },
        actual,
        ok,
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_default_to_todays_behaviour() {
        assert_eq!(parse_settings(None, None, None), Settings::default());
        assert_eq!(
            parse_settings(Some("masked"), Some("strip"), Some("30")),
            Settings { mask_ip: true, strip_query: true, retention_days: Some(30) }
        );
        assert_eq!(parse_settings(Some("full"), Some("keep"), Some("")), Settings::default());
        // Not one of the documented values: the default, not a guess.
        assert_eq!(parse_settings(Some("MASKED"), Some("yes"), Some("6")), Settings::default());
        for bad in ["3651", "-7", "7.5", "seven", "0"] {
            assert_eq!(parse_settings(None, None, Some(bad)).retention_days, None, "{bad}");
        }
        assert_eq!(parse_settings(None, None, Some(" 7 ")).retention_days, Some(7));
        assert_eq!(parse_settings(None, None, Some("3650")).retention_days, Some(3650));
    }

    #[test]
    fn settings_are_read_from_the_panels_database() {
        let dir = tempfile::tempdir().unwrap();
        let db = PanelDb::open(&dir.path().join("panel.db")).unwrap();
        // Before the panel made its tables.
        assert_eq!(read_settings(&db).unwrap(), Settings::default());
        db.with(|c| c.execute_batch("CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);"))
            .unwrap();
        assert_eq!(read_settings(&db).unwrap(), Settings::default());
        db.with(|c| {
            c.execute_batch(
                "INSERT INTO settings VALUES ('privacy.log_ip', 'masked'), ('privacy.retention_days', '14'),
                 ('privacy.server_geo', 'off'), ('dns.ns_pair', 'a,b');",
            )
        })
        .unwrap();
        assert_eq!(
            read_settings(&db).unwrap(),
            Settings { mask_ip: true, strip_query: false, retention_days: Some(14) }
        );
    }

    #[test]
    fn mask_ip_is_the_contracts_form() {
        for (addr, masked) in [
            ("203.0.113.87", "203.0.113.0"),
            (" 10.1.2.3 ", "10.1.2.0"),
            ("2001:db8:85a3:8d3::7", "2001:db8:85a3::"),
            ("2001:DB8:85A3:8D3::7", "2001:db8:85a3::"),
            ("2001:db8::1", "2001:db8::"),
            ("2001:0:0:1::5", "2001::"),
            ("::1", "::"),
            ("::ffff:203.0.113.87", "::"),
            ("fe80::1", "fe80::"),
        ] {
            assert_eq!(mask_ip(addr).as_deref(), Some(masked), "{addr}");
        }
        for bad in ["", "203.0.113", "example.com", "fe80::1%eth0", "1.2.3.4/24", "1.2.3.4 5.6.7.8"] {
            assert_eq!(mask_ip(bad), None, "{bad}");
        }
    }

    #[test]
    fn proc_stat_gives_the_name_and_the_start_time() {
        let stat = "2377 (nginx) S 2376 2376 2376 0 -1 4194624 97 0 0 0 0 0 0 0 20 0 1 0 5021347 12345 678 18446744073709551615 1 1 0 0 0 0 0 1 2 3 4 5 6 7 8 9 10";
        assert_eq!(parse_proc_stat(stat), Some(("nginx", 5021347)));
        let odd = "9 (a b) c) R 1 9 9 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 77 0 0";
        assert_eq!(parse_proc_stat(odd), Some(("a b) c", 77)));
        assert_eq!(parse_proc_stat("garbage"), None);
    }

    #[test]
    fn a_running_process_has_a_start_time_in_the_past() {
        let own = std::fs::read_to_string("/proc/self/stat").unwrap();
        let (comm, _) = parse_proc_stat(&own).unwrap();
        let started = newest_process_start(comm).unwrap();
        let now = epoch_secs(SystemTime::now());
        assert!(started <= now + 1 && started + 3600 > now, "{started} vs {now}");
        assert_eq!(newest_process_start("no-such-daemon-here"), None);
    }

    #[test]
    fn the_journald_file_for_seven_days() {
        assert_eq!(
            journald_dropin(7),
            "# Managed by vps-panel (Settings > Privacy): keep logs for 7 days.\n\
             # journald deletes a journal file whole once its first entry is older than\n\
             # MaxRetentionSec, and starts a new file every MaxFileSec, so no entry is\n\
             # kept longer than 7 days. The size limit in journald.conf still applies.\n\
             [Journal]\n\
             MaxRetentionSec=6day\n\
             MaxFileSec=1day\n"
        );
        // What it sets reads back as "nothing older than N days", and a long
        // retention does not make more files than journald keeps.
        for days in [7, 8, 29, 30, 31, 90, 365, 3650] {
            let conf = journald_conf(&journald_dropin(days));
            assert_eq!(conf.longest_kept(), Some(u64::from(days) * DAY), "{days}");
            let (retention, file) = journal_limits(days);
            assert!(file >= 1 && retention / file <= 30, "{days}: {retention} / {file}");
        }
        assert_eq!(journal_limits(30), (29, 1));
        assert_eq!(journal_limits(365), (352, 13));
        assert!(journald_dropin(365).ends_with("MaxRetentionSec=352day\nMaxFileSec=13day\n"));
    }

    #[test]
    fn time_spans_as_systemd_writes_them() {
        for (text, secs) in [
            ("6day", 6 * DAY),
            ("1day", DAY),
            ("7d", 7 * DAY),
            ("2 weeks", 14 * DAY),
            ("1month", 2_629_800),
            ("90", 90),
            ("0", 0),
            ("2h 30min", 9000),
            ("1h30m", 5400),
            ("1.5h", 5400),
        ] {
            assert_eq!(parse_timespan(text), Some(secs), "{text}");
        }
        for bad in ["", "soon", "5 fortnights", "day"] {
            assert_eq!(parse_timespan(bad), None, "{bad}");
        }
    }

    /// `systemd-analyze cat-config systemd/journald.conf` on Ubuntu 24.04
    /// after the installer set the size limit, shortened.
    const CAT_CONFIG: &str = "# /etc/systemd/journald.conf\n#  This file is part of systemd.\n\n[Journal]\nStorage=persistent\n#Compress=yes\nSystemMaxUse=1G\n#MaxRetentionSec=\n#MaxFileSec=1month\n\n# /usr/lib/systemd/journald.conf.d/syslog.conf\n[Journal]\nForwardToSyslog=yes\n";

    #[test]
    fn journalds_merged_configuration_is_read() {
        let stock = journald_conf(CAT_CONFIG);
        assert_eq!(stock, JournaldConf { retention: None, file: None, max_use: Some("1G".into()) });
        assert_eq!(stock.longest_kept(), None);

        let ours = format!("{CAT_CONFIG}\n# {JOURNALD_DROPIN}\n{}", journald_dropin(30));
        let conf = journald_conf(&ours);
        assert_eq!(conf.retention, Some(29 * DAY));
        assert_eq!(conf.file, Some(DAY));
        assert_eq!(conf.max_use.as_deref(), Some("1G"));
        assert_eq!(conf.longest_kept(), Some(30 * DAY));

        // A later file wins, and an empty value takes a setting back.
        let later = format!("{ours}\n# /etc/systemd/journald.conf.d/99-own.conf\n[Journal]\nMaxRetentionSec=\n");
        assert_eq!(journald_conf(&later).longest_kept(), None);
        // A limit with the default month-long files keeps up to a month more.
        let coarse = "[Journal]\nMaxRetentionSec=1week\n";
        assert_eq!(journald_conf(coarse).longest_kept(), Some(7 * DAY + DEFAULT_FILE_SECS));
        // Without time-based rotation a file never has to end.
        assert_eq!(journald_conf("[Journal]\nMaxRetentionSec=1week\nMaxFileSec=0\n").longest_kept(), None);
    }

    #[test]
    fn archived_journal_files_carry_the_time_of_their_first_entry() {
        let name = "system@6a1b2c3d4e5f60718293a4b5c6d7e8f9-0000000000012abc-00062a1b2c3d4e5f.journal";
        let start = journal_file_start(name).unwrap();
        assert_eq!(start, UNIX_EPOCH + Duration::from_micros(0x0006_2a1b_2c3d_4e5f));
        assert!(journal_file_start("user-1000@6a1b2c3d4e5f60718293a4b5c6d7e8f9-0000000000012abc-00062a1b2c3d4e5f.journal").is_some());
        // The file being written to, a damaged one, anything else.
        for other in [
            "system.journal",
            "user-1000.journal",
            "system@00062a1b2c3d4e5f-1234567890abcdef.journal~",
            "system@6a1b-12-34.journal",
            "notes.txt",
        ] {
            assert_eq!(journal_file_start(other), None, "{other}");
        }

        let dir = tempfile::tempdir().unwrap();
        let machine = dir.path().join("0123456789abcdef0123456789abcdef");
        std::fs::create_dir(&machine).unwrap();
        for micros in [0x0006_2a1b_2c3d_4e5fu64, 0x0006_0000_0000_0000, 0x0006_3000_0000_0000] {
            std::fs::write(
                machine.join(format!("system@6a1b2c3d4e5f60718293a4b5c6d7e8f9-0000000000000001-{micros:016x}.journal")),
                b"",
            )
            .unwrap();
        }
        std::fs::write(machine.join("system.journal"), b"").unwrap();
        assert_eq!(
            oldest_journal_file(dir.path()),
            Some(UNIX_EPOCH + Duration::from_micros(0x0006_0000_0000_0000))
        );
        assert_eq!(oldest_journal_file(&dir.path().join("missing")), None);
    }

    fn journal(conf: &str, read_by_journald: Option<bool>, our_file: bool, oldest: Option<u64>) -> JournalReading {
        JournalReading {
            conf: Ok(journald_conf(conf)),
            read_by_journald,
            our_file,
            oldest_file_days: oldest,
        }
    }

    #[test]
    fn the_journal_part_says_what_holds() {
        // Nothing wanted, nothing set.
        let part = journal_part(None, &journal(CAT_CONFIG, None, false, Some(200)), &[]);
        assert!(part.ok && part.detail.is_empty(), "{part:?}");
        assert_eq!(part.actual, "The journal has no time limit; it is kept up to 1G.");
        assert_eq!(part.wanted, "No time limit: kept as the system sets it.");

        // Wanted and in force.
        let with_ours = format!("{CAT_CONFIG}\n{}", journald_dropin(7));
        let part = journal_part(Some(7), &journal(&with_ours, Some(true), true, Some(6)), &[]);
        assert!(part.ok, "{part:?}");
        assert_eq!(part.wanted, "Deleted after 7 days.");
        assert_eq!(part.actual, "No journal entry is kept longer than 7 days.");

        // Saved, but journald still runs with what it read at its start.
        let part = journal_part(Some(7), &journal(&with_ours, Some(false), true, Some(6)), &[]);
        assert!(!part.ok && part.detail.contains("restart"), "{part:?}");
        assert_eq!(part.actual, "The limit is saved, but journald still runs with its earlier settings.");
        // Old files that a vacuum should have taken.
        let part = journal_part(Some(7), &journal(&with_ours, Some(true), true, Some(40)), &[]);
        assert!(!part.ok && part.detail.contains("--vacuum-time=6d"), "{part:?}");
        // Wanted, not set at all; and set, then overridden by a later file.
        let part = journal_part(Some(7), &journal(CAT_CONFIG, None, false, None), &[]);
        assert!(!part.ok && part.detail.contains("apply again"), "{part:?}");
        let overridden = format!("{with_ours}\n[Journal]\nMaxRetentionSec=1year\n");
        let part = journal_part(Some(7), &journal(&overridden, Some(true), true, None), &[]);
        assert!(!part.ok && part.detail.contains("Another file"), "{part:?}");
        // Switched off, our file still there.
        let part = journal_part(None, &journal(&with_ours, Some(true), true, None), &[]);
        assert!(!part.ok && part.detail.contains("still there"), "{part:?}");
        // An operator's own, stricter limit is fine.
        let strict = "[Journal]\nMaxRetentionSec=2day\nMaxFileSec=1day\n";
        assert!(journal_part(Some(7), &journal(strict, None, false, Some(2)), &[]).ok);

        // The text files beside the journal count too.
        let files = [RotateReading {
            target: &SYSTEM_FILES[0],
            kept: "at most 35 days".into(),
            ok: false,
            problems: vec!["/etc/logrotate.d/0-vps-panel-rsyslog is missing or out of date; apply again.".into()],
        }];
        let part = journal_part(Some(7), &journal(&with_ours, Some(true), true, Some(6)), &files);
        assert!(!part.ok);
        assert_eq!(
            part.actual,
            "No journal entry is kept longer than 7 days. Text logs in /var/log: syslog, auth.log and mail.log at most 35 days."
        );
        assert!(part.detail.contains("0-vps-panel-rsyslog"));
    }

    fn nginx_state(state: nginx::LogState) -> NginxReading {
        NginxReading::State { state, running: true, reloaded: true, pending: false }
    }

    #[test]
    fn the_nginx_part_says_what_holds() {
        let at = |file: &str, line| nginx::AccessLogAt { file: file.into(), line, format: None };
        let stock = || nginx::LogState { http_full: vec![at(nginx::NGINX_CONF, 41)], ..Default::default() };
        let ours = |masks, strips| nginx::LogState {
            dropin_loaded: true,
            masks,
            strips,
            http_ours: 1,
            ..Default::default()
        };
        let off = Settings::default();
        let both = Settings { mask_ip: true, strip_query: true, retention_days: None };

        let part = nginx_part(off, &nginx_state(stock()));
        assert!(part.ok && part.detail.is_empty(), "{part:?}");
        assert_eq!(part.actual, "Full visitor addresses and full URLs (nginx's standard format).");
        assert_eq!(part.wanted, part.actual);

        let part = nginx_part(both, &nginx_state(ours(true, true)));
        assert!(part.ok && part.detail.is_empty(), "{part:?}");
        assert_eq!(part.actual, "Visitor addresses masked; query strings left out of URLs and referrers.");

        // Saved, not applied yet.
        let part = nginx_part(both, &NginxReading::State { state: stock(), running: true, reloaded: true, pending: true });
        assert!(!part.ok, "{part:?}");
        assert_eq!(part.detail, "The settings are saved but not applied to nginx; apply them again.");
        // Applied, and nginx.conf's own access_log is one the panel leaves alone.
        let part = nginx_part(both, &nginx_state(stock()));
        assert!(!part.ok && part.detail.contains("/etc/nginx/nginx.conf line 41"), "{part:?}");
        // Half of what is wanted.
        assert!(!nginx_part(both, &nginx_state(ours(true, false))).ok);
        // Switched off, still on.
        let part = nginx_part(off, &nginx_state(ours(true, true)));
        assert!(!part.ok && part.detail.contains("apply them again"), "{part:?}");

        // A hand-written vhost with its own log: said, with what to do; no
        // matter while nothing is wanted.
        let mut own = ours(true, true);
        own.elsewhere.push(at("/etc/nginx/sites-enabled/shop", 3));
        let part = nginx_part(both, &nginx_state(own));
        assert!(!part.ok);
        assert!(part.detail.contains("/etc/nginx/sites-enabled/shop line 3"), "{part:?}");
        assert!(part.detail.contains("vps_panel_privacy"), "{part:?}");
        let mut own = stock();
        own.elsewhere.push(at("/etc/nginx/sites-enabled/shop", 3));
        assert!(nginx_part(off, &nginx_state(own)).ok);

        // nginx.conf with a format of the operator's own: named, left alone.
        let mut custom = stock();
        custom.http_full[0].format = Some("main".into());
        let part = nginx_part(both, &nginx_state(custom));
        assert!(!part.ok && part.detail.contains("line 41"), "{part:?}");
        assert_eq!(part.actual, "/etc/nginx/nginx.conf writes the access log in a format of its own (`main`).");

        // Logged twice (a package upgrade put its nginx.conf back).
        let mut twice = ours(true, true);
        twice.http_full.push(at(nginx::NGINX_CONF, 41));
        let part = nginx_part(both, &nginx_state(twice));
        assert!(!part.ok && part.actual.contains("twice"), "{part:?}");

        // Written and tested, but nginx has not reloaded.
        let part = nginx_part(both, &NginxReading::State { state: ours(true, true), running: true, reloaded: false, pending: false });
        assert!(!part.ok && part.detail.contains("Reload nginx"), "{part:?}");
        // Stopped: nothing is logged the old way either.
        let part = nginx_part(both, &NginxReading::State { state: ours(true, true), running: false, reloaded: false, pending: false });
        assert!(part.ok && part.actual.ends_with("nginx is not running."), "{part:?}");

        assert!(nginx_part(off, &NginxReading::NotInstalled).ok);
        assert!(!nginx_part(both, &NginxReading::NotInstalled).ok);
        let part = nginx_part(off, &NginxReading::Broken("nginx: [emerg] unknown directive".into()));
        assert!(!part.ok && part.detail.contains("unknown directive"));
    }

    // The files Ubuntu 24.04's packages install, tabs and all.
    const NGINX_ROTATE: &str = "/var/log/nginx/*.log {\n\tdaily\n\tmissingok\n\trotate 14\n\tcompress\n\tdelaycompress\n\tnotifempty\n\tcreate 0640 www-data adm\n\tsharedscripts\n\tprerotate\n\t\tif [ -d /etc/logrotate.d/httpd-prerotate ]; then \\\n\t\t\trun-parts /etc/logrotate.d/httpd-prerotate; \\\n\t\tfi \\\n\tendscript\n\tpostrotate\n\t\tinvoke-rc.d nginx rotate >/dev/null 2>&1\n\tendscript\n}\n";
    const RSYSLOG_ROTATE: &str = "/var/log/syslog\n/var/log/mail.log\n/var/log/kern.log\n/var/log/auth.log\n/var/log/user.log\n/var/log/cron.log\n{\n\trotate 4\n\tweekly\n\tmissingok\n\tnotifempty\n\tcompress\n\tdelaycompress\n\tsharedscripts\n\tpostrotate\n\t\t/usr/lib/rsyslog/rsyslog-rotate\n\tendscript\n}\n";
    const FAIL2BAN_ROTATE: &str = "/var/log/fail2ban.log {\n\n    weekly\n    rotate 4\n    compress\n    # Do not rotate if empty\n    notifempty\n\n    delaycompress\n    missingok\n    postrotate\n\tfail2ban-client flushlogs 1>/dev/null\n    endscript\n\n    # If fail2ban runs as non-root it still needs to have write access\n    # to logfiles.\n    # create 640 fail2ban adm\n    create 640 root adm\n}\n";
    const LOGROTATE_GLOBAL: &str = "# see \"man logrotate\" for details\n\n# rotate log files weekly\nweekly\n\n# use the adm group by default, since this is the owning group\n# of /var/log/.\nsu root adm\n\n# keep 4 weeks worth of backlogs\nrotate 4\n\n# create new (empty) log files after rotating old ones\ncreate\n\n#dateext\n#compress\n\n# packages drop log rotation information into this directory\ninclude /etc/logrotate.d\n";

    #[test]
    fn logrotate_files_are_read_block_by_block() {
        let scan = scan_logrotate(NGINX_ROTATE, false).unwrap();
        assert_eq!(
            scan.stanzas,
            vec![Stanza { paths: vec!["/var/log/nginx/*.log".into()], period: Some(1), rotate: Some(14), blocker: None }]
        );
        assert_eq!(scan.lines.len(), NGINX_ROTATE.lines().count());
        assert_eq!(scan.lines[0], Line::Open(0));
        assert_eq!(scan.lines[1], Line::Frequency(0));
        assert_eq!(scan.lines[3], Line::Replaced);

        let scan = scan_logrotate(RSYSLOG_ROTATE, false).unwrap();
        assert_eq!(scan.stanzas.len(), 1);
        assert_eq!(scan.stanzas[0].paths.len(), 6);
        assert_eq!((scan.stanzas[0].period, scan.stanzas[0].rotate), (Some(7), Some(4)));

        let global = scan_logrotate(LOGROTATE_GLOBAL, true).unwrap();
        assert_eq!((global.period, global.rotate), (Some(7), Some(4)));
        assert!(global.stanzas.is_empty());

        // A word of a script is not a directive.
        let script = "/var/log/x.log {\n\tpostrotate\n\t\trotate 99\n\t\tsize 5 }\n\tendscript\n}\n";
        let scan = scan_logrotate(script, false).unwrap();
        assert_eq!((scan.stanzas[0].rotate, &scan.stanzas[0].blocker), (None, &None));

        // Layouts that are refused rather than guessed at.
        for odd in [
            "compress\n/var/log/x.log {\n\tdaily\n}\n",
            "/var/log/x.log { daily }\n",
            "/var/log/x.log {\n\tdaily\n",
            "/var/log/x.log\n",
            "{\n\tdaily\n}\n",
        ] {
            assert!(scan_logrotate(odd, false).is_err(), "{odd}");
        }
    }

    #[test]
    fn the_copy_of_nginxs_logrotate_file_for_seven_days() {
        let expected = "# Managed by vps-panel (Settings > Privacy): keep logs for 7 days.\n\
# A copy of /etc/logrotate.d/nginx with \"rotate\" set so that no file holds lines older\n\
# than that. \"ignoreduplicates\" makes logrotate use this copy and pass over\n\
# the same logs in /etc/logrotate.d/nginx, which stays as the package installed it.\n\
# Clearing the setting removes this file.\n\
/var/log/nginx/*.log {\n\trotate 6\n\tmaxage 7\n\tignoreduplicates\n\tdaily\n\tmissingok\n\tcompress\n\tdelaycompress\n\tnotifempty\n\tcreate 0640 www-data adm\n\tsharedscripts\n\tprerotate\n\t\tif [ -d /etc/logrotate.d/httpd-prerotate ]; then \\\n\t\t\trun-parts /etc/logrotate.d/httpd-prerotate; \\\n\t\tfi \\\n\tendscript\n\tpostrotate\n\t\tinvoke-rc.d nginx rotate >/dev/null 2>&1\n\tendscript\n}\n";
        assert_eq!(render_override(&NGINX_FILES, NGINX_ROTATE, 7, Some(7)).unwrap(), expected);
        // More than the package's two weeks works the same way.
        let long = render_override(&NGINX_FILES, NGINX_ROTATE, 90, Some(7)).unwrap();
        assert!(long.contains("\trotate 89\n\tmaxage 90\n\tignoreduplicates\n\tdaily\n"), "{long}");
        // The copy reads back as what it sets.
        let scan = scan_logrotate(&long, false).unwrap();
        assert_eq!((scan.stanzas[0].period, scan.stanzas[0].rotate), (Some(1), Some(89)));
    }

    #[test]
    fn weekly_logs_keep_their_rhythm() {
        let system = &SYSTEM_FILES[0];
        // One week: only the file being written, which is at most a week old.
        let week = render_override(system, RSYSLOG_ROTATE, 7, Some(7)).unwrap();
        assert!(week.contains("{\n\trotate 0\n\tmaxage 7\n\tignoreduplicates\n\tweekly\n\tmissingok\n"), "{week}");
        assert!(week.contains("/var/log/syslog\n/var/log/mail.log\n"));
        // 30 days: four weeks fit.
        let month = render_override(system, RSYSLOG_ROTATE, 30, Some(7)).unwrap();
        assert!(month.contains("\trotate 3\n\tmaxage 30\n"), "{month}");
        // fail2ban's file, with its blank lines, comments and four spaces.
        let f2b = render_override(&SYSTEM_FILES[2], FAIL2BAN_ROTATE, 14, Some(7)).unwrap();
        assert!(f2b.contains("/var/log/fail2ban.log {\n\trotate 1\n\tmaxage 14\n\tignoreduplicates\n\n    weekly\n    compress\n"), "{f2b}");
        assert!(f2b.contains("    # create 640 fail2ban adm\n    create 640 root adm\n}\n"));

        // A block that says nothing takes logrotate.conf's rhythm.
        let silent = "/var/log/x.log {\n\tmissingok\n}\n";
        assert!(render_override(system, silent, 21, Some(7)).unwrap().contains("{\n\trotate 2\n\tmaxage 21\n\tignoreduplicates\n\tmissingok\n"));
        // Monthly does not fit into 7 days: daily, and the old line goes.
        let monthly = "/var/log/x.log {\n\tmonthly\n\trotate 12\n}\n";
        let daily = render_override(system, monthly, 7, Some(7)).unwrap();
        assert!(daily.contains("{\n\tdaily\n\trotate 6\n\tmaxage 7\n\tignoreduplicates\n}\n"), "{daily}");
        // Nothing says how often: daily as well.
        assert!(render_override(system, silent, 7, None).unwrap().contains("{\n\tdaily\n\trotate 6\n"));
    }

    #[test]
    fn a_logrotate_file_the_panel_cannot_reason_about_is_left_alone() {
        let system = &SYSTEM_FILES[0];
        for (text, word) in [
            ("/var/log/x.log {\n\tsize 100M\n\trotate 5\n}\n", "`size`"),
            ("/var/log/x.log {\n\tdaily\n\tdateext\n}\n", "`dateext`"),
            ("/var/log/x.log {\n\thourly\n}\n", "`hourly`"),
            ("compress\n/var/log/x.log {\n\tdaily\n}\n", "outside a log block"),
            ("# nothing here\n", "names no log files"),
        ] {
            let refused = render_override(system, text, 7, Some(7)).unwrap_err();
            assert!(refused.contains(word) && refused.contains("/etc/logrotate.d/rsyslog"), "{refused}");
        }
    }

    #[test]
    fn only_logrotates_numbered_files_count_as_rotated() {
        for (name, base) in [
            ("access.log.1", Some("access.log")),
            ("access.log.14.gz", Some("access.log")),
            ("syslog.2.gz", Some("syslog")),
            ("error.log.3.zst", Some("error.log")),
            ("access.log", None),
            ("access.log.gz", None),
            ("access.log.20261124", None),
            ("access.log.old", None),
            ("access.log-20261124.gz", None),
            (".1", None),
        ] {
            assert_eq!(rotated_base(name), base, "{name}");
        }
        assert!(wildcard(b"*.log", b"access.log"));
        assert!(wildcard(b"redis-server*.log", b"redis-server.log"));
        assert!(wildcard(b"syslog", b"syslog"));
        assert!(wildcard(b"a?c", b"abc"));
        assert!(!wildcard(b"*.log", b"access.log.1"));
        assert!(!wildcard(b"syslog", b"syslog2"));
        assert!(!wildcard(b"a?c", b"ac"));
    }

    fn file_aged(path: &Path, days: f64) {
        std::fs::write(path, b"x").unwrap();
        let when = SystemTime::now() - Duration::from_secs_f64(days * DAY as f64);
        std::fs::File::options().write(true).open(path).unwrap().set_modified(when).unwrap();
    }

    #[test]
    fn the_sweep_deletes_rotated_files_past_the_limit_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let p = |name: &str| d.join(name);
        // Daily files as logrotate leaves them, 14 kept so far.
        file_aged(&p("access.log"), 0.0);
        for n in 1..=14u32 {
            let ext = if n == 1 { "" } else { ".gz" };
            file_aged(&p(&format!("access.log.{n}{ext}")), f64::from(n) - 0.5);
        }
        // A log nothing wrote to for a month, and its old copy.
        file_aged(&p("error.log"), 30.0);
        file_aged(&p("error.log.1"), 31.0);
        // Not logrotate's: never touched, however old.
        file_aged(&p("access.log.2019-backup"), 400.0);
        file_aged(&p("notes.txt.1"), 400.0);
        std::fs::create_dir(p("old.log.1")).unwrap();
        std::os::unix::fs::symlink(p("error.log"), p("link.log.1")).unwrap();

        let text = format!("{}/*.log {{\n\tdaily\n\trotate 6\n}}\n", d.display());
        let scan = scan_logrotate(&text, false).unwrap();
        // 7 days, daily: a file last written more than 6 days ago began
        // more than 7 days ago. .1 to .6 stay, .7 to .14 and error.log.1 go.
        assert_eq!(sweep(&scan, 7, Some(7)), 9);
        let mut left: Vec<String> = std::fs::read_dir(d).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        left.sort();
        assert_eq!(
            left,
            [
                "access.log",
                "access.log.1",
                "access.log.2.gz",
                "access.log.2019-backup",
                "access.log.3.gz",
                "access.log.4.gz",
                "access.log.5.gz",
                "access.log.6.gz",
                "error.log",
                "link.log.1",
                "notes.txt.1",
                "old.log.1",
            ]
        );
        // Again: nothing left to do.
        assert_eq!(sweep(&scan, 7, Some(7)), 0);

        // Weekly files, 30 days: one last written more than 23 days ago
        // holds lines older than 30.
        let weekly = tempfile::tempdir().unwrap();
        let w = weekly.path();
        for (n, age) in [(1, 3.0), (2, 10.0), (3, 17.0), (4, 24.0)] {
            file_aged(&w.join(format!("syslog.{n}.gz")), age);
        }
        let text = format!("{}/syslog\n{{\n\trotate 3\n}}\n", w.display());
        let scan = scan_logrotate(&text, false).unwrap();
        assert_eq!(sweep(&scan, 30, Some(7)), 1);
        assert!(!w.join("syslog.4.gz").exists() && w.join("syslog.3.gz").exists());
    }

    #[test]
    fn only_logrotate_errors_about_our_files_count() {
        let stderr = "reading config file 0-vps-panel-nginx\nerror: 0-vps-panel-nginx:4 unknown option 'rotato' -- ignoring line\nerror: mariadb:3 bad something\nerror: nginx:1 duplicate log entry for /var/log/nginx/access.log\nerror: found error in file nginx, skipping\nrotating pattern: /var/log/nginx/*.log  after 1 days (6 rotations)\n";
        let errors = logrotate_errors(stderr, &NGINX_FILES);
        assert_eq!(errors.len(), 3, "{errors:?}");
        assert!(errors.iter().all(|e| !e.contains("mariadb")));
        assert!(logrotate_errors("error: mariadb:3 bad something\n", &NGINX_FILES).is_empty());
    }

    /// `cargo test -- --ignored the_real_logrotate` on a disposable machine
    /// with nginx's and logrotate's packages: writes into /etc/logrotate.d
    /// and /var/log/nginx and takes everything out again.
    #[tokio::test]
    #[ignore = "root only: needs logrotate and nginx's package files; writes to /etc/logrotate.d and /var/log/nginx (cargo test -- --ignored)"]
    async fn the_real_logrotate_takes_the_copy_and_the_package_file_stays() {
        let package_before = std::fs::read(NGINX_FILES.package).unwrap();
        assert!(!Path::new(NGINX_FILES.over).exists(), "start from an untouched logrotate.d");
        let old = Path::new("/var/log/nginx/access.log.12.gz");
        let recent = Path::new("/var/log/nginx/access.log.2.gz");
        file_aged(old, 11.5);
        file_aged(recent, 1.5);

        apply_rotate(&NGINX_FILES, Some(7)).await.unwrap();
        let copy = std::fs::read_to_string(NGINX_FILES.over).unwrap();
        assert!(copy.contains("\trotate 6\n\tmaxage 7\n\tignoreduplicates\n"), "{copy}");
        assert_eq!(std::fs::read(NGINX_FILES.package).unwrap(), package_before);
        // logrotate reads the directory without an error and rotates the
        // nginx logs by the copy.
        exec::run("logrotate", ["-d", LOGROTATE_CONF]).await.unwrap();
        assert!(!old.exists() && recent.exists());
        let reading = read_rotate(&NGINX_FILES, Some(7)).unwrap();
        assert!(reading.ok, "{:?}", reading.problems);
        assert_eq!(reading.kept, "at most 7 days");

        // A longer limit than the package's own.
        apply_rotate(&NGINX_FILES, Some(60)).await.unwrap();
        assert!(std::fs::read_to_string(NGINX_FILES.over).unwrap().contains("\trotate 59\n"));
        assert_eq!(read_rotate(&NGINX_FILES, Some(60)).unwrap().kept, "at most 60 days");
        assert!(!read_rotate(&NGINX_FILES, Some(7)).unwrap().ok);

        // Cleared: only the package's file is left, unchanged.
        apply_rotate(&NGINX_FILES, None).await.unwrap();
        assert!(!Path::new(NGINX_FILES.over).exists());
        assert_eq!(std::fs::read(NGINX_FILES.package).unwrap(), package_before);
        let reading = read_rotate(&NGINX_FILES, None).unwrap();
        assert!(reading.ok);
        assert_eq!(reading.kept, "at most 15 days");
        std::fs::remove_file(recent).unwrap();
    }

    async fn call(addr: std::net::SocketAddr, method: &str, path: &str, body: Option<&str>) -> (u16, serde_json::Value) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut request = format!("{method} {path} HTTP/1.1\r\nHost: agent\r\nConnection: close\r\n");
        match body {
            Some(b) => request.push_str(&format!("Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{b}", b.len())),
            None => request.push_str("Content-Length: 0\r\n\r\n"),
        }
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut answer = String::new();
        stream.read_to_string(&mut answer).await.unwrap();
        let status: u16 = answer.split_whitespace().nth(1).unwrap().parse().unwrap();
        let json = answer.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
        (status, serde_json::from_str(json).unwrap_or(serde_json::Value::Null))
    }

    /// `cargo test -- --ignored the_endpoints --nocapture`, by itself, on a
    /// disposable machine with nginx running from the stock nginx.conf and
    /// logrotate installed. It turns every setting on through the two routes,
    /// prints what they answer, and turns everything off again. Where
    /// journald cannot be restarted (a container) the journal part says so.
    #[tokio::test]
    #[ignore = "root only: applies the privacy settings to this machine's nginx, logrotate and journald, then takes them back (cargo test -- --ignored)"]
    async fn the_endpoints_apply_and_report_on_a_real_machine() {
        use axum::routing::{get, post};
        let nginx_before = std::fs::read(nginx::NGINX_CONF).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let db = PanelDb::open(&dir.path().join("panel.db")).unwrap();
        db.with(|c| {
            c.execute_batch(
                "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO settings VALUES ('privacy.log_ip', 'masked'), ('privacy.log_query', 'strip'),
                 ('privacy.retention_days', '7');",
            )
        })
        .unwrap();
        let state = AppState {
            config: std::sync::Arc::new(crate::config::Config {
                socket_path: dir.path().join("agent.sock"),
                agent_token: "t".repeat(32),
                panel_db_path: dir.path().join("panel.db"),
                mariadb_url: "mysql://root@localhost:3306/mysql".into(),
                restic_password: String::new(),
                restic_repository: String::new(),
                panel_host: "panel.test".into(),
                acme_email: String::new(),
                acme_directory: String::new(),
                vps_panel_gid: 0,
                vps_panel_uid: None,
                apps_uid: 0,
                apps_gid: 0,
            }),
            panel_db: db.clone(),
            mariadb: crate::db::MariaDb::connect("mysql://root@localhost:3306/mysql").unwrap(),
            tasks: crate::ipc::tasks::TaskTracker::new(db.clone()),
        };
        let app = axum::Router::new()
            .route("/privacy/status", get(status))
            .route("/privacy/apply", post(apply))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        // Before anything is applied: the settings are stored, the system
        // still does what it did.
        let (code, before) = call(addr, "GET", "/privacy/status", None).await;
        println!("--- status before apply\n{}", serde_json::to_string_pretty(&before).unwrap());
        assert_eq!(code, 200);
        assert_eq!(before["log_ip"], "masked");
        assert_eq!(before["retention_days"], 7);
        assert_eq!(before["parts"][0]["ok"], false);
        assert_eq!(std::fs::read(nginx::NGINX_CONF).unwrap(), nginx_before);

        // The panel sends `{}`; no body at all is as good.
        let (code, on) = call(addr, "POST", "/privacy/apply", Some("{}")).await;
        println!("--- apply, everything on\n{}", serde_json::to_string_pretty(&on).unwrap());
        assert_eq!(code, 200);
        let part = |status: &serde_json::Value, id: &str| {
            status["parts"].as_array().unwrap().iter().find(|p| p["id"] == id).unwrap().clone()
        };
        assert_eq!(part(&on, "nginx_logs")["ok"], true, "{on}");
        assert_eq!(part(&on, "nginx_log_files")["ok"], true, "{on}");
        assert!(Path::new(NGINX_FILES.over).exists() && Path::new(nginx::LOG_PRIVACY_CONF).exists());
        let (code, again) = call(addr, "POST", "/privacy/apply", None).await;
        assert_eq!(code, 200);
        assert_eq!(part(&again, "nginx_logs"), part(&on, "nginx_logs"));
        let (_, read) = call(addr, "GET", "/privacy/status", None).await;
        assert_eq!(part(&read, "nginx_logs"), part(&on, "nginx_logs"));
        assert_eq!(part(&read, "nginx_log_files"), part(&on, "nginx_log_files"));

        // Everything off: what the packages installed, to the byte.
        db.with(|c| c.execute("DELETE FROM settings", [])).unwrap();
        let (code, off) = call(addr, "POST", "/privacy/apply", Some("{}")).await;
        println!("--- apply, everything off\n{}", serde_json::to_string_pretty(&off).unwrap());
        assert_eq!(code, 200);
        assert_eq!(off["log_ip"], "full");
        assert_eq!(off["retention_days"], serde_json::Value::Null);
        assert_eq!(part(&off, "nginx_logs")["ok"], true, "{off}");
        assert_eq!(part(&off, "nginx_log_files")["ok"], true, "{off}");
        assert_eq!(std::fs::read(nginx::NGINX_CONF).unwrap(), nginx_before);
        for gone in [nginx::LOG_PRIVACY_CONF, NGINX_FILES.over, JOURNALD_DROPIN] {
            assert!(!Path::new(gone).exists(), "{gone}");
        }
    }
}
