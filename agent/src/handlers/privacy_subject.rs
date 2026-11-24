//! Personal data requests (Settings > Privacy): what this server holds about
//! one email address or one IP address outside panel.db, and erasing it from
//! the sources where that can be done safely.
//!
//! Search is read-only. Every source has a time limit and a size limit, and
//! one that cannot be read answers with a note instead of failing the call.
//! Erase replaces the subject inside nginx's log lines and the logs of
//! finished tasks (an IP by its masked form, an email by `[erased]`) and
//! touches nothing else: the journal cannot be edited, a ban is the
//! operator's decision, and mailboxes belong to the Mailboxes page.
//!
//! The value is never passed to a shell. It reaches a regular expression and
//! journalctl's --grep only escaped, and it is never written to the agent's
//! own log (which is the journal this tool searches).

use crate::error::{AgentError, AgentResult};
use crate::exec;
use crate::handlers::mail::mail_conn;
use crate::state::AppState;
use axum::{extract::State, Json};
use mysql_async::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime};

/// Samples per source, newest first.
const MAX_SAMPLES: usize = 20;
/// One sample is cut after this many characters.
const MAX_SAMPLE_CHARS: usize = 500;
/// A search stops reading one source after this long, and all of them after
/// SEARCH_TOTAL_LIMIT (they run side by side, so the two rarely differ). The
/// panel waits 280 s for the answer.
const SEARCH_SOURCE_LIMIT: Duration = Duration::from_secs(45);
const SEARCH_TOTAL_LIMIT: Duration = Duration::from_secs(60);
/// Bytes of log text (after decompression) one file source reads at most.
const SEARCH_MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Log files one source looks at, newest first.
const MAX_LOG_FILES: usize = 400;
/// Matching journal lines read at most (mail_log.rs has the same limit).
const JOURNAL_MAX_LINES: u64 = 200_000;
/// One journal line is cut after this many bytes before it is looked at.
const JOURNAL_MAX_LINE_BYTES: usize = 64 * 1024;
/// Bytes of task logs read at most.
const TASKS_MAX_BYTES: u64 = 512 * 1024 * 1024;
/// The quick sources (fail2ban, the mail queue, a maildir's size).
const QUICK_LIMIT: Duration = Duration::from_secs(10);
/// An erase starts no new log file after this long ...
const ERASE_SOFT_LIMIT: Duration = Duration::from_secs(120);
/// ... and gives up on the file it is rewriting after this long. That file
/// is then left exactly as it was. (nginx in front of the panel waits 300 s.)
const ERASE_HARD_LIMIT: Duration = Duration::from_secs(240);
/// How long nginx gets to reopen a log that was replaced under it.
const REOPEN_WAIT: Duration = Duration::from_secs(15);
/// A process that still writes to the replaced file after REOPEN_WAIT is
/// followed for this long: what it writes there is copied to the new file.
const FOLLOW_LIMIT: Duration = Duration::from_secs(30 * 60);
/// Free space a rewrite wants on top of the size of the file.
const REWRITE_HEADROOM: u64 = 32 * 1024 * 1024;

const NGINX_LOG_DIR: &str = "/var/log/nginx";
const TRASH_DIR: &str = "/var/lib/vps-panel/trash";
const FAIL2BAN_DB: &str = "/var/lib/fail2ban/fail2ban.sqlite3";

const JOURNAL_KEPT: &str =
    "The system journal cannot be edited; these lines go when the log retention removes them.";

// ---------------------------------------------------------------------------
// Request and response shapes (the privacy contract)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct SubjectReq {
    pub kind: String,
    pub value: String,
}

#[derive(Deserialize)]
pub struct EraseReq {
    pub kind: String,
    pub value: String,
    #[serde(default)]
    pub sources: Vec<String>,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct SubjectSource {
    pub id: String,
    pub label: String,
    /// Matching records: lines, rows, messages.
    pub count: u64,
    /// Up to MAX_SAMPLES of them, newest first.
    pub samples: Vec<String>,
    /// More may exist than were counted: a limit was reached or a part of
    /// the source could not be read. The note says which.
    pub truncated: bool,
    /// POST /privacy/subject/erase acts on this source.
    pub erasable: bool,
    pub note: String,
}

#[derive(Serialize)]
pub struct SubjectReport {
    pub kind: &'static str,
    /// The value as it was searched for (trimmed, lowercased, an address in
    /// its usual text form).
    pub value: String,
    pub sources: Vec<SubjectSource>,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct EraseOutcome {
    pub id: String,
    pub ok: bool,
    /// Records really rewritten.
    pub changed: u64,
    pub detail: String,
}

#[derive(Serialize)]
pub struct SubjectEraseResult {
    pub results: Vec<EraseOutcome>,
}

fn source(id: &str, label: &str, erasable: bool, note: impl Into<String>) -> SubjectSource {
    SubjectSource {
        id: id.into(),
        label: label.into(),
        count: 0,
        samples: Vec::new(),
        truncated: false,
        erasable,
        note: note.into(),
    }
}

/// The source ids this agent knows, and whether erase acts on them.
const SOURCES: [(&str, bool); 13] = [
    ("nginx_access", true),
    ("nginx_error", true),
    ("journal_mail", false),
    ("journal_ssh", false),
    ("journal_other", false),
    ("system_logs", false),
    ("fail2ban", false),
    ("mailboxes", false),
    ("forwarders", false),
    ("maildir", false),
    ("mail_trash", false),
    ("mail_queue", false),
    ("task_logs", true),
];

// ---------------------------------------------------------------------------
// The subject: validation, exact matching, the replacement text
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Email,
    Ip,
}

#[derive(Clone)]
enum Finder {
    /// An escaped literal (or a few spellings of one), with the characters
    /// around a hit checked in code.
    Literal { re: regex::bytes::Regex, email: bool },
    /// IPv6 has many spellings of one address: every address-shaped token is
    /// parsed and compared.
    V6(Ipv6Addr),
}

#[derive(Clone)]
pub(crate) struct Subject {
    kind: Kind,
    /// Normalised: what is searched for and echoed back.
    value: String,
    ip: Option<IpAddr>,
    finder: Finder,
    /// What erase puts in its place.
    replacement: String,
}

/// Characters accepted in the local part besides letters, digits and the
/// dot: what RFC 5321 allows without quoting, less `$`, the backtick and `|`,
/// which no real address has.
const EMAIL_LOCAL_EXTRA: &[u8] = b"!#%&'*+/=?^_{}~-";

/// One email address, lowercased. Quoted local parts, display names, address
/// lists and non-ASCII domains (give those in their xn-- form) are refused.
fn parse_email(raw: &str) -> Result<String, &'static str> {
    const WHAT: &str = "enter one email address, like name@example.com";
    let email = raw.trim().to_ascii_lowercase();
    if email.len() > 254 || !email.is_ascii() {
        return Err(WHAT);
    }
    let Some((local, domain)) = email.rsplit_once('@') else {
        return Err(WHAT);
    };
    let local_ok = !local.is_empty()
        && local.len() <= 64
        && local
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || EMAIL_LOCAL_EXTRA.contains(&b))
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..");
    if !local_ok || domain.len() > 253 || !crate::validate::is_safe_subdomain(domain) {
        return Err(WHAT);
    }
    Ok(email)
}

/// One IPv4 or IPv6 address. No range, no port, no brackets, no zone. An
/// IPv4 address written as IPv6 (`::ffff:203.0.113.5`) is that IPv4 address.
fn parse_ip(raw: &str) -> Result<IpAddr, &'static str> {
    const WHAT: &str = "enter one IPv4 or IPv6 address, like 203.0.113.5 or 2001:db8::1";
    let raw = raw.trim();
    if raw.len() > 45 {
        return Err(WHAT);
    }
    let ip = raw.parse::<IpAddr>().map_err(|_| WHAT)?.to_canonical();
    if ip.is_unspecified() {
        return Err(WHAT);
    }
    Ok(ip)
}

/// The masked form of the privacy contract: IPv4 with the last octet zeroed,
/// IPv6 cut to its first three groups.
fn masked(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, c, _] = v4.octets();
            IpAddr::V4(Ipv4Addr::new(a, b, c, 0))
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            IpAddr::V6(Ipv6Addr::new(s[0], s[1], s[2], 0, 0, 0, 0, 0))
        }
    }
}

/// encodeURIComponent: how the address looks inside a query string.
fn percent_encoded(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The spellings of an email address a log line can have: as it is, with
/// only the `@` percent-encoded, and fully percent-encoded.
fn email_spellings(email: &str) -> Vec<String> {
    let mut out = vec![email.to_string()];
    for s in [email.replace('@', "%40"), percent_encoded(email)] {
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

/// All eight groups with their leading zeros, as the kernel logs an IPv6
/// address (UFW's `SRC=`).
fn v6_expanded(v6: &Ipv6Addr) -> String {
    v6.segments().iter().map(|g| format!("{g:04x}")).collect::<Vec<_>>().join(":")
}

/// A backslash before everything that is not a letter or a digit: in PCRE2
/// (journalctl --grep) that always means the character itself.
fn pcre_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        if !c.is_ascii_alphanumeric() {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

impl Subject {
    pub(crate) fn parse(kind: &str, value: &str) -> AgentResult<Subject> {
        let invalid = |why: &str| AgentError::Invalid(why.to_string());
        match kind {
            "email" => {
                let email = parse_email(value).map_err(invalid)?;
                let spellings: Vec<String> =
                    email_spellings(&email).iter().map(|s| regex::escape(s)).collect();
                let re = regex::bytes::RegexBuilder::new(&spellings.join("|"))
                    .case_insensitive(true)
                    .unicode(false)
                    .build()
                    .map_err(|_| invalid("this address cannot be searched for"))?;
                Ok(Subject {
                    kind: Kind::Email,
                    value: email,
                    ip: None,
                    finder: Finder::Literal { re, email: true },
                    replacement: "[erased]".into(),
                })
            }
            "ip" => {
                let ip = parse_ip(value).map_err(invalid)?;
                let finder = match ip {
                    IpAddr::V4(v4) => Finder::Literal {
                        re: regex::bytes::RegexBuilder::new(&regex::escape(&v4.to_string()))
                            .unicode(false)
                            .build()
                            .map_err(|_| invalid("this address cannot be searched for"))?,
                        email: false,
                    },
                    IpAddr::V6(v6) => Finder::V6(v6),
                };
                Ok(Subject {
                    kind: Kind::Ip,
                    value: ip.to_string(),
                    ip: Some(ip),
                    finder,
                    replacement: masked(ip).to_string(),
                })
            }
            _ => Err(invalid("kind must be \"email\" or \"ip\"")),
        }
    }

    fn kind_str(&self) -> &'static str {
        match self.kind {
            Kind::Email => "email",
            Kind::Ip => "ip",
        }
    }

    /// Erasing would put the same text back (an address that is already in
    /// its masked form).
    fn is_its_own_replacement(&self) -> bool {
        self.value == self.replacement
    }

    /// The next exact occurrence in `hay` at or after `from`, as a byte
    /// range. `hay` starts at the start of a line.
    fn next_match(&self, hay: &[u8], from: usize) -> Option<(usize, usize)> {
        match &self.finder {
            Finder::Literal { re, email } => {
                let mut at = from;
                while at <= hay.len() {
                    let m = re.find_at(hay, at)?;
                    let (s, e) = (m.start(), m.end());
                    let exact = if *email { email_bounds(hay, s, e) } else { v4_bounds(hay, s, e) };
                    if exact {
                        return Some((s, e));
                    }
                    at = s + 1;
                }
                None
            }
            Finder::V6(want) => next_v6(hay, from, want),
        }
    }

    /// Call `on_line` with every line of `part` that holds the subject.
    fn matching_lines<'a>(&self, part: &'a [u8], mut on_line: impl FnMut(&'a [u8])) {
        let mut pos = 0;
        while let Some((s, e)) = self.next_match(part, pos) {
            let start = part[..s].iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
            let end = part[e..].iter().position(|&b| b == b'\n').map_or(part.len(), |i| e + i);
            on_line(&part[start..end]);
            pos = end;
        }
    }

    /// `part` with every occurrence replaced, appended to `out`. Returns the
    /// number of lines that changed.
    fn rewrite(&self, part: &[u8], out: &mut Vec<u8>) -> u64 {
        let mut lines = 0;
        let mut written = 0;
        let mut line_end = 0;
        let mut pos = 0;
        while let Some((s, e)) = self.next_match(part, pos) {
            out.extend_from_slice(&part[written..s]);
            out.extend_from_slice(self.replacement.as_bytes());
            written = e;
            pos = e;
            if s >= line_end {
                lines += 1;
                line_end = part[e..].iter().position(|&b| b == b'\n').map_or(part.len(), |i| e + i);
            }
        }
        out.extend_from_slice(&part[written..]);
        lines
    }

    /// The pattern for `journalctl --grep`: only a first filter (it knows
    /// nothing of what stands around a hit), every spelling escaped.
    fn journal_pattern(&self) -> String {
        let spellings = match (&self.kind, self.ip) {
            (Kind::Email, _) => email_spellings(&self.value),
            (_, Some(IpAddr::V6(v6))) => vec![v6.to_string(), v6_expanded(&v6)],
            _ => vec![self.value.clone()],
        };
        let escaped: Vec<String> = spellings.iter().map(|s| pcre_escape(s)).collect();
        format!("(?:{})", escaped.join("|"))
    }
}

/// `1.2.3.4` is not found inside `1.2.3.45`, `11.2.3.4`, `1.2.3.4.5` or a
/// version number like `v1.2.3.4`. `::ffff:1.2.3.4` and `1.2.3.4:443` are
/// that address.
fn v4_bounds(hay: &[u8], s: usize, e: usize) -> bool {
    let before = s == 0 || !(hay[s - 1].is_ascii_alphanumeric() || hay[s - 1] == b'.');
    let after = match hay.get(e) {
        None => true,
        Some(b) if b.is_ascii_alphanumeric() => false,
        // The end of a sentence, or a host name like 1.2.3.4.example.net.
        Some(b'.') => !hay.get(e + 1).is_some_and(|c| c.is_ascii_digit()),
        Some(_) => true,
    };
    before && after
}

fn hex_val(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}

/// `jane@example.com` is not found inside `maryjane@example.com`,
/// `mary.jane@example.com`, `jane@example.com.au` or `jane@example.community`.
/// A percent-encoded character before it (`to%3Ajane%40example.com`) counts
/// as the character it stands for.
fn email_bounds(hay: &[u8], s: usize, e: usize) -> bool {
    let before = if s == 0 {
        true
    } else {
        let escaped = (s >= 3 && hay[s - 3] == b'%')
            .then(|| Some(hex_val(hay[s - 2])? * 16 + hex_val(hay[s - 1])?))
            .flatten();
        let b = escaped.unwrap_or(hay[s - 1]);
        !(b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
    };
    let after = match hay.get(e) {
        None => true,
        Some(b) if b.is_ascii_alphanumeric() || *b == b'-' => false,
        Some(b'.') => !hay.get(e + 1).is_some_and(|c| c.is_ascii_alphanumeric()),
        Some(_) => true,
    };
    before && after
}

fn is_v6_byte(b: u8) -> bool {
    b.is_ascii_hexdigit() || b == b':' || b == b'.'
}

/// The next token in `hay` that is an IPv6 address equal to `want`, in any
/// spelling. A token is a whole run of hex digits, colons and dots, less a
/// colon or dot of the sentence around it; so `2001:db8::1` is not found
/// inside `2001:db8::12` or `1:2001:db8::1`, which are other addresses.
fn next_v6(hay: &[u8], from: usize, want: &Ipv6Addr) -> Option<(usize, usize)> {
    let mut i = from;
    while i < hay.len() {
        if !is_v6_byte(hay[i]) {
            i += 1;
            continue;
        }
        let start = i;
        let mut colons = 0;
        while i < hay.len() && is_v6_byte(hay[i]) {
            colons += usize::from(hay[i] == b':');
            i += 1;
        }
        if colons < 2 {
            continue;
        }
        let (mut s, mut e) = (start, i);
        // "from 2001:db8::1." and "2001:db8::1: refused", but not the `::`
        // an address can end or start with.
        while e > s && (hay[e - 1] == b'.' || (hay[e - 1] == b':' && e - s >= 2 && hay[e - 2] != b':')) {
            e -= 1;
        }
        while e > s && (hay[s] == b'.' || (hay[s] == b':' && e - s >= 2 && hay[s + 1] != b':')) {
            s += 1;
        }
        if e - s < 2 || e - s > 45 {
            continue;
        }
        let parsed = std::str::from_utf8(&hay[s..e]).ok().and_then(|t| t.parse::<Ipv6Addr>().ok());
        if parsed.as_ref() == Some(want) {
            return Some((s, e));
        }
    }
    None
}

/// A matching record as it is shown to an admin: one line, no control or
/// direction-changing characters, at most MAX_SAMPLE_CHARS characters.
fn clean_sample(raw: &[u8]) -> String {
    // A line can be megabytes; only its beginning is ever shown.
    let cut = raw.len() > MAX_SAMPLE_CHARS * 4 + 64;
    let text = String::from_utf8_lossy(if cut { &raw[..MAX_SAMPLE_CHARS * 4 + 64] } else { raw });
    let mut kept: Vec<char> = text
        .chars()
        .filter_map(|c| match c {
            '\t' => Some(' '),
            c if c.is_control() => None,
            '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}' => None,
            c => Some(c),
        })
        .take(MAX_SAMPLE_CHARS + 1)
        .collect();
    if kept.len() > MAX_SAMPLE_CHARS || cut {
        kept.truncate(MAX_SAMPLE_CHARS - 1);
        kept.push('…');
    }
    kept.into_iter().collect()
}

// ---------------------------------------------------------------------------
// Log files: which ones, and reading them within limits
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Packed {
    Plain,
    Gzip,
    /// Compressed with something else (logrotate's compresscmd): not read.
    Other,
}

#[derive(Clone, Debug)]
struct LogFile {
    path: PathBuf,
    name: String,
    packed: Packed,
    mtime: SystemTime,
}

/// What logrotate leaves after a log's own name: nothing, `.1`, `-20261124`,
/// and a compression suffix after either.
fn rotation(rest: &str) -> Option<Packed> {
    let (rest, packed) = if let Some(r) = rest.strip_suffix(".gz") {
        (r, Packed::Gzip)
    } else if let Some(r) = [".zst", ".xz", ".bz2", ".lz4", ".Z"].iter().find_map(|x| rest.strip_suffix(x)) {
        (r, Packed::Other)
    } else {
        (rest, Packed::Plain)
    };
    let numbered = rest.is_empty()
        || (rest.len() >= 2
            && (rest.starts_with('.') || rest.starts_with('-'))
            && rest[1..].bytes().all(|b| b.is_ascii_digit() || b == b'-'));
    numbered.then_some(packed)
}

/// Which files of a directory are logs.
#[derive(Clone, Copy)]
enum Names {
    /// Every `<anything>.log` and its rotations.
    AnyLog,
    /// These base names and their rotations.
    Bases(&'static [&'static str]),
}

/// The log's own name and how the file is packed; None for a file that is
/// not one of the logs.
fn log_name(name: &str, names: Names) -> Option<(&str, Packed)> {
    if name.starts_with('.') {
        return None;
    }
    match names {
        Names::AnyLog => name.match_indices(".log").find_map(|(i, _)| {
            let (base, rest) = name.split_at(i + 4);
            rotation(rest).map(|p| (base, p))
        }),
        Names::Bases(bases) => bases.iter().find_map(|base| {
            name.strip_prefix(base).and_then(rotation).map(|p| (*base, p))
        }),
    }
}

/// The regular files of `dir` that are logs and whose own name `want`
/// accepts, newest first. Links are not followed.
fn list_logs(dir: &Path, names: Names, want: &dyn Fn(&str) -> bool) -> io::Result<Vec<LogFile>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some((base, packed)) = log_name(&name, names) else { continue };
        if !want(base) {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else { continue };
        if !meta.is_file() {
            continue;
        }
        out.push(LogFile {
            path: entry.path(),
            name,
            packed,
            mtime: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        });
    }
    out.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| a.name.cmp(&b.name)));
    Ok(out)
}

fn is_error_log(base: &str) -> bool {
    base.contains("error")
}

/// nginx's access logs in `dir` (every log there that is not an error log)
/// or its error logs.
fn nginx_logs(dir: &Path, errors: bool) -> io::Result<Vec<LogFile>> {
    list_logs(dir, Names::AnyLog, &|base| is_error_log(base) == errors)
}

/// Text logs outside nginx's directory that name addresses: rsyslog's copies
/// of the journal, the firewall's and fail2ban's own logs, Rspamd, MariaDB
/// and PostgreSQL.
const SYSTEM_LOGS: [(&str, Names); 4] = [
    (
        "/var/log",
        Names::Bases(&[
            "syslog", "auth.log", "mail.log", "mail.err", "mail.warn", "mail.info", "kern.log",
            "ufw.log", "fail2ban.log", "daemon.log", "user.log", "messages", "debug",
        ]),
    ),
    ("/var/log/rspamd", Names::AnyLog),
    ("/var/log/mysql", Names::AnyLog),
    ("/var/log/postgresql", Names::AnyLog),
];

fn system_logs() -> Vec<LogFile> {
    let mut out = Vec::new();
    for (dir, names) in SYSTEM_LOGS {
        out.extend(list_logs(Path::new(dir), names, &|_| true).unwrap_or_default());
    }
    out.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| a.name.cmp(&b.name)));
    out
}

/// A command the agent runs itself, without its secrets (exec.rs does the
/// same for the ones that go through it).
fn tool(cmd: &str) -> std::process::Command {
    let mut command = std::process::Command::new(cmd);
    for name in exec::AGENT_SECRET_ENV {
        command.env_remove(name);
    }
    command
}

/// A child that is killed and reaped when it goes out of scope.
struct Kid(std::process::Child);

impl Drop for Kid {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Open a log for reading without following a link. O_NONBLOCK so a FIFO put
/// in its place cannot hang the open.
fn open_log(path: &Path) -> io::Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    Ok(file)
}

/// How reading a file ended.
#[derive(Debug, PartialEq, Clone, Copy)]
enum End {
    Eof,
    /// The caller had seen enough.
    Stopped,
    Time,
    Bytes,
}

const READ_CHUNK: usize = 1024 * 1024;
/// A "line" longer than this is handed on in pieces.
const MAX_LINE: usize = 4 * 1024 * 1024;

/// Hand `r` to `f` in pieces that end at a line end, until it ends, `f`
/// returns false, `deadline` passes or `bytes_left` is used up.
fn read_parts(
    r: &mut dyn Read,
    deadline: Instant,
    bytes_left: &mut u64,
    f: &mut dyn FnMut(&[u8]) -> io::Result<bool>,
) -> io::Result<End> {
    let mut buf: Vec<u8> = Vec::with_capacity(READ_CHUNK * 2);
    loop {
        if Instant::now() >= deadline {
            return Ok(End::Time);
        }
        if *bytes_left == 0 {
            return Ok(End::Bytes);
        }
        let old = buf.len();
        let want = READ_CHUNK.min(usize::try_from(*bytes_left).unwrap_or(READ_CHUNK));
        buf.resize(old + want, 0);
        let n = match r.read(&mut buf[old..]) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                buf.truncate(old);
                continue;
            }
            Err(e) => return Err(e),
        };
        buf.truncate(old + n);
        *bytes_left -= n as u64;
        if n == 0 {
            if !buf.is_empty() && !f(&buf)? {
                return Ok(End::Stopped);
            }
            return Ok(End::Eof);
        }
        let cut = match buf.iter().rposition(|&b| b == b'\n') {
            Some(i) => i + 1,
            None if buf.len() >= MAX_LINE => buf.len(),
            None => continue,
        };
        if !f(&buf[..cut])? {
            return Ok(End::Stopped);
        }
        buf.drain(..cut);
    }
}

/// Read one log file (through `gzip -dc` when it is compressed; the file is
/// gzip's standard input, its name never an argument).
fn read_log(
    file: &LogFile,
    deadline: Instant,
    bytes_left: &mut u64,
    f: &mut dyn FnMut(&[u8]) -> io::Result<bool>,
) -> Result<End, String> {
    let src = open_log(&file.path).map_err(|e| e.to_string())?;
    read_opened(src, file.packed, deadline, bytes_left, f)
}

fn read_opened(
    mut src: std::fs::File,
    packed: Packed,
    deadline: Instant,
    bytes_left: &mut u64,
    f: &mut dyn FnMut(&[u8]) -> io::Result<bool>,
) -> Result<End, String> {
    match packed {
        Packed::Plain => read_parts(&mut src, deadline, bytes_left, f).map_err(|e| e.to_string()),
        Packed::Gzip => {
            let child = tool("gzip")
                .arg("-dc")
                .stdin(Stdio::from(src))
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("gzip could not be started: {e}"))?;
            let mut kid = Kid(child);
            let mut out = kid.0.stdout.take().expect("stdout piped");
            let end = read_parts(&mut out, deadline, bytes_left, f).map_err(|e| e.to_string())?;
            drop(out);
            if end == End::Eof && !kid.0.wait().map(|s| s.success()).unwrap_or(false) {
                return Err("is not a complete gzip file".into());
            }
            Ok(end)
        }
        Packed::Other => Err("is compressed in a format that is not read here".into()),
    }
}

#[derive(Default, Debug)]
struct FileHits {
    count: u64,
    samples: Vec<String>,
    truncated: bool,
    /// What could not be read, and why a search stopped early.
    problems: Vec<String>,
}

/// Count the lines of `files` (newest first) that hold the subject.
fn search_files(files: &[LogFile], subj: &Subject, deadline: Instant, max_bytes: u64) -> FileHits {
    let mut hits = FileHits::default();
    let mut bytes_left = max_bytes;
    if files.len() > MAX_LOG_FILES {
        hits.truncated = true;
        hits.problems.push(format!(
            "only the newest {MAX_LOG_FILES} of {} files were read",
            files.len()
        ));
    }
    let files = &files[..files.len().min(MAX_LOG_FILES)];
    for (i, file) in files.iter().enumerate() {
        // The last matching lines of this file are its newest.
        let room = MAX_SAMPLES - hits.samples.len();
        let mut newest: VecDeque<String> = VecDeque::with_capacity(room);
        let mut count = 0u64;
        let end = read_log(file, deadline, &mut bytes_left, &mut |part| {
            subj.matching_lines(part, |line| {
                count += 1;
                if room > 0 {
                    if newest.len() == room {
                        newest.pop_front();
                    }
                    newest.push_back(clean_sample(line));
                }
            });
            Ok(true)
        });
        hits.count += count;
        hits.samples.extend(newest.into_iter().rev());
        match end {
            Ok(End::Eof) | Ok(End::Stopped) => {}
            Ok(stopped) => {
                hits.truncated = true;
                let left = files.len() - i;
                hits.problems.push(match stopped {
                    End::Time => format!(
                        "the search stopped at its time limit with {left} older files not read to the end"
                    ),
                    _ => format!(
                        "the search stopped after reading {} GB, with {left} older files not read to the end",
                        max_bytes / (1024 * 1024 * 1024)
                    ),
                });
                break;
            }
            Err(why) => {
                hits.truncated = true;
                hits.problems.push(format!("{} {why}", file.name));
            }
        }
    }
    hits
}

// ---------------------------------------------------------------------------
// Erasing inside log files
// ---------------------------------------------------------------------------

/// What happened to the log files of one source.
#[derive(Default, Debug)]
struct ErasedFiles {
    /// Lines rewritten.
    changed: u64,
    /// Files that had such lines.
    files: u64,
    /// Files that were left as they are, and why.
    problems: Vec<String>,
    /// Things to know about files that were rewritten.
    notes: Vec<String>,
}

struct EraseLimits {
    /// No new file is started after this.
    soft: Instant,
    /// The file in work is given up after this.
    hard: Instant,
    reopen_wait: Duration,
}

impl EraseLimits {
    fn starting_now() -> Self {
        let now = Instant::now();
        EraseLimits { soft: now + ERASE_SOFT_LIMIT, hard: now + ERASE_HARD_LIMIT, reopen_wait: REOPEN_WAIT }
    }
}

fn erase_in_files(dir: &Path, files: &[LogFile], subj: &Subject, limits: &EraseLimits) -> ErasedFiles {
    let mut done = ErasedFiles::default();
    // Files are created and renamed in `dir`: only in a real directory that
    // is root's own (or the agent's, where it does not run as root). In one
    // another user can write to, a name could be swapped under the rename.
    let trusted = std::fs::symlink_metadata(dir).is_ok_and(|m| {
        m.is_dir() && (m.uid() == 0 || m.uid() == nix::unistd::geteuid().as_raw()) && m.mode() & 0o022 == 0
    });
    if !trusted {
        if !files.is_empty() {
            done.problems.push(format!(
                "{} is not a directory only root can write to, so nothing in it was rewritten",
                dir.display()
            ));
        }
        return done;
    }
    inplace::remove_stale_copies(dir);
    for (i, file) in files.iter().enumerate() {
        if Instant::now() >= limits.soft {
            done.problems.push(format!(
                "stopped at the time limit with {} files not looked at; run the erase again to go on",
                files.len() - i
            ));
            break;
        }
        match inplace::rewrite(file, subj, limits) {
            Ok(r) => {
                done.changed += r.changed;
                done.files += u64::from(r.changed > 0);
                done.notes.extend(r.note);
            }
            Err(why) => done.problems.push(why),
        }
    }
    done
}

#[cfg(not(target_os = "linux"))]
mod inplace {
    use super::*;

    pub(super) struct Rewritten {
        pub changed: u64,
        pub note: Option<String>,
    }

    pub(super) fn remove_stale_copies(_dir: &Path) {}

    pub(super) fn rewrite(file: &LogFile, _subj: &Subject, _limits: &EraseLimits) -> Result<Rewritten, String> {
        Err(format!("{}: log files can only be rewritten on Linux", file.name))
    }
}

/// Rewriting a log file that a server may be writing to.
///
/// nginx holds its logs open and appends to them. Writing a corrected copy
/// and renaming it over the log would lose every line nginx writes to the old
/// file afterwards, and truncating the log in place loses the lines written
/// between the copy and the truncation. So this does what logrotate's
/// `create` mode does, without losing the tail:
///
/// 1. write the corrected copy next to the log, with the log's owner, mode
///    and times;
/// 2. swap the two names in one step (RENAME_EXCHANGE): the log's name is
///    the copy now, and the old file, which nginx still writes to, has the
///    temporary name;
/// 3. tell nginx to reopen its logs (USR1, as logrotate's postrotate does)
///    and wait until no process has the old file open for writing;
/// 4. append what was written to the old file in the meantime to the new
///    one, and delete the old file.
///
/// A rotated file nobody writes to takes the same path; steps 3 and 4 then
/// find nothing to do. A `.gz` is unpacked, corrected and packed again.
#[cfg(target_os = "linux")]
mod inplace {
    use super::*;
    use std::collections::HashSet;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Mutex, OnceLock};

    pub(super) struct Rewritten {
        pub changed: u64,
        pub note: Option<String>,
    }

    /// Part of the temporary name of a copy (and, after the swap, of the old
    /// file).
    const COPY_MARK: &str = ".vps-panel-erase.";

    /// Old files a thread of this process is still following (see `follow`).
    fn followed() -> &'static Mutex<HashSet<PathBuf>> {
        static FOLLOWED: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
        FOLLOWED.get_or_init(Default::default)
    }

    /// Copies an agent that was stopped mid-erase left behind. One erase
    /// runs at a time, so anything with the mark that is not being followed
    /// is such a leftover; it holds log lines and must not stay.
    pub(super) fn remove_stale_copies(dir: &Path) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let followed = followed().lock().unwrap_or_else(|e| e.into_inner()).clone();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') && name.contains(COPY_MARK) && !followed.contains(&entry.path()) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    /// Device and inode, read the same way for our own files and for the
    /// ones other processes hold (through /proc), so the two always compare.
    type FileId = (u64, u64);

    fn id_of_fd(pid: &str, fd: &str) -> Option<FileId> {
        let m = std::fs::metadata(format!("/proc/{pid}/fd/{fd}")).ok()?;
        Some((m.dev(), m.ino()))
    }

    fn id_of(file: &std::fs::File) -> io::Result<FileId> {
        id_of_fd("self", &file.as_raw_fd().to_string())
            .ok_or_else(|| io::Error::other("/proc is not readable"))
    }

    fn id_of_path(path: &Path) -> Option<FileId> {
        let handle = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_PATH | nix::libc::O_NOFOLLOW)
            .open(path)
            .ok()?;
        id_of(&handle).ok()
    }

    #[derive(Debug, Clone)]
    pub(super) struct Writer {
        pub pid: i32,
        pub comm: String,
    }

    fn comm_of(pid: i32) -> String {
        std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }

    /// The processes that have the file open for writing.
    pub(super) fn writers_of(id: FileId) -> Vec<Writer> {
        let mut out: Vec<Writer> = Vec::new();
        let me = std::process::id().to_string();
        let Ok(procs) = std::fs::read_dir("/proc") else { return out };
        for proc in procs.flatten() {
            let pid = proc.file_name().to_string_lossy().into_owned();
            if pid == me || !pid.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else { continue };
            for fd in fds.flatten() {
                let fd = fd.file_name().to_string_lossy().into_owned();
                if id_of_fd(&pid, &fd) != Some(id) {
                    continue;
                }
                // "flags:\t0102001": octal open flags; the last digit is the
                // access mode (1 write only, 2 read and write).
                let writes = std::fs::read_to_string(format!("/proc/{pid}/fdinfo/{fd}"))
                    .ok()
                    .and_then(|info| {
                        let flags = info.lines().find_map(|l| l.strip_prefix("flags:"))?.trim().to_string();
                        u32::from_str_radix(&flags, 8).ok()
                    })
                    // Unreadable: assume it writes rather than lose its lines.
                    .is_none_or(|flags| flags & 3 != 0);
                if writes {
                    if let Ok(pid) = pid.parse::<i32>() {
                        out.push(Writer { pid, comm: comm_of(pid) });
                    }
                    break;
                }
            }
        }
        out
    }

    /// The only writer this knows how to ask to reopen its logs.
    fn can_reopen(w: &Writer) -> bool {
        w.comm == "nginx"
    }

    /// USR1 to the nginx master of every writer: it reopens its logs and
    /// tells its workers to (what `nginx -s reopen` and logrotate do).
    fn ask_to_reopen(writers: &[Writer]) {
        let mut masters: Vec<i32> = Vec::new();
        for w in writers.iter().filter(|w| can_reopen(w)) {
            // "<pid> (<comm>) <state> <ppid> ...": the fields follow the last ')'.
            let parent = std::fs::read_to_string(format!("/proc/{}/stat", w.pid)).ok().and_then(|stat| {
                stat.rsplit(')').next()?.split_whitespace().nth(1)?.parse::<i32>().ok()
            });
            let master = match parent {
                Some(p) if p > 1 && comm_of(p) == "nginx" => p,
                _ => w.pid,
            };
            if !masters.contains(&master) {
                masters.push(master);
            }
        }
        for pid in masters {
            let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), nix::sys::signal::Signal::SIGUSR1);
        }
    }

    /// Swap two names in one step. Neither name is ever missing, so a server
    /// reopening its log at that moment finds one of the two files.
    fn exchange(a: &Path, b: &Path) -> io::Result<()> {
        nix::fcntl::renameat2(None, a, None, b, nix::fcntl::RenameFlags::RENAME_EXCHANGE)
            .map_err(io::Error::from)
    }

    /// The corrected copy while it is written; after the swap, the name of
    /// the old file. Removed when it goes out of scope, unless kept.
    struct Copy {
        path: PathBuf,
        file: std::fs::File,
        keep: bool,
    }

    impl Copy {
        fn create(of: &Path, name: &str, uid: u32, gid: u32) -> io::Result<Copy> {
            let dir = of.parent().unwrap_or(Path::new("."));
            let path = dir.join(format!(
                ".{name}{COPY_MARK}{}",
                &uuid::Uuid::new_v4().simple().to_string()[..12]
            ));
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(nix::libc::O_NOFOLLOW)
                .open(&path)?;
            let copy = Copy { path, file, keep: false };
            std::os::unix::fs::fchown(&copy.file, Some(uid), Some(gid))?;
            Ok(copy)
        }

        /// Give the copy the mode and times of the file it replaces and make
        /// sure it is on disk.
        fn finish(&self, like: &std::fs::Metadata) -> io::Result<()> {
            self.file.set_permissions(std::fs::Permissions::from_mode(like.mode() & 0o7777))?;
            self.file.sync_data()?;
            self.file.set_times(
                std::fs::FileTimes::new().set_accessed(like.accessed()?).set_modified(like.modified()?),
            )
        }
    }

    impl Drop for Copy {
        fn drop(&mut self) {
            if !self.keep {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }

    fn room_for(file: &LogFile, size: u64) -> Result<(), String> {
        let dir = file.path.parent().unwrap_or(Path::new("."));
        match nix::sys::statvfs::statvfs(dir) {
            Ok(vfs) => {
                // The two are not u64 on every Linux.
                #[allow(clippy::unnecessary_cast)]
                let free = (vfs.blocks_available() as u64).saturating_mul(vfs.fragment_size() as u64);
                if free < size.saturating_add(REWRITE_HEADROOM) {
                    return Err(format!(
                        "{}: not enough free disk space to rewrite it ({} MB free, {} MB needed)",
                        file.name,
                        free / (1024 * 1024),
                        (size + REWRITE_HEADROOM) / (1024 * 1024)
                    ));
                }
                Ok(())
            }
            // Not knowing is no reason to refuse; a full disk fails the copy.
            Err(_) => Ok(()),
        }
    }

    fn rotated(name: &str) -> String {
        format!("{name} was rotated while it was being rewritten and was left as it is; run the erase again")
    }

    pub(super) fn rewrite(file: &LogFile, subj: &Subject, limits: &EraseLimits) -> Result<Rewritten, String> {
        let name = &file.name;
        let fail = |e: io::Error| format!("{name}: {e}");
        if file.packed == Packed::Other {
            return Err(format!("{name} is compressed in a format that cannot be rewritten here"));
        }
        let src = match open_log(&file.path) {
            Ok(f) => f,
            // Rotated away or deleted since the directory was listed.
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Rewritten { changed: 0, note: None }),
            Err(e) => return Err(fail(e)),
        };
        let meta = src.metadata().map_err(fail)?;
        let id = id_of(&src).map_err(fail)?;

        // Most files hold nothing about the subject and are only read.
        let mut found = false;
        let mut unlimited = u64::MAX;
        let scanned = read_opened(src.try_clone().map_err(fail)?, file.packed, limits.hard, &mut unlimited, &mut |part| {
            found = subj.next_match(part, 0).is_some();
            Ok(!found)
        })
        .map_err(|why| format!("{name} {why}"))?;
        if scanned == End::Time {
            return Err(format!("{name} could not be read within the time limit and was left as it is"));
        }
        if !found {
            return Ok(Rewritten { changed: 0, note: None });
        }

        room_for(file, meta.len())?;
        match file.packed {
            Packed::Plain => rewrite_plain(file, subj, limits, src, meta, id),
            _ => rewrite_gz(file, subj, limits, src, meta, id),
        }
    }

    fn timed_out(name: &str) -> String {
        format!("{name} could not be rewritten within the time limit and was left as it is")
    }

    fn rewrite_plain(
        file: &LogFile,
        subj: &Subject,
        limits: &EraseLimits,
        src: std::fs::File,
        meta: std::fs::Metadata,
        id: FileId,
    ) -> Result<Rewritten, String> {
        let name = &file.name;
        let fail = |e: io::Error| format!("{name}: {e}");
        if let Some(w) = writers_of(id).iter().find(|w| !can_reopen(w)) {
            return Err(format!(
                "{name} is open for writing by {} (pid {}), which cannot be told to reopen it, so it was left as it is",
                if w.comm.is_empty() { "a process" } else { &w.comm },
                w.pid
            ));
        }

        let mut copy = Copy::create(&file.path, name, meta.uid(), meta.gid()).map_err(fail)?;
        let mut changed = 0u64;
        let mut copied = 0u64;
        if !copy_from(&src, &mut copied, meta.len(), &mut &copy.file, subj, &mut changed, false, limits.hard)
            .map_err(fail)?
        {
            return Err(timed_out(name));
        }
        copy.file.sync_data().map_err(fail)?;
        // What arrived during the copy and the sync, so the swap leaves
        // little behind. The rest follows after the swap.
        let now = src.metadata().map_err(fail)?;
        if !copy_from(&src, &mut copied, now.len(), &mut &copy.file, subj, &mut changed, false, limits.hard)
            .map_err(fail)?
        {
            return Err(timed_out(name));
        }
        copy.finish(&now).map_err(fail)?;
        let copy_len = copy.file.metadata().map_err(fail)?.len();

        if id_of_path(&file.path) != Some(id) {
            return Err(rotated(name));
        }
        exchange(&copy.path, &file.path).map_err(fail)?;
        // From here on `copy.path` is the name of the old file.

        if id_of_path(&copy.path) != Some(id) {
            // The log was rotated in the instant before the swap: what has
            // the temporary name now was never read. Put it back; anything a
            // server appended to the copy in between follows it there.
            if let Err(e) = exchange(&copy.path, &file.path) {
                copy.keep = true;
                return Err(format!(
                    "{name} was rotated while it was being rewritten and could not be put back ({e}); its content is in {}",
                    copy.path.display()
                ));
            }
            if let Ok(copy_id) = id_of(&copy.file) {
                let (mut at, mut none) = (copy_len, 0u64);
                let _ = retire(&copy.file, copy_id, &mut at, &file.path, subj, &mut none, limits.reopen_wait);
            }
            return Err(rotated(name));
        }

        match retire(&src, id, &mut copied, &file.path, subj, &mut changed, limits.reopen_wait) {
            Ok(None) => Ok(Rewritten { changed, note: None }),
            Ok(Some(w)) => {
                let note = format!(
                    "{} (pid {}) had not reopened {name} after {} s; what it still writes to the old file is copied to the new one for the next {} minutes",
                    if w.comm.is_empty() { "a process" } else { &w.comm },
                    w.pid,
                    limits.reopen_wait.as_secs(),
                    FOLLOW_LIMIT.as_secs() / 60
                );
                copy.keep = true;
                follow(src, id, copied, copy.path.clone(), file.path.clone(), subj.clone());
                Ok(Rewritten { changed, note: Some(note) })
            }
            // The old file goes with `copy`; what was copied is in place.
            Err(e) => Err(format!("{name}: the lines written during the rewrite could not all be carried over: {e}")),
        }
    }

    /// Copy `src` from `*offset` up to `upto` to `out` with the subject
    /// replaced: whole lines only, and the unfinished last one too when
    /// `last`. `*offset` moves past what was copied. `upto` is the size the
    /// file had when the caller looked: a server can append faster than this
    /// copies, and a copy that chased the end would never finish.
    /// Ok(false): the deadline passed.
    #[allow(clippy::too_many_arguments)]
    fn copy_from(
        src: &std::fs::File,
        offset: &mut u64,
        upto: u64,
        out: &mut dyn Write,
        subj: &Subject,
        changed: &mut u64,
        last: bool,
        deadline: Instant,
    ) -> io::Result<bool> {
        let mut buf: Vec<u8> = Vec::with_capacity(READ_CHUNK * 2);
        let mut fixed: Vec<u8> = Vec::with_capacity(READ_CHUNK * 2);
        loop {
            if Instant::now() >= deadline {
                return Ok(false);
            }
            let old = buf.len();
            let at = *offset + old as u64;
            let want = usize::try_from(upto.saturating_sub(at)).unwrap_or(READ_CHUNK).min(READ_CHUNK);
            buf.resize(old + want, 0);
            let n = match src.read_at(&mut buf[old..], at) {
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                    buf.truncate(old);
                    continue;
                }
                Err(e) => return Err(e),
            };
            buf.truncate(old + n);
            // `upto` reached, or the file is shorter than it was.
            let ended = n == 0;
            let cut = if ended {
                if !last || buf.is_empty() {
                    return Ok(true);
                }
                buf.len()
            } else {
                match buf.iter().rposition(|&b| b == b'\n') {
                    Some(i) => i + 1,
                    None if buf.len() >= MAX_LINE => buf.len(),
                    None => continue,
                }
            };
            fixed.clear();
            *changed += subj.rewrite(&buf[..cut], &mut fixed);
            // One write per piece: appended to a log that a server appends
            // to as well, the lines of the two never mix.
            out.write_all(&fixed)?;
            *offset += cut as u64;
            buf.drain(..cut);
            if ended {
                return Ok(true);
            }
        }
    }

    /// Append what `old` has from `*offset` on to the log at `live`.
    fn carry_over(
        old: &std::fs::File,
        offset: &mut u64,
        live: &Path,
        subj: &Subject,
        changed: &mut u64,
        last: bool,
    ) -> io::Result<()> {
        let meta = old.metadata()?;
        if meta.len() <= *offset {
            // Nothing new: the log is not even opened, and keeps its times.
            return Ok(());
        }
        let opened = std::fs::OpenOptions::new()
            .append(true)
            .custom_flags(nix::libc::O_NOFOLLOW)
            .open(live);
        let mut out = match opened {
            Ok(f) => f,
            // logrotate is between its rename and its create.
            Err(e) if e.kind() == io::ErrorKind::NotFound && !last => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let f = std::fs::OpenOptions::new()
                    .append(true)
                    .create(true)
                    .mode(meta.mode() & 0o7777)
                    .custom_flags(nix::libc::O_NOFOLLOW)
                    .open(live)?;
                let _ = std::os::unix::fs::fchown(&f, Some(meta.uid()), Some(meta.gid()));
                f
            }
            Err(e) => return Err(e),
        };
        let far = Instant::now() + Duration::from_secs(3600);
        copy_from(old, offset, meta.len(), &mut out, subj, changed, last, far).map(|_| ())
    }

    /// The old file after the swap: ask its writers to reopen, carry over
    /// what they still write to it, until nobody has it open for writing.
    /// Ok(None) when that is so; Ok(Some(writer)) when `wait` passed first.
    fn retire(
        old: &std::fs::File,
        id: FileId,
        offset: &mut u64,
        live: &Path,
        subj: &Subject,
        changed: &mut u64,
        wait: Duration,
    ) -> io::Result<Option<Writer>> {
        let started = Instant::now();
        let mut asked: Option<Instant> = None;
        let mut quiet = 0;
        loop {
            let writers = writers_of(id);
            if writers.is_empty() {
                quiet += 1;
            } else {
                quiet = 0;
                if asked.is_none_or(|t| t.elapsed() >= Duration::from_secs(2)) {
                    ask_to_reopen(&writers);
                    asked = Some(Instant::now());
                }
            }
            // Twice in a row: a process forked while /proc was being read
            // is in the second reading.
            let last = quiet >= 2;
            carry_over(old, offset, live, subj, changed, last)?;
            if last {
                return Ok(None);
            }
            if started.elapsed() >= wait {
                if let Some(w) = writers.into_iter().next() {
                    return Ok(Some(w));
                }
            }
            std::thread::sleep(if quiet > 0 {
                Duration::from_millis(10)
            } else if started.elapsed() > Duration::from_secs(20) {
                Duration::from_secs(1)
            } else {
                Duration::from_millis(50)
            });
        }
    }

    /// A writer that did not reopen in time: keep carrying its lines over on
    /// a thread of its own, then delete the old file. What was already
    /// copied is freed right away, so the erased lines are gone from the
    /// disk even while the old file still has a name.
    fn follow(old: std::fs::File, id: FileId, mut offset: u64, old_path: PathBuf, live: PathBuf, subj: Subject) {
        if let Ok(len) = nix::libc::off_t::try_from(offset) {
            let _ = nix::fcntl::fallocate(
                old.as_raw_fd(),
                nix::fcntl::FallocateFlags::FALLOC_FL_PUNCH_HOLE | nix::fcntl::FallocateFlags::FALLOC_FL_KEEP_SIZE,
                0,
                len,
            );
        }
        followed().lock().unwrap_or_else(|e| e.into_inner()).insert(old_path.clone());
        let name = old_path.clone();
        let work = move || {
            let mut changed = 0u64;
            let _ = retire(&old, id, &mut offset, &live, &subj, &mut changed, FOLLOW_LIMIT);
            let _ = carry_over(&old, &mut offset, &live, &subj, &mut changed, true);
            let _ = std::fs::remove_file(&old_path);
            followed().lock().unwrap_or_else(|e| e.into_inner()).remove(&old_path);
        };
        // No thread to be had: the old file goes now, with what is still
        // written to it.
        if let Err(e) = std::thread::Builder::new().name("log-follow".into()).spawn(work) {
            tracing::warn!("could not start a thread to follow a replaced log: {e}");
            let _ = std::fs::remove_file(&name);
            followed().lock().unwrap_or_else(|e| e.into_inner()).remove(&name);
        }
    }

    fn rewrite_gz(
        file: &LogFile,
        subj: &Subject,
        limits: &EraseLimits,
        mut src: std::fs::File,
        meta: std::fs::Metadata,
        id: FileId,
    ) -> Result<Rewritten, String> {
        use std::io::Seek;
        let name = &file.name;
        let fail = |e: io::Error| format!("{name}: {e}");
        let mut copy = Copy::create(&file.path, name, meta.uid(), meta.gid()).map_err(fail)?;
        src.seek(io::SeekFrom::Start(0)).map_err(fail)?;

        let unpack = tool("gzip")
            .arg("-dc")
            .stdin(Stdio::from(src.try_clone().map_err(fail)?))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(fail)?;
        let mut unpack = Kid(unpack);
        let pack = tool("gzip")
            .arg("-nc")
            .stdin(Stdio::piped())
            .stdout(Stdio::from(copy.file.try_clone().map_err(fail)?))
            .stderr(Stdio::null())
            .spawn()
            .map_err(fail)?;
        let mut pack = Kid(pack);

        let mut text = unpack.0.stdout.take().expect("stdout piped");
        let mut packed = pack.0.stdin.take().expect("stdin piped");
        let mut changed = 0u64;
        let mut fixed: Vec<u8> = Vec::with_capacity(READ_CHUNK * 2);
        let mut unlimited = u64::MAX;
        let end = read_parts(&mut text, limits.hard, &mut unlimited, &mut |part| {
            fixed.clear();
            changed += subj.rewrite(part, &mut fixed);
            packed.write_all(&fixed)?;
            Ok(true)
        })
        .map_err(fail)?;
        drop(text);
        drop(packed);
        if end != End::Eof {
            return Err(timed_out(name));
        }
        let unpacked = unpack.0.wait().map(|s| s.success()).unwrap_or(false);
        let repacked = pack.0.wait().map(|s| s.success()).unwrap_or(false);
        if !unpacked {
            return Err(format!("{name} is not a complete gzip file and was left as it is"));
        }
        if !repacked {
            return Err(format!("{name} could not be compressed again and was left as it is"));
        }
        if changed == 0 {
            return Ok(Rewritten { changed: 0, note: None });
        }

        copy.finish(&meta).map_err(fail)?;
        if id_of_path(&file.path) != Some(id) {
            return Err(rotated(name));
        }
        exchange(&copy.path, &file.path).map_err(fail)?;
        if id_of_path(&copy.path) != Some(id) {
            // Rotated in the instant before the swap: put the other file back.
            if let Err(e) = exchange(&copy.path, &file.path) {
                copy.keep = true;
                return Err(format!(
                    "{name} was rotated while it was being rewritten and could not be put back ({e}); its content is in {}",
                    copy.path.display()
                ));
            }
            return Err(rotated(name));
        }
        Ok(Rewritten { changed, note: None })
    }
}

/// One erase at a time. Held on the blocking thread that does the work, so
/// a caller that gives up waiting does not let a second one start under it.
static ERASE_RUNNING: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn erased_files_outcome(id: &str, subj: &Subject, done: ErasedFiles) -> EraseOutcome {
    let mut detail = if done.changed == 0 {
        "No line holds it.".to_string()
    } else {
        format!(
            "Replaced with {} in {} {} of {} {}.",
            subj.replacement,
            done.changed,
            if done.changed == 1 { "line" } else { "lines" },
            done.files,
            if done.files == 1 { "file" } else { "files" },
        )
    };
    for p in done.notes.iter().chain(&done.problems) {
        detail.push(' ');
        detail.push_str(&sentence(p));
    }
    EraseOutcome { id: id.into(), ok: done.problems.is_empty(), changed: done.changed, detail }
}

/// `text` as a sentence: a capital first, a full stop last.
fn sentence(text: &str) -> String {
    let text = text.trim();
    let mut out = String::with_capacity(text.len() + 1);
    let mut chars = text.chars();
    if let Some(first) = chars.next() {
        // A file name keeps its case.
        if text.split_whitespace().next().is_some_and(|w| w.contains('.')) {
            out.push(first);
        } else {
            out.extend(first.to_uppercase());
        }
        out.push_str(chars.as_str());
    }
    if !out.ends_with(['.', '!', '?']) {
        out.push('.');
    }
    out
}

// ---------------------------------------------------------------------------
// The system journal
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum JournalClass {
    Mail,
    Ssh,
    Other,
}

/// Which service wrote a `journalctl -o short-iso` line
/// (`<time> <host> <ident>[<pid>]: <text>`); None for the continuation of a
/// message of several lines.
fn journal_class(line: &str) -> Option<JournalClass> {
    if !line.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let ident = line.split(' ').nth(2)?;
    let ident = ident.split(['[', ':']).next().unwrap_or(ident);
    Some(if ident.starts_with("postfix") || ident.starts_with("dovecot") {
        JournalClass::Mail
    } else if ident.starts_with("sshd") {
        JournalClass::Ssh
    } else {
        JournalClass::Other
    })
}

#[derive(Default, Debug)]
struct Bucket {
    count: u64,
    samples: Vec<String>,
}

#[derive(Default, Debug)]
struct JournalHits {
    mail: Bucket,
    ssh: Bucket,
    other: Bucket,
    truncated: bool,
    /// Why the journal could not be read (to the end).
    problem: Option<String>,
}

impl JournalHits {
    /// One line of journalctl's output, newest first.
    fn take(&mut self, line: &[u8], subj: &Subject, last_class: &mut JournalClass) {
        let text = String::from_utf8_lossy(line);
        let class = journal_class(&text).unwrap_or(*last_class);
        *last_class = class;
        if subj.next_match(line, 0).is_none() {
            return;
        }
        let bucket = match class {
            JournalClass::Mail => &mut self.mail,
            JournalClass::Ssh => &mut self.ssh,
            JournalClass::Other => &mut self.other,
        };
        bucket.count += 1;
        if bucket.samples.len() < MAX_SAMPLES {
            bucket.samples.push(clean_sample(line));
        }
    }
}

/// How a streamed command ended.
struct Streamed {
    /// None: it was stopped (the deadline, or the caller had enough).
    exit: Option<i32>,
    timed_out: bool,
    stderr: String,
}

/// Run `cmd` and hand its output to `on_line` a line at a time (each cut at
/// JOURNAL_MAX_LINE_BYTES) until it ends, `on_line` returns false or
/// `deadline` passes. Err: it could not be started.
async fn stream_lines(
    cmd: &str,
    args: &[String],
    deadline: tokio::time::Instant,
    mut on_line: impl FnMut(&[u8]) -> bool,
) -> io::Result<Streamed> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};
    let mut command = tokio::process::Command::new(cmd);
    command.args(args);
    for name in exec::AGENT_SECRET_ENV {
        command.env_remove(name);
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let stderr = child.stderr.take().expect("stderr piped");
    let errors = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stderr.take(4096).read_to_end(&mut buf).await;
        String::from_utf8_lossy(&buf).into_owned()
    });
    let mut reader = tokio::io::BufReader::with_capacity(64 * 1024, child.stdout.take().expect("stdout piped"));
    let mut line: Vec<u8> = Vec::new();
    let mut stopped = false;
    let mut timed_out = false;
    loop {
        let chunk = match tokio::time::timeout_at(deadline, reader.fill_buf()).await {
            Err(_) => {
                timed_out = true;
                break;
            }
            Ok(Err(_)) => break,
            Ok(Ok(chunk)) => chunk,
        };
        if chunk.is_empty() {
            if !line.is_empty() {
                on_line(&line);
            }
            break;
        }
        let (used, ended) = match chunk.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (chunk.len(), false),
        };
        let take = (used - usize::from(ended)).min(JOURNAL_MAX_LINE_BYTES.saturating_sub(line.len()));
        line.extend_from_slice(&chunk[..take]);
        reader.consume(used);
        if ended {
            let more = on_line(&line);
            line.clear();
            if !more {
                stopped = true;
                break;
            }
        }
    }
    let exit = if stopped || timed_out {
        let _ = child.start_kill();
        let _ = child.wait().await;
        None
    } else {
        child.wait().await.ok().map(|s| s.code().unwrap_or(-1))
    };
    let stderr = tokio::time::timeout(Duration::from_secs(1), errors)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    Ok(Streamed { exit, timed_out, stderr })
}

/// Every journal line that holds the subject, newest first, sorted by the
/// service that wrote it. `extra` selects another journal (tests).
async fn search_journal(subj: &Subject, deadline: tokio::time::Instant, extra: &[&str]) -> JournalHits {
    let mut hits = JournalHits::default();
    // With --grep journalctl itself leaves out every entry that cannot
    // match; the lines it prints are still checked here. A journalctl built
    // without pattern matching prints its newest lines instead.
    for grep in [true, false] {
        hits = JournalHits::default();
        let mut args: Vec<String> =
            ["-q", "--no-pager", "-o", "short-iso", "-r"].iter().map(|s| s.to_string()).collect();
        args.extend(extra.iter().map(|s| s.to_string()));
        if grep {
            args.push("--case-sensitive=false".into());
            args.push(format!("--grep={}", subj.journal_pattern()));
        } else {
            args.push(format!("--lines={JOURNAL_MAX_LINES}"));
        }
        let mut class = JournalClass::Other;
        let mut seen = 0u64;
        let ran = stream_lines("journalctl", &args, deadline, |line| {
            seen += 1;
            hits.take(line, subj, &mut class);
            seen < JOURNAL_MAX_LINES
        })
        .await;
        let ran = match ran {
            Ok(ran) => ran,
            Err(e) => {
                hits.problem = Some(format!("journalctl could not be started ({e})"));
                return hits;
            }
        };
        if grep && ran.exit.is_some_and(|c| c != 0) && ran.stderr.contains("pattern matching") {
            continue;
        }
        match ran.exit {
            Some(0) => {}
            // With --grep, 1 and no message is "no entry matched".
            Some(1) if ran.stderr.trim().is_empty() => {}
            Some(_) => {
                let why = ran.stderr.lines().next().unwrap_or("").replace(&subj.value, "the value");
                hits.truncated = true;
                hits.problem = Some(format!("the journal could not be read: {}", clean_sample(why.as_bytes())));
            }
            None if ran.timed_out => {
                hits.truncated = true;
                hits.problem = Some("the search stopped at its time limit before it reached the oldest entries".into());
            }
            None => {
                hits.truncated = true;
                hits.problem = Some(format!("only the newest {JOURNAL_MAX_LINES} lines were looked at"));
            }
        }
        if !grep && hits.problem.is_none() && seen >= JOURNAL_MAX_LINES {
            hits.truncated = true;
        }
        break;
    }
    hits
}

fn journal_sources(subj: &Subject, hits: JournalHits) -> Vec<SubjectSource> {
    let mut out = Vec::new();
    let mut add = |id: &str, label: &str, what: &str, bucket: Bucket| {
        let mut s = source(id, label, false, format!("{what} {JOURNAL_KEPT}"));
        s.count = bucket.count;
        s.samples = bucket.samples;
        s.truncated = hits.truncated;
        if let Some(p) = &hits.problem {
            s.note.push(' ');
            s.note.push_str(&sentence(p));
        }
        out.push(s);
    };
    let JournalHits { mail, ssh, mut other, .. } = hits;
    add("journal_mail", "Mail server log (journal)", "Postfix and Dovecot lines in the system journal.", mail);
    match subj.kind {
        Kind::Ip => add("journal_ssh", "SSH log (journal)", "SSH server lines in the system journal.", ssh),
        // An SSH line can only hold an email address by accident (a login
        // name); it is listed with the rest.
        Kind::Email => {
            other.count += ssh.count;
            other.samples.extend(ssh.samples);
            other.samples.truncate(MAX_SAMPLES);
        }
    }
    add(
        "journal_other",
        "Other services (journal)",
        "Lines of every other service in the system journal: the firewall, apps, the panel itself.",
        other,
    );
    out
}

// ---------------------------------------------------------------------------
// fail2ban
// ---------------------------------------------------------------------------

/// The jails of `fail2ban-client status` (firewall.rs reads the same line).
fn fail2ban_jails(status: &str) -> Vec<String> {
    status
        .lines()
        .find(|l| l.contains("Jail list:"))
        .and_then(|l| l.split_once(':'))
        .map(|(_, list)| {
            list.split(',')
                .map(|j| j.trim().to_string())
                .filter(|j| !j.is_empty() && j.len() <= 64 && j.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
                .collect()
        })
        .unwrap_or_default()
}

/// Whether `fail2ban-client status <jail>` lists the address as banned.
fn fail2ban_bans(status: &str, ip: IpAddr) -> bool {
    status
        .lines()
        .find_map(|l| l.split_once("Banned IP list:"))
        .is_some_and(|(_, list)| {
            list.split_whitespace().any(|t| t.parse::<IpAddr>().map(|a| a.to_canonical()) == Ok(ip))
        })
}

/// One line per jail that holds the address in fail2ban's database.
fn fail2ban_records(db: &Path, ip: &str) -> rusqlite::Result<Vec<(String, String)>> {
    use rusqlite::OpenFlags;
    let conn = rusqlite::Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_secs(2))?;
    let mut out: Vec<(String, String)> = Vec::new();
    for table in ["bips", "bans"] {
        let mut stmt = conn.prepare(&format!(
            "SELECT jail, COUNT(*), MAX(timeofban), MAX(bancount) FROM {table} WHERE ip = ?1 GROUP BY jail ORDER BY jail"
        ))?;
        let rows = stmt.query_map([ip], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?))
        })?;
        for row in rows {
            let (jail, _n, last, bancount) = row?;
            if out.iter().any(|(j, _)| *j == jail) {
                continue;
            }
            let when = chrono::DateTime::from_timestamp(last, 0)
                .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
                .unwrap_or_else(|| "an unknown time".into());
            let times = if bancount == 1 { "1 ban".to_string() } else { format!("{bancount} bans") };
            out.push((jail, format!("{times} on record, the last one on {when}")));
        }
    }
    Ok(out)
}

async fn search_fail2ban(subj: &Subject) -> SubjectSource {
    let mut s = source(
        "fail2ban",
        "fail2ban bans",
        false,
        "Bans fail2ban holds for this address. A ban is a security measure and is not lifted by an erase: \
         unban the address on the Firewall page if it should go. fail2ban drops a ban from its database by \
         itself, one day after it was set unless dbpurgeage says otherwise.",
    );
    let Some(ip) = subj.ip else { return s };
    let mut jails: Vec<(String, Vec<String>)> = Vec::new();
    let mut problems: Vec<String> = Vec::new();

    let live = tokio::time::timeout(QUICK_LIMIT, async {
        let status = exec::run("fail2ban-client", ["status"]).await?;
        let mut banned = Vec::new();
        for jail in fail2ban_jails(&status.stdout).into_iter().take(64) {
            if let Ok(detail) = exec::run("fail2ban-client", ["status", jail.as_str()]).await {
                if fail2ban_bans(&detail.stdout, ip) {
                    banned.push(jail);
                }
            }
        }
        Ok::<_, AgentError>(banned)
    })
    .await;
    match live {
        Ok(Ok(banned)) => jails.extend(banned.into_iter().map(|j| (j, vec!["banned now".to_string()]))),
        Ok(Err(_)) => problems.push("fail2ban is not running, so its current bans could not be read".into()),
        Err(_) => {
            s.truncated = true;
            problems.push("fail2ban did not answer in time, so its current bans could not be read".into());
        }
    }

    let text = subj.value.clone();
    let records = tokio::task::spawn_blocking(move || {
        let db = Path::new(FAIL2BAN_DB);
        if !db.exists() {
            return Ok(Vec::new());
        }
        fail2ban_records(db, &text)
    })
    .await;
    match records {
        Ok(Ok(rows)) => {
            for (jail, what) in rows {
                match jails.iter_mut().find(|(j, _)| *j == jail) {
                    Some((_, parts)) => parts.push(what),
                    None => jails.push((jail, vec![what])),
                }
            }
        }
        _ => problems.push("fail2ban's database could not be read".into()),
    }

    s.count = jails.len() as u64;
    s.samples = jails
        .iter()
        .take(MAX_SAMPLES)
        .map(|(jail, parts)| clean_sample(format!("{jail}: {}", parts.join("; ")).as_bytes()))
        .collect();
    for p in problems {
        s.note.push(' ');
        s.note.push_str(&sentence(&p));
    }
    s
}

// ---------------------------------------------------------------------------
// Mail: mailbox, forwarders, stored mail, the trash, the queue
// ---------------------------------------------------------------------------

/// The maildir of an address (Dovecot's `%d/%n` under /var/vmail), when the
/// address is one a mailbox here can have.
fn maildir_of(email: &str) -> Option<PathBuf> {
    let (_, local, domain) = crate::handlers::mail::mailbox_parts(email).ok()?;
    Some(PathBuf::from(format!("/var/vmail/{domain}/{local}")))
}

/// Files and bytes under `dir`, without following links. The bool is false
/// when the walk stopped at `deadline` or after `max_entries`.
fn folder_usage(dir: &Path, deadline: Instant, max_entries: u64) -> (u64, u64, bool) {
    let (mut files, mut bytes, mut seen) = (0u64, 0u64, 0u64);
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for entry in entries.flatten() {
            seen += 1;
            if seen > max_entries || (seen % 256 == 0 && Instant::now() >= deadline) {
                return (files, bytes, false);
            }
            let Ok(meta) = std::fs::symlink_metadata(entry.path()) else { continue };
            if meta.is_dir() {
                todo.push(entry.path());
            } else if meta.is_file() {
                files += 1;
                bytes += meta.len();
            }
        }
    }
    (files, bytes, true)
}

fn human_size(bytes: u64) -> String {
    const MB: u64 = 1024 * 1024;
    if bytes >= 1024 * MB {
        format!("{:.1} GB", bytes as f64 / (1024 * MB) as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else {
        format!("{} kB", bytes.div_ceil(1024))
    }
}

async fn search_mail_rows(state: &AppState, subj: &Subject) -> (SubjectSource, SubjectSource) {
    let mut boxes = source(
        "mailboxes",
        "Mailbox",
        false,
        "A mailbox with this address on this server. It is not deleted here: delete it on the Mailboxes page.",
    );
    let mut fwd = source(
        "forwarders",
        "Forwarders",
        false,
        "Forwarders that have this address as their source or as a destination. They are not removed here: \
         remove them on the Mailboxes page.",
    );
    let email = subj.value.clone();
    let rows = tokio::time::timeout(QUICK_LIMIT, async {
        let mut conn = mail_conn(state).await?;
        let mailboxes: Vec<(String, u64)> = conn
            .exec("SELECT email, quota_mb FROM virtual_users WHERE email = ? LIMIT 5", (&email,))
            .await?;
        let total: Option<u64> = conn
            .exec_first(
                "SELECT COUNT(*) FROM virtual_aliases WHERE source = ? OR destination = ?",
                (&email, &email),
            )
            .await?;
        let aliases: Vec<(String, String)> = conn
            .exec(
                "SELECT source, destination FROM virtual_aliases WHERE source = ? OR destination = ? \
                 ORDER BY id DESC LIMIT 20",
                (&email, &email),
            )
            .await?;
        Ok::<_, AgentError>((mailboxes, total.unwrap_or(0), aliases))
    })
    .await;
    match rows {
        Ok(Ok((mailboxes, total, aliases))) => {
            boxes.count = mailboxes.len() as u64;
            boxes.samples = mailboxes
                .iter()
                .map(|(email, quota)| clean_sample(format!("{email} (quota {quota} MB)").as_bytes()))
                .collect();
            fwd.count = total;
            fwd.samples = aliases
                .iter()
                .map(|(from, to)| clean_sample(format!("{from} -> {to}").as_bytes()))
                .collect();
        }
        _ => {
            let why = " The mail database could not be read (is MariaDB running?), so this was not checked.";
            for s in [&mut boxes, &mut fwd] {
                s.truncated = true;
                s.note.push_str(why);
            }
        }
    }
    (boxes, fwd)
}

fn search_maildir(subj: &Subject, deadline: Instant) -> SubjectSource {
    let mut s = source(
        "maildir",
        "Stored mail",
        false,
        "The mail stored for a mailbox with this address. It is not deleted here: deleting the mailbox on the \
         Mailboxes page moves it to Files > Trash, where it is deleted for good after 30 days or when the \
         trash is emptied. Mail in other mailboxes that was sent by or to this address is not searched.",
    );
    let Some(dir) = maildir_of(&subj.value) else { return s };
    if !std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) {
        return s;
    }
    let (files, bytes, complete) = folder_usage(&dir, deadline, 2_000_000);
    s.count = 1;
    s.truncated = !complete;
    s.samples.push(clean_sample(
        format!(
            "{}: {}{files} files, {}",
            dir.display(),
            if complete { "" } else { "more than " },
            human_size(bytes)
        )
        .as_bytes(),
    ));
    s
}

/// Trash entries that are the maildir of this address (a deleted mailbox's
/// mail is kept there, see mail.rs::delete_mailbox).
fn search_trash(trash: &Path, subj: &Subject) -> SubjectSource {
    let days = crate::handlers::files::TRASH_RETENTION_DAYS;
    let mut s = source(
        "mail_trash",
        "Mail of a deleted mailbox (Files > Trash)",
        false,
        format!(
            "The mail of a deleted mailbox with this address, kept in Files > Trash. It is deleted for good \
             {days} days after the mailbox was; delete the entry there (or empty the trash) to do it now."
        ),
    );
    let Some(maildir) = maildir_of(&subj.value) else { return s };
    let Ok(entries) = std::fs::read_dir(trash) else { return s };
    let mut found: Vec<crate::handlers::files::TrashMeta> = Vec::new();
    for entry in entries.flatten().take(100_000) {
        let Ok(body) = std::fs::read(entry.path().join("meta.json")) else { continue };
        let Ok(meta) = serde_json::from_slice::<crate::handlers::files::TrashMeta>(&body) else { continue };
        if Path::new(&meta.original_path) == maildir {
            found.push(meta);
        }
    }
    found.sort_by(|a, b| b.trashed_at.cmp(&a.trashed_at));
    s.count = found.len() as u64;
    s.samples = found
        .iter()
        .take(MAX_SAMPLES)
        .map(|m| clean_sample(format!("{} (deleted {}, trash entry {})", m.original_path, m.trashed_at, m.id).as_bytes()))
        .collect();
    s
}

/// One line of `postqueue -j`: a description of the message when the
/// address is its sender or one of its recipients.
fn queue_line(line: &[u8], email: &str) -> Option<String> {
    let msg: serde_json::Value = serde_json::from_slice(line).ok()?;
    let is = |v: Option<&serde_json::Value>| v.and_then(|a| a.as_str()).is_some_and(|a| a.eq_ignore_ascii_case(email));
    let recipients: Vec<&serde_json::Value> =
        msg.get("recipients").and_then(|r| r.as_array()).map(|r| r.iter().collect()).unwrap_or_default();
    let to_it = recipients.iter().any(|r| is(r.get("address")) || is(r.get("orig_address")));
    if !is(msg.get("sender")) && !to_it {
        return None;
    }
    let text = |key: &str| msg.get(key).and_then(|v| v.as_str()).unwrap_or("?").to_string();
    let to: Vec<&str> = recipients.iter().filter_map(|r| r.get("address")?.as_str()).take(5).collect();
    let arrived = msg
        .get("arrival_time")
        .and_then(|t| t.as_i64())
        .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
        .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "?".into());
    Some(format!(
        "{} {}: from {} to {}, arrived {arrived}",
        text("queue_name"),
        text("queue_id"),
        text("sender"),
        to.join(", ")
    ))
}

async fn search_mail_queue(subj: &Subject) -> SubjectSource {
    let mut s = source(
        "mail_queue",
        "Mail queue",
        false,
        "Messages from or to this address that Postfix has not delivered yet. They leave the queue when they \
         are delivered or returned, after five days at the latest; nothing is removed from it here.",
    );
    let deadline = tokio::time::Instant::now() + QUICK_LIMIT;
    let mut seen = 0u64;
    let (mut count, mut samples) = (0u64, Vec::new());
    let ran = stream_lines("postqueue", &["-j".to_string()], deadline, |line| {
        seen += 1;
        if let Some(what) = queue_line(line, &subj.value) {
            count += 1;
            if samples.len() < MAX_SAMPLES {
                samples.push(clean_sample(what.as_bytes()));
            }
        }
        seen < JOURNAL_MAX_LINES
    })
    .await;
    s.count = count;
    s.samples = samples;
    match ran {
        Ok(ran) if ran.exit == Some(0) => {}
        Ok(ran) if ran.exit.is_none() => {
            s.truncated = true;
            s.note.push_str(" The queue is too long to read in one go, so there may be more.");
        }
        _ => s.note.push_str(" The queue could not be read (is Postfix installed?)."),
    }
    s
}

// ---------------------------------------------------------------------------
// Task logs (the `tasks` table of panel.db, which the agent writes)
// ---------------------------------------------------------------------------

fn task_logs_source(subj: &Subject) -> SubjectSource {
    source(
        "task_logs",
        "Task logs",
        true,
        format!(
            "Lines in the logs of the panel's tasks (deploys, installs, certificates). Erase replaces the {} \
             with {} in the logs of finished tasks. A task and its log are deleted after {} days.",
            match subj.kind {
                Kind::Email => "address",
                Kind::Ip => "IP address",
            },
            subj.replacement,
            crate::ipc::tasks::TASK_RETENTION_DAYS
        ),
    )
}

/// A connection of its own, so a long read does not hold the one the task
/// tracker writes through.
fn open_panel_db(path: &Path) -> rusqlite::Result<rusqlite::Connection> {
    use rusqlite::OpenFlags;
    let conn = rusqlite::Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_secs(5))?;
    Ok(conn)
}

fn search_tasks(db: &Path, subj: &Subject, deadline: Instant) -> SubjectSource {
    let mut s = task_logs_source(subj);
    let mut read = || -> rusqlite::Result<bool> {
        let conn = open_panel_db(db)?;
        let mut stmt = conn.prepare("SELECT kind, started_at, log_blob FROM tasks ORDER BY started_at DESC")?;
        let mut rows = stmt.query([])?;
        let mut bytes = 0u64;
        while let Some(row) = rows.next()? {
            let (kind, started): (String, String) = (row.get(0)?, row.get(1)?);
            let log = row.get_ref(2)?.as_bytes().unwrap_or_default();
            bytes += log.len() as u64;
            let mut lines: Vec<&[u8]> = Vec::new();
            subj.matching_lines(log, |line| lines.push(line));
            s.count += lines.len() as u64;
            for line in lines.iter().rev() {
                if s.samples.len() < MAX_SAMPLES {
                    let mut text = format!("{kind} ({started}): ").into_bytes();
                    text.extend_from_slice(line);
                    s.samples.push(clean_sample(&text));
                }
            }
            if bytes >= TASKS_MAX_BYTES || Instant::now() >= deadline {
                return Ok(false);
            }
        }
        Ok(true)
    };
    match read() {
        Ok(true) => {}
        Ok(false) => {
            s.truncated = true;
            s.note.push_str(" The search stopped at its limit before it reached the oldest tasks.");
        }
        Err(_) => {
            s.truncated = true;
            s.note.push_str(" The task logs could not be read.");
        }
    }
    s
}

fn erase_tasks(db: &Path, subj: &Subject, deadline: Instant) -> EraseOutcome {
    let mut changed = 0u64;
    let mut running = 0u64;
    let mut complete = true;
    let work = |changed: &mut u64, running: &mut u64, complete: &mut bool| -> rusqlite::Result<()> {
        let conn = open_panel_db(db)?;
        let ids: Vec<String> = conn
            .prepare("SELECT id FROM tasks ORDER BY started_at DESC")?
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for id in ids {
            if Instant::now() >= deadline {
                *complete = false;
                break;
            }
            let row: Option<(String, String)> = conn
                .query_row("SELECT status, log_blob FROM tasks WHERE id = ?1", [&id], |r| Ok((r.get(0)?, r.get(1)?)))
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    e => Err(e),
                })?;
            let Some((status, log)) = row else { continue };
            let mut fixed = Vec::with_capacity(log.len());
            let lines = subj.rewrite(log.as_bytes(), &mut fixed);
            if lines == 0 {
                continue;
            }
            // A running task writes its whole log again every two seconds.
            if status == "pending" || status == "running" {
                *running += 1;
                continue;
            }
            // The replacement is ASCII put where ASCII was.
            let Ok(fixed) = String::from_utf8(fixed) else { continue };
            // Only if the log is still what was read (a self-update's row
            // gets a last line appended after it has ended).
            let n = conn.execute(
                "UPDATE tasks SET log_blob = ?1 WHERE id = ?2 AND log_blob = ?3",
                rusqlite::params![fixed, id, log],
            )?;
            if n == 1 {
                *changed += lines;
            }
        }
        Ok(())
    };
    let result = work(&mut changed, &mut running, &mut complete);
    let mut detail = if changed == 0 {
        "No finished task's log holds it.".to_string()
    } else {
        format!(
            "Replaced with {} in {changed} {} of finished tasks.",
            subj.replacement,
            if changed == 1 { "line" } else { "lines" }
        )
    };
    if running > 0 {
        detail.push_str(&format!(
            " {running} running {} it and {} left alone; run the erase again when {} finished.",
            if running == 1 { "task holds" } else { "tasks hold" },
            if running == 1 { "was" } else { "were" },
            if running == 1 { "it has" } else { "they have" },
        ));
    }
    if !complete {
        detail.push_str(" Stopped at the time limit; run the erase again to go on.");
    }
    if result.is_err() {
        detail.push_str(" The task logs could not be read or written to the end.");
    }
    EraseOutcome {
        id: "task_logs".into(),
        ok: result.is_ok() && complete && running == 0,
        changed,
        detail,
    }
}

// ---------------------------------------------------------------------------
// Putting a report together
// ---------------------------------------------------------------------------

/// Whether the panel is set to mask addresses in new log lines
/// (`privacy.log_ip`, which the privacy module applies to nginx).
fn masking_is_on(state: &AppState) -> bool {
    state
        .panel_db
        .with(|conn| {
            conn.query_row("SELECT value FROM settings WHERE key = 'privacy.log_ip'", [], |r| r.get::<_, String>(0))
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    e => Err(e),
                })
        })
        .ok()
        .flatten()
        .is_some_and(|v| v.trim() == "masked")
}

fn replaces(subj: &Subject) -> String {
    match subj.kind {
        Kind::Email => format!("Erase replaces the address with {} and keeps the rest of each line.", subj.replacement),
        Kind::Ip => format!(
            "Erase replaces the address with its masked form, {}, and keeps the rest of each line.",
            subj.replacement
        ),
    }
}

fn file_source(mut s: SubjectSource, dir: Option<&Path>, listed: io::Result<Vec<LogFile>>, hits: FileHits) -> SubjectSource {
    s.count = hits.count;
    s.samples = hits.samples;
    s.truncated = hits.truncated;
    if let (Some(dir), Err(e)) = (dir, &listed) {
        s.note.push_str(&if e.kind() == io::ErrorKind::NotFound {
            format!(" There is no {} on this server.", dir.display())
        } else {
            s.truncated = true;
            format!(" {} could not be read: {e}.", dir.display())
        });
    }
    if !hits.problems.is_empty() {
        s.note.push_str(" Not everything was read:");
        for p in hits.problems.iter().take(8) {
            s.note.push(' ');
            s.note.push_str(&sentence(p));
        }
    }
    s
}

/// The two nginx sources for the logs in `dir`.
fn search_nginx(dir: &Path, subj: &Subject, masking: bool, deadline: Instant) -> Vec<SubjectSource> {
    let mut out = Vec::new();
    for errors in [false, true] {
        let mut s = if errors {
            source(
                "nginx_error",
                "Web server error log",
                true,
                format!(
                    "Lines in nginx's error logs in {} (the current one and the rotated ones) that hold it. {}",
                    dir.display(),
                    replaces(subj)
                ),
            )
        } else {
            let what = match subj.kind {
                Kind::Ip => "requests from this address, and lines that hold it anywhere else",
                Kind::Email => "lines whose address, referrer or any other field holds it",
            };
            source(
                "nginx_access",
                "Web server access log",
                true,
                format!(
                    "Lines in nginx's access logs in {} (the current ones and the rotated ones): {what}. {}",
                    dir.display(),
                    replaces(subj)
                ),
            )
        };
        if masking && subj.kind == Kind::Ip && !subj.is_its_own_replacement() {
            s.note.push_str(&format!(
                " The panel is set to mask addresses in new log lines, which then show {} for this visitor; \
                 only lines written before that can be found under the full address.",
                subj.replacement
            ));
        }
        let listed = nginx_logs(dir, errors);
        let hits = match &listed {
            Ok(files) => search_files(files, subj, deadline, SEARCH_MAX_BYTES),
            Err(_) => FileHits::default(),
        };
        out.push(file_source(s, Some(dir), listed, hits));
    }
    out
}

fn search_system_logs(subj: &Subject, deadline: Instant) -> SubjectSource {
    let s = source(
        "system_logs",
        "Other log files in /var/log",
        false,
        "Lines in the text logs next to the journal: syslog, auth.log, mail.log, kern.log and ufw.log (copies \
         rsyslog keeps of journal lines), fail2ban.log, and the logs of Rspamd, MariaDB and PostgreSQL. They \
         are not rewritten here: logrotate deletes them on its schedule, after four weeks unless that was \
         changed.",
    );
    let files = system_logs();
    let hits = search_files(&files, subj, deadline, SEARCH_MAX_BYTES);
    file_source(s, None, Ok(files), hits)
}

fn unread(id: &str, label: &str, erasable: bool) -> SubjectSource {
    let mut s = source(id, label, erasable, "This could not be searched: the agent failed while reading it.");
    s.truncated = true;
    s
}

/// POST /privacy/subject/search
pub async fn search(
    State(state): State<AppState>,
    Json(req): Json<SubjectReq>,
) -> AgentResult<Json<SubjectReport>> {
    let subj = Subject::parse(&req.kind, &req.value)?;
    // Each source stops SEARCH_SOURCE_LIMIT after it started, and all of
    // them at `overall` (a source can wait for a free thread first).
    let overall = Instant::now() + SEARCH_TOTAL_LIMIT;
    let limit = move |each: Duration| overall.min(Instant::now() + each);
    let masking = masking_is_on(&state);

    let blocking = |f: Box<dyn FnOnce() -> Vec<SubjectSource> + Send>| tokio::task::spawn_blocking(f);
    let nginx = {
        let subj = subj.clone();
        blocking(Box::new(move || search_nginx(Path::new(NGINX_LOG_DIR), &subj, masking, limit(SEARCH_SOURCE_LIMIT))))
    };
    let system = {
        let subj = subj.clone();
        blocking(Box::new(move || vec![search_system_logs(&subj, limit(SEARCH_SOURCE_LIMIT))]))
    };
    let tasks = {
        let (subj, db) = (subj.clone(), state.config.panel_db_path.clone());
        blocking(Box::new(move || vec![search_tasks(&db, &subj, limit(SEARCH_SOURCE_LIMIT))]))
    };
    let on_disk = {
        let subj = subj.clone();
        blocking(Box::new(move || match subj.kind {
            Kind::Email => vec![search_maildir(&subj, limit(QUICK_LIMIT)), search_trash(Path::new(TRASH_DIR), &subj)],
            Kind::Ip => Vec::new(),
        }))
    };
    let journal = search_journal(&subj, tokio::time::Instant::from_std(limit(SEARCH_SOURCE_LIMIT)), &[]);
    let by_kind = async {
        match subj.kind {
            Kind::Ip => vec![search_fail2ban(&subj).await],
            Kind::Email => {
                let ((boxes, fwd), queue) = tokio::join!(search_mail_rows(&state, &subj), search_mail_queue(&subj));
                vec![boxes, fwd, queue]
            }
        }
    };
    let (nginx, system, tasks, on_disk, journal, by_kind) =
        tokio::join!(nginx, system, tasks, on_disk, journal, by_kind);

    let mut sources = nginx.unwrap_or_else(|_| {
        vec![
            unread("nginx_access", "Web server access log", true),
            unread("nginx_error", "Web server error log", true),
        ]
    });
    sources.extend(journal_sources(&subj, journal));
    sources.extend(system.unwrap_or_else(|_| vec![unread("system_logs", "Other log files in /var/log", false)]));
    let mut by_kind = by_kind.into_iter();
    match subj.kind {
        Kind::Ip => sources.extend(by_kind),
        Kind::Email => {
            // mailboxes, forwarders, then what is on disk, then the queue.
            sources.extend(by_kind.by_ref().take(2));
            sources.extend(on_disk.unwrap_or_else(|_| {
                vec![
                    unread("maildir", "Stored mail", false),
                    unread("mail_trash", "Mail of a deleted mailbox (Files > Trash)", false),
                ]
            }));
            sources.extend(by_kind);
        }
    }
    sources.extend(tasks.unwrap_or_else(|_| vec![unread("task_logs", "Task logs", true)]));
    Ok(Json(SubjectReport { kind: subj.kind_str(), value: subj.value.clone(), sources }))
}

/// POST /privacy/subject/erase
pub async fn erase(
    State(state): State<AppState>,
    Json(req): Json<EraseReq>,
) -> AgentResult<Json<SubjectEraseResult>> {
    let subj = Subject::parse(&req.kind, &req.value)?;
    if req.sources.len() > 64 {
        return Err(AgentError::Invalid("too many sources".into()));
    }
    let mut ids: Vec<String> = Vec::new();
    for id in &req.sources {
        if !ids.contains(id) {
            ids.push(id.clone());
        }
    }
    let db = state.config.panel_db_path.clone();
    let work = move || -> Option<Vec<EraseOutcome>> {
        let _one_at_a_time = match ERASE_RUNNING.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        };
        Some(ids.iter().map(|id| erase_source(id, &subj, Path::new(NGINX_LOG_DIR), &db)).collect())
    };
    let results = tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| AgentError::Other(anyhow::anyhow!("erase task: {e}")))?
        .ok_or_else(|| AgentError::Conflict("Another erase is still running. Try again when it has finished.".into()))?;
    // Without the value: this line goes to the journal.
    tracing::info!(
        kind = %req.kind,
        sources = results.len(),
        changed = results.iter().map(|r| r.changed).sum::<u64>(),
        "privacy: subject erase"
    );
    Ok(Json(SubjectEraseResult { results }))
}

fn erase_source(id: &str, subj: &Subject, nginx_dir: &Path, panel_db: &Path) -> EraseOutcome {
    let refuse = |detail: String| EraseOutcome { id: clean_sample(id.as_bytes()), ok: false, changed: 0, detail };
    match SOURCES.iter().find(|(known, _)| *known == id) {
        None => return refuse("This agent has no source with that name.".into()),
        Some((_, false)) => {
            return refuse("Nothing is erased from this source here; its note in the search result says what happens to it.".into())
        }
        Some((_, true)) => {}
    }
    if subj.is_its_own_replacement() {
        return EraseOutcome {
            id: id.into(),
            ok: true,
            changed: 0,
            detail: format!("{} is already a masked address; there is nothing to replace it with.", subj.value),
        };
    }
    match id {
        "nginx_access" | "nginx_error" => match nginx_logs(nginx_dir, id == "nginx_error") {
            Ok(files) => {
                let done = erase_in_files(nginx_dir, &files, subj, &EraseLimits::starting_now());
                erased_files_outcome(id, subj, done)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => EraseOutcome {
                id: id.into(),
                ok: true,
                changed: 0,
                detail: format!("There is no {} on this server.", nginx_dir.display()),
            },
            Err(e) => refuse(format!("{} could not be read: {e}.", nginx_dir.display())),
        },
        _ => erase_tasks(panel_db, subj, Instant::now() + ERASE_SOFT_LIMIT),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn email(v: &str) -> Subject {
        Subject::parse("email", v).unwrap()
    }

    fn ip(v: &str) -> Subject {
        Subject::parse("ip", v).unwrap()
    }

    fn found(s: &Subject, hay: &str) -> bool {
        s.next_match(hay.as_bytes(), 0).is_some()
    }

    fn erased(s: &Subject, hay: &str) -> (String, u64) {
        let mut out = Vec::new();
        let lines = s.rewrite(hay.as_bytes(), &mut out);
        (String::from_utf8(out).unwrap(), lines)
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("privacy-subject-{name}-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        dir
    }

    fn gzip(path: &Path, content: &[u8]) {
        let mut child = std::process::Command::new("gzip")
            .arg("-c")
            .stdin(Stdio::piped())
            .stdout(Stdio::from(std::fs::File::create(path).unwrap()))
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(content).unwrap();
        assert!(child.wait().unwrap().success());
    }

    fn gunzip(path: &Path) -> Vec<u8> {
        let out = std::process::Command::new("gzip").arg("-dc").arg(path).output().unwrap();
        assert!(out.status.success(), "{} is not valid gzip", path.display());
        out.stdout
    }

    fn set_mtime(path: &Path, secs_ago: u64) -> SystemTime {
        let t = SystemTime::UNIX_EPOCH
            + Duration::from_secs(
                SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs() - secs_ago,
            );
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_times(std::fs::FileTimes::new().set_modified(t).set_accessed(t)).unwrap();
        t
    }

    fn far() -> Instant {
        Instant::now() + Duration::from_secs(120)
    }

    const NO_DB: &str = "/nonexistent/panel.db";

    // --- validation ---------------------------------------------------------

    #[test]
    fn one_email_address_is_accepted_and_lowercased() {
        assert_eq!(email("jane@example.com").value, "jane@example.com");
        assert_eq!(email("  Jane.Doe+News@Mail.Example.CO.uk \n").value, "jane.doe+news@mail.example.co.uk");
        assert_eq!(email("o'brien@example.ie").value, "o'brien@example.ie");
        assert_eq!(email("first_last-2@xn--bcher-kva.example").value, "first_last-2@xn--bcher-kva.example");
        let local64 = "a".repeat(64);
        assert!(Subject::parse("email", &format!("{local64}@example.com")).is_ok());
    }

    #[test]
    fn anything_that_is_not_one_plain_email_address_is_refused() {
        let local65 = format!("{}@example.com", "a".repeat(65));
        let long = format!("a@{}.example.com", "b".repeat(250));
        for bad in [
            "", " ", "jane", "jane@", "@example.com", "jane@example", "jane@localhost",
            "jane doe@example.com", "jane@exa mple.com", "jane@example.com, joe@example.com",
            "jane@example.com joe@example.com", "Jane <jane@example.com>", "\"jane\"@example.com",
            "jane@example.com\nbcc: x@example.com", "jane@[203.0.113.5]", "jäne@example.com",
            "jane@exämple.com", ".jane@example.com", "jane.@example.com", "ja..ne@example.com",
            "jane@-example.com", "jane@example..com", "jane@example.com.", "jane@@example.com",
            "jane@example.com;id", "$(id)@example.com", "`id`@example.com", "a|b@example.com",
            "jane\\@example.com", "jane@example.com/../x", "jane@example.com\0", "*", ".*@.*",
            local65.as_str(), long.as_str(),
        ] {
            match Subject::parse("email", bad) {
                Err(AgentError::Invalid(_)) => {}
                Err(e) => panic!("{bad:?}: wrong error {e}"),
                Ok(s) => panic!("{bad:?} was accepted as {}", s.value),
            }
        }
    }

    #[test]
    fn one_ip_address_is_accepted_and_normalised() {
        assert_eq!(ip("203.0.113.5").value, "203.0.113.5");
        assert_eq!(ip(" 203.0.113.5\n").value, "203.0.113.5");
        assert_eq!(ip("2001:DB8::1").value, "2001:db8::1");
        assert_eq!(ip("2001:0db8:0000:0000:0000:0000:0000:0001").value, "2001:db8::1");
        assert_eq!(ip("2001:db8:0:0:0:0:0:1").value, "2001:db8::1");
        // An IPv4 address in IPv6 clothes is searched as the IPv4 address.
        assert_eq!(ip("::ffff:203.0.113.5").value, "203.0.113.5");
        assert_eq!(ip("::FFFF:CB00:7105").value, "203.0.113.5");
        assert_eq!(ip("127.0.0.1").value, "127.0.0.1");
    }

    #[test]
    fn anything_that_is_not_one_ip_address_is_refused() {
        for bad in [
            "", " ", "203.0.113", "203.0.113.256", "203.0.113.5.6", "203.0.113.05", "0203.0.113.5",
            "203.0.113.5/24", "203.0.113.5:443", "203.0.113.5 198.51.100.7", "203.0.113.5,198.51.100.7",
            "203.0.113.5-203.0.113.9", "203.0.113.*", "[2001:db8::1]", "2001:db8::1/64", "fe80::1%eth0",
            "2001:db8::1::2", "2001:db8:::1", "2001:db8::g", "example.com", "localhost", "0.0.0.0", "::",
            "::ffff:0.0.0.0", "1.2.3.4;id", "1.2.3.4\n5.6.7.8", "$(id)", ".*", "0x7f.0.0.1", "2130706433",
            "1.2.3.4 ; rm -rf /", "١.٢.٣.٤",
        ] {
            match Subject::parse("ip", bad) {
                Err(AgentError::Invalid(_)) => {}
                Err(e) => panic!("{bad:?}: wrong error {e}"),
                Ok(s) => panic!("{bad:?} was accepted as {}", s.value),
            }
        }
        assert!(matches!(Subject::parse("phone", "203.0.113.5"), Err(AgentError::Invalid(_))));
        assert!(matches!(Subject::parse("", "jane@example.com"), Err(AgentError::Invalid(_))));
        // The kind decides: an address of the other kind is not guessed at.
        assert!(Subject::parse("ip", "jane@example.com").is_err());
        assert!(Subject::parse("email", "203.0.113.5").is_err());
    }

    #[test]
    fn the_masked_form_is_the_one_of_the_contract() {
        assert_eq!(ip("203.0.113.87").replacement, "203.0.113.0");
        assert_eq!(ip("2001:db8:85a3:8d3::7").replacement, "2001:db8:85a3::");
        // Parsed, the last five groups zeroed, printed in the shortest form
        // (as the panel does it), not cut out of the text.
        assert_eq!(ip("2001:0:0:1::5").replacement, "2001::");
        assert_eq!(ip("2001:0db8:85A3:08d3:0000:0000:0000:0007").replacement, "2001:db8:85a3::");
        assert_eq!(ip("2001:db8::1").replacement, "2001:db8::");
        assert_eq!(ip("fe80::1").replacement, "fe80::");
        assert_eq!(ip("::1").replacement, "::");
        assert_eq!(ip("10.1.2.3").replacement, "10.1.2.0");
        assert_eq!(ip("::ffff:203.0.113.87").replacement, "203.0.113.0");
        assert_eq!(email("jane@example.com").replacement, "[erased]");
        assert!(ip("203.0.113.0").is_its_own_replacement());
        assert!(ip("2001:db8:85a3::").is_its_own_replacement());
        assert!(!ip("203.0.113.87").is_its_own_replacement());
    }

    // --- exact matching -----------------------------------------------------

    #[test]
    fn an_ipv4_address_is_found_where_it_stands_as_an_address() {
        let s = ip("203.0.113.5");
        for line in [
            "203.0.113.5",
            "203.0.113.5 - - [24/Nov/2026:10:00:00 +0000] \"GET / HTTP/1.1\" 200 5",
            "2026/11/24 10:00:00 [error] 7#7: *1 open() failed, client: 203.0.113.5, server: x",
            "postfix/smtpd[1]: connect from unknown[203.0.113.5]",
            "dovecot: imap-login: Login: user=<a@b.example>, rip=203.0.113.5, lip=198.51.100.1",
            "sshd[9]: Failed password for root from 203.0.113.5 port 51234 ssh2",
            "[UFW BLOCK] IN=eth0 SRC=203.0.113.5 DST=198.51.100.1 LEN=40",
            "::ffff:203.0.113.5 - - [24/Nov/2026]",
            "upstream: \"http://203.0.113.5:8080/x\"",
            "fail2ban.actions: NOTICE [sshd] Ban 203.0.113.5",
            "the client was 203.0.113.5.",
            "reverse name 203.0.113.5.static.example.net",
            "a,203.0.113.5,b",
            "tab\t203.0.113.5\tend",
            "x 198.51.100.7 and 203.0.113.5",
        ] {
            assert!(found(&s, line), "not found in {line:?}");
        }
    }

    #[test]
    fn an_ipv4_address_is_not_found_inside_a_longer_one() {
        let s = ip("1.2.3.4");
        for line in [
            "1.2.3.45 - -", "1.2.3.40", "11.2.3.4 - -", "21.2.3.4", "1.2.3.4.5", "0.1.2.3.4", "5.1.2.3.4",
            "1.2.3", "1.2.3.", "1.2.3.5", "1.2.34", "1a2b3c4", "1-2-3-4", "1.2.3.4x", "v1.2.3.4",
            "Chrome/1.2.3.4.5", "lib1.2.3.4", "1.2.3.4a", "x1.2.3.4", "1.2.3.44.55", "111.2.3.4",
            "1.2.3.4.5.6", "10.1.2.3.40", "1.2.3.004", "1.22.3.4", "1,2,3,4", "::ffff:1.2.3.40",
            "::ffff:11.2.3.4", "",
        ] {
            assert!(!found(&s, line), "{line:?}");
        }
        // Every one of these is another address; the dot is not "any character".
        let s = ip("203.0.113.5");
        for line in ["203.0.113.55", "203.0.113.50", "1203.0.113.5", "203.0.113.5.7", "203a0b113c5", "20300113050"] {
            assert!(!found(&s, line), "{line:?}");
        }
        // A rejected hit does not hide a real one later in the line.
        assert!(found(&ip("1.2.3.4"), "11.2.3.4 then 1.2.3.45 then 1.2.3.4"));
        assert_eq!(erased(&ip("1.2.3.4"), "11.2.3.4 1.2.3.45 1.2.3.4\n").0, "11.2.3.4 1.2.3.45 1.2.3.0\n");
    }

    #[test]
    fn an_ipv6_address_is_found_in_every_spelling() {
        let s = ip("2001:db8::1");
        for line in [
            "2001:db8::1",
            "2001:db8::1 - - [24/Nov/2026:10:00:00 +0000] \"GET / HTTP/1.1\" 200 5",
            "connect from unknown[2001:db8::1]",
            "upstream \"http://[2001:db8::1]:8080/\"",
            "rip=2001:db8::1, lip=2001:db8::2",
            "Failed password for root from 2001:db8::1 port 51234 ssh2",
            "client: 2001:db8::1, server: x",
            "[UFW BLOCK] SRC=2001:0db8:0000:0000:0000:0000:0000:0001 DST=2001:0db8:0000:0000:0000:0000:0000:0002",
            "2001:DB8::1",
            "2001:db8:0:0:0:0:0:1",
            "2001:db8:0::1",
            "2001:0DB8::0001",
            "it was 2001:db8::1.",
            "from 2001:db8::1: connection refused",
            "addr:2001:db8::1 ok",
            "fe80::1%eth0 and 2001:db8::1%eth0",
            "10:00:01 2001:db8::1",
        ] {
            assert!(found(&s, line), "not found in {line:?}");
        }
        assert!(found(&ip("2001:db8:85a3::"), "prefix 2001:db8:85a3:: seen"));
        assert!(found(&ip("::1"), "client: ::1, server"));
        // An address with an IPv4 tail, as inet_ntop prints some of them.
        assert!(found(&ip("64:ff9b::cb00:7105"), "from 64:ff9b::203.0.113.5 port 1"));
    }

    #[test]
    fn an_ipv6_address_is_not_found_inside_another_one() {
        let s = ip("2001:db8::1");
        for line in [
            "2001:db8::12", "2001:db8::10", "2001:db8::1:2", "12001:db8::1", "1:2001:db8::1", "a2001:db8::1",
            "2001:db8:1::1", "2001:db8::", "2001:db8::2", "2001:db8::1a", "2001:db8::1:0", "2001:db9::1",
            "2001:db8:0:0:0:0:1:1", "::2001:db8:0:1", "10:00:01", "aa:bb:cc:dd:ee:ff", "::ffff:1.2.3.4",
            "2001:db8", "2001:db8:", "db8::1", "",
        ] {
            assert!(!found(&s, line), "{line:?}");
        }
        assert!(!found(&ip("::1"), "2001:db8::1"));
        assert!(!found(&ip("::1"), "10:00:01"));
    }

    /// nginx on a dual-stack socket logs an IPv4 visitor as `::ffff:a.b.c.d`.
    #[test]
    fn an_ipv4_mapped_address_is_the_ipv4_address() {
        let s = ip("203.0.113.87");
        let line = "::ffff:203.0.113.87 - - [24/Nov/2026:10:00:00 +0000] \"GET / HTTP/1.1\" 200 5\n";
        assert!(found(&s, line));
        assert_eq!(erased(&s, line).0, "::ffff:203.0.113.0 - - [24/Nov/2026:10:00:00 +0000] \"GET / HTTP/1.1\" 200 5\n");
        // Asked for in its IPv6 form, it is the same subject.
        let mapped = ip("::ffff:203.0.113.87");
        assert_eq!(mapped.value, "203.0.113.87");
        assert!(found(&mapped, line) && found(&mapped, "203.0.113.87 - -"));
        assert!(!found(&mapped, "::ffff:203.0.113.8 - -") && !found(&mapped, "::ffff:203.0.113.870 - -"));
    }

    #[test]
    fn an_email_address_is_found_plain_and_percent_encoded() {
        let s = email("jane@example.com");
        for line in [
            "jane@example.com",
            "postfix/smtp[1]: 4AB12C3D4E: to=<jane@example.com>, relay=x, status=sent",
            "postfix/smtpd[1]: 5BC: client=unknown[203.0.113.7], sasl_username=jane@example.com",
            "dovecot: lmtp(jane@example.com)<12><abc>: saved mail to INBOX",
            "dovecot: imap-login: Login: user=<JANE@Example.COM>, method=PLAIN",
            "GET /unsubscribe?email=jane@example.com HTTP/1.1",
            "GET /unsubscribe?email=jane%40example.com&x=1 HTTP/1.1",
            "GET /unsubscribe?email=Jane%40Example.com HTTP/1.1",
            "\"https://shop.example/reset?to=jane%40example.com\" \"Mozilla\"",
            "GET /search?q=to%3Ajane%40example.com HTTP/1.1",
            "GET /u/jane@example.com/profile",
            "mail for jane@example.com.",
            "a,jane@example.com;b",
        ] {
            assert!(found(&s, line), "not found in {line:?}");
        }
        let plus = email("jane+news@example.com");
        assert!(found(&plus, "to=<jane+news@example.com>"));
        assert!(found(&plus, "?e=jane%2Bnews%40example.com&"));
        assert!(found(&plus, "?e=jane%2bnews%40example.com&"));
        assert!(found(&plus, "?e=jane+news%40example.com&"));
        assert!(!found(&plus, "to=<jane@example.com>"));
    }

    #[test]
    fn an_email_address_is_not_found_inside_another_one() {
        let s = email("jane@example.com");
        for line in [
            "maryjane@example.com", "mary.jane@example.com", "mary-jane@example.com", "mary_jane@example.com",
            "mary+jane@example.com", "jane@example.com.au", "jane@example.community", "jane@example.co",
            "jane@example-com", "jane@examplexcom", "janexexample.com", "jane@example.com-x", "jane@example.comx",
            "x=maryjane%40example.com", "x=mary%2Ejane%40example.com", "x=mary%2Bjane%40example.com",
            "jane@sub.example.com", "jane@example", "jane%example.com", "",
        ] {
            assert!(!found(&s, line), "{line:?}");
        }
        // The dots of the address are dots.
        assert!(!found(&email("j.ne@example.com"), "jane@example.com"));
        assert!(!found(&email("jane@example.com"), "jane@exampleXcom"));
        assert!(found(&s, "maryjane@example.com and jane@example.com"));
    }

    #[test]
    fn erase_replaces_the_subject_and_only_the_subject() {
        let s = ip("203.0.113.87");
        let log = "203.0.113.87 - - [t] \"GET / HTTP/1.1\" 200 5 \"http://203.0.113.87/\" \"UA\"\n\
                   203.0.113.8 - - [t] \"GET /a HTTP/1.1\" 200 5\n\
                   198.51.100.7 - - [t] \"GET /?from=203.0.113.87 HTTP/1.1\" 200 5\n\
                   ::ffff:203.0.113.87 - - [t] \"GET /b HTTP/1.1\" 200 5\n\
                   203.0.113.870 - - [t]\n";
        let (out, lines) = erased(&s, log);
        assert_eq!(
            out,
            "203.0.113.0 - - [t] \"GET / HTTP/1.1\" 200 5 \"http://203.0.113.0/\" \"UA\"\n\
             203.0.113.8 - - [t] \"GET /a HTTP/1.1\" 200 5\n\
             198.51.100.7 - - [t] \"GET /?from=203.0.113.0 HTTP/1.1\" 200 5\n\
             ::ffff:203.0.113.0 - - [t] \"GET /b HTTP/1.1\" 200 5\n\
             203.0.113.870 - - [t]\n"
        );
        // Lines, not occurrences.
        assert_eq!(lines, 3);
        assert_eq!(out.lines().count(), log.lines().count());
        // Twice is harmless.
        assert_eq!(erased(&s, &out), (out.clone(), 0));

        let s = ip("2001:db8:85a3:8d3::7");
        let (out, lines) = erased(
            &s,
            "2001:db8:85a3:8d3::7 - -\n[2001:0db8:85a3:08d3:0000:0000:0000:0007]:443\n2001:db8:85a3:8d3::70 - -\n",
        );
        assert_eq!(out, "2001:db8:85a3:: - -\n[2001:db8:85a3::]:443\n2001:db8:85a3:8d3::70 - -\n");
        assert_eq!(lines, 2);
        assert_eq!(erased(&s, &out), (out.clone(), 0));

        let s = email("jane@example.com");
        let (out, lines) = erased(
            &s,
            "1.2.3.4 - - \"GET /u?email=jane%40example.com&n=1 HTTP/1.1\" 200 1 \"https://x.example/?m=Jane@Example.com\"\n\
             1.2.3.4 - - \"GET /u?email=maryjane%40example.com HTTP/1.1\" 200 1\n",
        );
        assert_eq!(
            out,
            "1.2.3.4 - - \"GET /u?email=[erased]&n=1 HTTP/1.1\" 200 1 \"https://x.example/?m=[erased]\"\n\
             1.2.3.4 - - \"GET /u?email=maryjane%40example.com HTTP/1.1\" 200 1\n"
        );
        assert_eq!(lines, 1);
        assert_eq!(erased(&s, &out), (out.clone(), 0));
        // Bytes that are not UTF-8 pass through untouched.
        let raw = b"\xff\xfe 203.0.113.87 \xc3\x28\n".to_vec();
        let mut out = Vec::new();
        assert_eq!(ip("203.0.113.87").rewrite(&raw, &mut out), 1);
        assert_eq!(out, b"\xff\xfe 203.0.113.0 \xc3\x28\n");
    }

    #[test]
    fn matching_lines_are_counted_once_each() {
        let s = ip("203.0.113.87");
        let part = b"a 203.0.113.87 b 203.0.113.87\nnothing\n203.0.113.87\nlast 203.0.113.87";
        let mut lines: Vec<&[u8]> = Vec::new();
        s.matching_lines(part, |l| lines.push(l));
        assert_eq!(lines, vec![&b"a 203.0.113.87 b 203.0.113.87"[..], b"203.0.113.87", b"last 203.0.113.87"]);
    }

    // --- escaping -----------------------------------------------------------

    #[test]
    fn the_journal_pattern_is_escaped() {
        assert_eq!(pcre_escape("jane.doe+x@example.com"), r"jane\.doe\+x\@example\.com");
        assert_eq!(pcre_escape(r"a|b(c)*[d]{2}\E$^?.-"), r"a\|b\(c\)\*\[d\]\{2\}\\E\$\^\?\.\-");
        assert_eq!(pcre_escape("abcXYZ019"), "abcXYZ019");
        assert_eq!(ip("203.0.113.5").journal_pattern(), r"(?:203\.0\.113\.5)");
        assert_eq!(
            ip("2001:db8::1").journal_pattern(),
            r"(?:2001\:db8\:\:1|2001\:0db8\:0000\:0000\:0000\:0000\:0000\:0001)"
        );
        assert_eq!(
            email("jane+x@example.com").journal_pattern(),
            r"(?:jane\+x\@example\.com|jane\+x\%40example\.com|jane\%2Bx\%40example\.com)"
        );
        // Every character a validated value can have, escaped or alphanumeric.
        let all = email("a!#%&'*+/=?^_{}~-.z@example.com").journal_pattern();
        let inner = &all[3..all.len() - 1];
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            match c {
                '\\' => assert!(!chars.next().unwrap().is_ascii_alphanumeric()),
                '|' => {}
                c => assert!(c.is_ascii_alphanumeric(), "{c:?} is not escaped in {all}"),
            }
        }
    }

    #[test]
    fn email_spellings_cover_query_strings() {
        assert_eq!(email_spellings("jane@example.com"), vec!["jane@example.com", "jane%40example.com"]);
        assert_eq!(
            email_spellings("jane+x@example.com"),
            vec!["jane+x@example.com", "jane+x%40example.com", "jane%2Bx%40example.com"]
        );
        assert_eq!(v6_expanded(&"2001:db8::1".parse().unwrap()), "2001:0db8:0000:0000:0000:0000:0000:0001");
    }

    // --- samples ------------------------------------------------------------

    #[test]
    fn samples_are_one_clean_line_of_at_most_500_characters() {
        assert_eq!(clean_sample(b"a\tb\x1b[31mc\r\n\0d\x7f"), "a b[31mcd");
        assert_eq!(clean_sample("x\u{202e}evil\u{2066}y\u{2028}z".as_bytes()), "xevilyz");
        assert_eq!(clean_sample(b"caf\xe9 \xff"), "caf\u{fffd} \u{fffd}");
        let exact = "x".repeat(500);
        assert_eq!(clean_sample(exact.as_bytes()), exact);
        let long = clean_sample("x".repeat(501).as_bytes());
        assert_eq!(long.chars().count(), 500);
        assert!(long.ends_with('…'));
        let wide = clean_sample("é".repeat(5000).as_bytes());
        assert_eq!(wide.chars().count(), 500);
        assert!(wide.ends_with('…'));
        let huge = clean_sample(&vec![b'a'; 8 * 1024 * 1024]);
        assert_eq!(huge.chars().count(), 500);
        // Control characters do not count towards the limit.
        let mut noisy = "\x07".repeat(300);
        noisy.push_str(&"y".repeat(400));
        assert_eq!(clean_sample(noisy.as_bytes()), "y".repeat(400));
    }

    // --- log files ----------------------------------------------------------

    #[test]
    fn log_names_and_their_rotations() {
        let any = |n| log_name(n, Names::AnyLog);
        assert_eq!(any("access.log"), Some(("access.log", Packed::Plain)));
        assert_eq!(any("access.log.1"), Some(("access.log", Packed::Plain)));
        assert_eq!(any("access.log.14.gz"), Some(("access.log", Packed::Gzip)));
        assert_eq!(any("access.log-20261124"), Some(("access.log", Packed::Plain)));
        assert_eq!(any("access.log-20261124.gz"), Some(("access.log", Packed::Gzip)));
        assert_eq!(any("example.com.access.log.3.gz"), Some(("example.com.access.log", Packed::Gzip)));
        assert_eq!(any("error.log.2.zst"), Some(("error.log", Packed::Other)));
        assert_eq!(any("error.log.2.xz"), Some(("error.log", Packed::Other)));
        for not in [
            ".access.log.vps-panel-erase.0123456789ab", "access.log.bak", "access.log~", "access.log.1.gz.tmp",
            "nginx.logrotate", "access.log.old", "access", "access.log.", "access.log.gz.1", "README",
        ] {
            assert_eq!(any(not), None, "{not}");
        }
        let sys = |n| log_name(n, Names::Bases(&["syslog", "auth.log"]));
        assert_eq!(sys("syslog"), Some(("syslog", Packed::Plain)));
        assert_eq!(sys("syslog.1"), Some(("syslog", Packed::Plain)));
        assert_eq!(sys("syslog.2.gz"), Some(("syslog", Packed::Gzip)));
        assert_eq!(sys("auth.log.4.gz"), Some(("auth.log", Packed::Gzip)));
        assert_eq!(sys("syslog-ng.conf"), None);
        assert_eq!(sys("kern.log"), None);
        assert!(is_error_log("error.log") && is_error_log("example.com.error.log") && !is_error_log("access.log"));
    }

    #[test]
    fn reading_stops_at_the_byte_limit_and_at_the_deadline() {
        let line = b"0123456789 0123456789 0123456789\n";
        let data: Vec<u8> = line.iter().copied().cycle().take(line.len() * 200_000).collect();
        // All of it, in pieces that end at a line end.
        let (mut seen, mut left) = (0usize, u64::MAX);
        let end = read_parts(&mut &data[..], far(), &mut left, &mut |part| {
            assert_eq!(part.last(), Some(&b'\n'));
            seen += part.len();
            Ok(true)
        })
        .unwrap();
        assert_eq!((end, seen), (End::Eof, data.len()));
        // The byte limit.
        let (mut seen, mut left) = (0usize, 1_000_000u64);
        let end = read_parts(&mut &data[..], far(), &mut left, &mut |part| {
            seen += part.len();
            Ok(true)
        })
        .unwrap();
        assert_eq!(end, End::Bytes);
        assert!(seen <= 1_000_000 && seen > 900_000, "{seen}");
        // The deadline.
        let mut left = u64::MAX;
        let end = read_parts(&mut &data[..], Instant::now(), &mut left, &mut |_| Ok(true)).unwrap();
        assert_eq!(end, End::Time);
        // The caller has seen enough.
        let mut left = u64::MAX;
        assert_eq!(read_parts(&mut &data[..], far(), &mut left, &mut |_| Ok(false)).unwrap(), End::Stopped);
        // A last line without a line end, and a "line" longer than MAX_LINE.
        let mut got = Vec::new();
        let mut left = u64::MAX;
        read_parts(&mut &b"one\ntwo"[..], far(), &mut left, &mut |p| {
            got.extend_from_slice(p);
            Ok(true)
        })
        .unwrap();
        assert_eq!(got, b"one\ntwo");
        let blob = vec![b'x'; MAX_LINE * 2 + 17];
        let (mut seen, mut left) = (0usize, u64::MAX);
        read_parts(&mut &blob[..], far(), &mut left, &mut |p| {
            seen += p.len();
            Ok(true)
        })
        .unwrap();
        assert_eq!(seen, blob.len());
    }

    /// access.log (newest), access.log.1, access.log.2.gz, error.log, and
    /// things that are not logs. Every access log has `per_file` lines of the
    /// subject among others.
    fn nginx_dir(name: &str, per_file: usize) -> PathBuf {
        let dir = scratch(name);
        let body = |tag: &str| {
            let mut s = String::new();
            for i in 0..per_file {
                s.push_str(&format!("198.51.100.{} - - [t] \"GET /{tag}/{i} HTTP/1.1\" 200 1\n", i % 250));
                s.push_str(&format!("203.0.113.87 - - [t] \"GET /{tag}/{i} HTTP/1.1\" 200 1 \"-\" \"UA\"\n"));
                s.push_str(&format!("203.0.113.8 - - [t] \"GET /{tag}/{i}?e=jane%40example.com HTTP/1.1\" 200 1\n"));
            }
            s
        };
        std::fs::write(dir.join("access.log"), body("live")).unwrap();
        std::fs::write(dir.join("access.log.1"), body("one")).unwrap();
        gzip(&dir.join("access.log.2.gz"), body("two").as_bytes());
        std::fs::write(
            dir.join("error.log"),
            "2026/11/24 10:00:00 [error] 7#7: *1 open() \"/x\" failed (2: No such file or directory), client: 203.0.113.87, server: x\n\
             2026/11/24 10:00:01 [error] 7#7: *2 open() \"/y\" failed (2: No such file or directory), client: 198.51.100.7, server: x\n",
        )
        .unwrap();
        std::fs::write(dir.join("notes.txt"), "203.0.113.87\n").unwrap();
        std::fs::write(dir.join(".access.log.swp"), "203.0.113.87\n").unwrap();
        std::os::unix::fs::symlink(dir.join("access.log"), dir.join("linked.log")).unwrap();
        set_mtime(&dir.join("access.log"), 10);
        set_mtime(&dir.join("access.log.1"), 86_400);
        set_mtime(&dir.join("access.log.2.gz"), 2 * 86_400);
        dir
    }

    #[test]
    fn a_search_reads_current_and_rotated_logs_newest_first() {
        let dir = nginx_dir("search", 30);
        let files = nginx_logs(&dir, false).unwrap();
        let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["access.log", "access.log.1", "access.log.2.gz"]);
        assert_eq!(nginx_logs(&dir, true).unwrap().len(), 1);

        let hits = search_files(&files, &ip("203.0.113.87"), far(), u64::MAX);
        assert_eq!(hits.count, 90);
        assert!(!hits.truncated && hits.problems.is_empty(), "{:?}", hits.problems);
        assert_eq!(hits.samples.len(), MAX_SAMPLES);
        // The newest file's last line first.
        assert_eq!(hits.samples[0], "203.0.113.87 - - [t] \"GET /live/29 HTTP/1.1\" 200 1 \"-\" \"UA\"");
        assert!(hits.samples[19].contains("/live/10 "));
        // The neighbour address is not this one.
        assert_eq!(search_files(&files, &ip("203.0.113.8"), far(), u64::MAX).count, 90);
        assert_eq!(search_files(&files, &ip("203.0.113.9"), far(), u64::MAX).count, 0);
        assert_eq!(search_files(&files, &email("jane@example.com"), far(), u64::MAX).count, 90);
        assert_eq!(search_files(&files, &email("ane@example.com"), far(), u64::MAX).count, 0);

        // With fewer than twenty in the newest file, older files fill up.
        let dir2 = nginx_dir("search-few", 3);
        let files2 = nginx_logs(&dir2, false).unwrap();
        let hits = search_files(&files2, &ip("203.0.113.87"), far(), u64::MAX);
        assert_eq!(hits.count, 9);
        let order: Vec<&str> =
            hits.samples.iter().map(|s| s.split("GET ").nth(1).unwrap().split(' ').next().unwrap()).collect();
        assert_eq!(order, ["/live/2", "/live/1", "/live/0", "/one/2", "/one/1", "/one/0", "/two/2", "/two/1", "/two/0"]);
        std::fs::remove_dir_all(dir).unwrap();
        std::fs::remove_dir_all(dir2).unwrap();
    }

    #[test]
    fn a_search_says_when_it_did_not_read_everything() {
        let dir = nginx_dir("limits", 2000);
        let files = nginx_logs(&dir, false).unwrap();
        let s = ip("203.0.113.87");
        // The byte limit ends it inside the first file.
        let hits = search_files(&files, &s, far(), 20_000);
        assert!(hits.truncated);
        assert!(hits.count > 0 && hits.count < 2000, "{}", hits.count);
        assert!(hits.problems[0].contains("3 older files not read to the end"), "{:?}", hits.problems);
        // The deadline.
        let hits = search_files(&files, &s, Instant::now(), u64::MAX);
        assert!(hits.truncated && hits.count == 0);
        assert!(hits.problems[0].contains("time limit"), "{:?}", hits.problems);
        // A broken archive and a format that is not read: the rest still is.
        std::fs::write(dir.join("access.log.3.gz"), b"\x1f\x8b\x08\x00 not gzip at all").unwrap();
        std::fs::write(dir.join("access.log.4.zst"), b"203.0.113.87\n").unwrap();
        let files = nginx_logs(&dir, false).unwrap();
        let hits = search_files(&files, &s, far(), u64::MAX);
        assert_eq!(hits.count, 6000);
        assert!(hits.truncated);
        let all = hits.problems.join(" | ");
        assert!(all.contains("access.log.3.gz is not a complete gzip file"), "{all}");
        assert!(all.contains("access.log.4.zst is compressed in a format that is not read here"), "{all}");
        // A directory that is not there is an empty source with a note.
        let gone = search_nginx(&dir.join("missing"), &s, false, far());
        assert_eq!(gone.len(), 2);
        assert!(gone.iter().all(|g| g.count == 0 && !g.truncated && g.note.contains("There is no ")));
        // Masking in new lines is mentioned where it matters.
        let masked = search_nginx(&dir, &s, true, far());
        assert!(masked[0].note.contains("mask addresses in new log lines, which then show 203.0.113.0"), "{}", masked[0].note);
        assert!(!search_nginx(&dir, &s, false, far())[0].note.contains("mask addresses in new"));
        assert!(!search_nginx(&dir, &email("jane@example.com"), true, far())[0].note.contains("mask addresses in new"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn file_id(path: &Path) -> (u64, u64) {
        let m = std::fs::symlink_metadata(path).unwrap();
        (m.dev(), m.ino())
    }

    fn no_copies_left(dir: &Path) {
        let left: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".vps-panel-erase."))
            .collect();
        assert!(left.is_empty(), "left behind: {left:?}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn erase_rewrites_logs_in_place_and_keeps_owner_mode_time_and_line_count() {
        let dir = nginx_dir("erase", 500);
        let as_root = nix::unistd::getuid().is_root();
        let mut before = Vec::new();
        for (name, mode) in [("access.log", 0o640), ("access.log.1", 0o640), ("access.log.2.gz", 0o600)] {
            let path = dir.join(name);
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            if as_root {
                std::os::unix::fs::chown(&path, Some(33), Some(4)).unwrap();
            }
            let age = match name {
                "access.log" => 3600,
                "access.log.1" => 86_400,
                _ => 2 * 86_400,
            };
            let mtime = set_mtime(&path, age);
            let meta = std::fs::metadata(&path).unwrap();
            let text = if name.ends_with(".gz") { gunzip(&path) } else { std::fs::read(&path).unwrap() };
            before.push((path, meta.uid(), meta.gid(), meta.mode(), mtime, text));
        }
        let error_id = file_id(&dir.join("error.log"));
        let notes = std::fs::read(dir.join("notes.txt")).unwrap();

        let s = ip("203.0.113.87");
        let out = erase_source("nginx_access", &s, &dir, Path::new(NO_DB));
        assert!(out.ok, "{}", out.detail);
        assert_eq!(out.changed, 1500);
        assert_eq!(out.detail, "Replaced with 203.0.113.0 in 1500 lines of 3 files.");

        for (path, uid, gid, mode, mtime, text) in &before {
            let meta = std::fs::metadata(path).unwrap();
            assert_eq!((meta.uid(), meta.gid()), (*uid, *gid), "{}", path.display());
            assert_eq!(meta.mode(), *mode, "{}", path.display());
            assert_eq!(meta.modified().unwrap(), *mtime, "{}", path.display());
            let now = if path.extension().is_some_and(|e| e == "gz") { gunzip(path) } else { std::fs::read(path).unwrap() };
            let (old, new) = (String::from_utf8_lossy(text), String::from_utf8_lossy(&now));
            assert_eq!(old.lines().count(), new.lines().count());
            for (a, b) in old.lines().zip(new.lines()) {
                if a.starts_with("203.0.113.87 ") {
                    assert_eq!(b, a.replacen("203.0.113.87", "203.0.113.0", 1));
                } else {
                    assert_eq!(a, b);
                }
            }
        }
        // Nothing else in the directory was touched.
        assert_eq!(file_id(&dir.join("error.log")), error_id);
        assert_eq!(std::fs::read(dir.join("notes.txt")).unwrap(), notes);
        assert_eq!(std::fs::read(dir.join(".access.log.swp")).unwrap(), b"203.0.113.87\n");
        no_copies_left(&dir);
        assert_eq!(search_files(&nginx_logs(&dir, false).unwrap(), &s, far(), u64::MAX).count, 0);

        // A second time finds nothing and rewrites nothing.
        let ids: Vec<(u64, u64)> = before.iter().map(|b| file_id(&b.0)).collect();
        let again = erase_source("nginx_access", &s, &dir, Path::new(NO_DB));
        assert_eq!((again.ok, again.changed, again.detail.as_str()), (true, 0, "No line holds it."));
        assert_eq!(before.iter().map(|b| file_id(&b.0)).collect::<Vec<_>>(), ids);

        // The error log is its own source.
        let out = erase_source("nginx_error", &s, &dir, Path::new(NO_DB));
        assert_eq!((out.ok, out.changed), (true, 1), "{}", out.detail);
        let errors = std::fs::read_to_string(dir.join("error.log")).unwrap();
        assert!(errors.contains("client: 203.0.113.0, server") && errors.contains("client: 198.51.100.7,"));

        // An email in the query string, in all three files.
        let out = erase_source("nginx_access", &email("jane@example.com"), &dir, Path::new(NO_DB));
        assert_eq!((out.ok, out.changed), (true, 1500), "{}", out.detail);
        assert!(std::fs::read_to_string(dir.join("access.log")).unwrap().contains("?e=[erased] HTTP/1.1"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn erase_leaves_alone_what_it_cannot_rewrite_safely() {
        let dir = nginx_dir("erase-refuse", 5);
        let s = ip("203.0.113.87");
        // A process that is not nginx has the live log open for writing.
        let log = std::fs::OpenOptions::new().append(true).open(dir.join("access.log")).unwrap();
        let mut holder = Kid(std::process::Command::new("sleep").arg("60").stdout(log).spawn().unwrap());
        // Broken and unsupported archives.
        std::fs::write(dir.join("access.log.3.gz"), b"\x1f\x8b\x08\x00 not gzip at all").unwrap();
        std::fs::write(dir.join("access.log.4.zst"), b"203.0.113.87\n").unwrap();
        let live = std::fs::read(dir.join("access.log")).unwrap();
        let live_id = file_id(&dir.join("access.log"));

        let out = erase_source("nginx_access", &s, &dir, Path::new(NO_DB));
        assert!(!out.ok);
        // The two files that could be rewritten were.
        assert_eq!(out.changed, 10, "{}", out.detail);
        assert!(out.detail.contains("access.log is open for writing by sleep (pid "), "{}", out.detail);
        assert!(out.detail.contains("access.log.3.gz is not a complete gzip file"), "{}", out.detail);
        assert!(out.detail.contains("access.log.4.zst is compressed in a format that cannot be rewritten here"), "{}", out.detail);
        assert_eq!(std::fs::read(dir.join("access.log")).unwrap(), live);
        assert_eq!(file_id(&dir.join("access.log")), live_id);
        no_copies_left(&dir);

        // Once the writer is gone the same call finishes the job.
        let _ = holder.0.kill();
        let _ = holder.0.wait();
        std::fs::remove_file(dir.join("access.log.3.gz")).unwrap();
        std::fs::remove_file(dir.join("access.log.4.zst")).unwrap();
        let out = erase_source("nginx_access", &s, &dir, Path::new(NO_DB));
        assert_eq!((out.ok, out.changed), (true, 5), "{}", out.detail);

        // Sources that are not erasable, unknown ones, and a masked address.
        for id in ["journal_mail", "journal_ssh", "fail2ban", "mailboxes", "forwarders", "maildir", "system_logs"] {
            let out = erase_source(id, &s, &dir, Path::new(NO_DB));
            assert_eq!((out.ok, out.changed), (false, 0), "{id}");
            assert!(out.detail.starts_with("Nothing is erased from this source here"), "{id}: {}", out.detail);
        }
        let out = erase_source("panel_audit\u{1b}[2J", &s, &dir, Path::new(NO_DB));
        assert_eq!((out.id.as_str(), out.ok, out.changed), ("panel_audit[2J", false, 0));
        std::fs::write(dir.join("access.log"), "203.0.113.0 - -\n").unwrap();
        let out = erase_source("nginx_access", &ip("203.0.113.0"), &dir, Path::new(NO_DB));
        assert_eq!((out.ok, out.changed), (true, 0));
        assert!(out.detail.contains("already a masked address"));
        // A leftover copy from an agent that was stopped mid-erase goes.
        std::fs::write(dir.join(".access.log.vps-panel-erase.0123456789ab"), "203.0.113.87\n").unwrap();
        erase_source("nginx_access", &s, &dir, Path::new(NO_DB));
        no_copies_left(&dir);
        // In a directory others can write to, nothing is rewritten.
        std::fs::write(dir.join("access.log"), "203.0.113.87 - -\n").unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        let out = erase_source("nginx_access", &s, &dir, Path::new(NO_DB));
        assert_eq!((out.ok, out.changed), (false, 0));
        assert!(out.detail.contains("is not a directory only root can write to"), "{}", out.detail);
        assert_eq!(std::fs::read(dir.join("access.log")).unwrap(), b"203.0.113.87 - -\n");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let out = erase_source("nginx_access", &s, &dir, Path::new(NO_DB));
        assert_eq!((out.ok, out.changed), (true, 1), "{}", out.detail);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A stand-in for nginx: a shell under the name `nginx` that appends
    /// numbered lines to `log`, some ten thousand a second, until it is
    /// stopped, and reopens the log by name on USR1 unless `stubborn`.
    #[cfg(target_os = "linux")]
    fn fake_nginx(dir: &Path, log: &Path, stubborn: bool) -> Kid {
        let bin = dir.join("nginx");
        if !bin.exists() {
            std::fs::copy("/bin/sh", &bin).unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let trap = if stubborn { "trap '' USR1" } else { "trap 'exec >>\"$LOG\"' USR1" };
        let script = format!(
            "{trap}; trap 'exit 0' TERM; exec >>\"$LOG\"; i=0; \
             while [ $i -lt 300000 ]; do i=$((i+1)); echo \"203.0.113.87 - - req $i\"; \
             j=0; while [ $j -lt 100 ]; do j=$((j+1)); done; done"
        );
        Kid(std::process::Command::new(&bin).arg("-c").arg(script).env("LOG", log).spawn().unwrap())
    }

    #[cfg(target_os = "linux")]
    fn stop(writer: &mut Kid) {
        let pid = nix::unistd::Pid::from_raw(writer.0.id() as i32);
        nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM).unwrap();
        assert!(writer.0.wait().unwrap().success());
    }

    /// The request numbers in the log, and how many lines have the masked
    /// address. Every number from 1 to the last must be there exactly once.
    #[cfg(target_os = "linux")]
    fn requests_logged(log: &Path) -> (u32, u64) {
        let text = std::fs::read_to_string(log).unwrap();
        let mut masked = 0;
        let mut seen: Vec<u32> = text
            .lines()
            .filter(|l| l.contains(" req "))
            .map(|l| {
                masked += u64::from(l.starts_with("203.0.113.0 "));
                assert!(l.starts_with("203.0.113.0 - - req ") || l.starts_with("203.0.113.87 - - req "), "torn line {l:?}");
                l.rsplit(' ').next().unwrap().parse().unwrap()
            })
            .collect();
        seen.sort_unstable();
        let last = *seen.last().unwrap();
        assert_eq!(seen.len(), last as usize, "lines were lost or doubled");
        assert!(seen.iter().copied().eq(1..=last), "lines were lost or doubled");
        (last, masked)
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_log_that_is_being_written_loses_no_line_when_it_is_rewritten() {
        let dir = scratch("live");
        let logs = dir.join("logs");
        std::fs::create_dir(&logs).unwrap();
        std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o755)).unwrap();
        let log = logs.join("access.log");
        std::fs::write(&log, "198.51.100.7 - - old\n203.0.113.87 - - old\n").unwrap();
        let mut writer = fake_nginx(&dir, &log, false);
        // Let it get going, then erase under it, three times.
        let s = ip("203.0.113.87");
        let mut changed = 0;
        for _ in 0..3 {
            std::thread::sleep(Duration::from_millis(60));
            let out = erase_source("nginx_access", &s, &logs, Path::new(NO_DB));
            assert!(out.ok, "{}", out.detail);
            assert!(!out.detail.contains("had not reopened"), "{}", out.detail);
            assert!(out.changed > 0);
            changed += out.changed;
        }
        std::thread::sleep(Duration::from_millis(60));
        stop(&mut writer);
        let (last, masked) = requests_logged(&log);
        assert!(last > 1000 && masked > 100 && masked < u64::from(last), "{last} lines, {masked} masked");
        // `changed` is exactly the lines that now have the masked address
        // (the one old line and the requests).
        assert_eq!(changed, masked + 1);
        assert!(std::fs::read_to_string(&log).unwrap().starts_with("198.51.100.7 - - old\n203.0.113.0 - - old\n"));
        no_copies_left(&logs);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_writer_that_does_not_reopen_is_followed_until_it_lets_go() {
        let dir = scratch("stubborn");
        let logs = dir.join("logs");
        std::fs::create_dir(&logs).unwrap();
        std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o755)).unwrap();
        let log = logs.join("access.log");
        std::fs::write(&log, "203.0.113.87 - - old\n").unwrap();
        let mut writer = fake_nginx(&dir, &log, true);
        std::thread::sleep(Duration::from_millis(60));
        let files = nginx_logs(&logs, false).unwrap();
        let wait = EraseLimits { soft: far(), hard: far(), reopen_wait: Duration::from_millis(300) };
        let done = erase_in_files(&logs, &files, &ip("203.0.113.87"), &wait);
        assert!(done.problems.is_empty(), "{:?}", done.problems);
        assert_eq!(done.notes.len(), 1);
        assert!(done.notes[0].starts_with("nginx (pid "), "{:?}", done.notes);
        assert!(done.notes[0].contains("had not reopened access.log after 0 s"), "{:?}", done.notes);
        let out = erased_files_outcome("nginx_access", &ip("203.0.113.87"), done);
        assert!(out.ok && out.changed > 0);

        // The writer still writes to the old file; its lines keep arriving.
        let at_erase = std::fs::read_to_string(&log).unwrap().lines().count();
        std::thread::sleep(Duration::from_millis(400));
        assert!(std::fs::read_to_string(&log).unwrap().lines().count() > at_erase);
        stop(&mut writer);
        let mut waited = 0;
        while std::fs::read_dir(&logs).unwrap().flatten().any(|e| e.file_name().to_string_lossy().contains(".vps-panel-erase.")) {
            waited += 1;
            assert!(waited < 400, "the old file was never let go");
            std::thread::sleep(Duration::from_millis(50));
        }
        let (last, masked) = requests_logged(&log);
        assert_eq!(u64::from(last), masked);
        // Everything that went through the erase or the follower is masked.
        assert!(!std::fs::read_to_string(&log).unwrap().contains("203.0.113.87"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    // --- the journal --------------------------------------------------------

    /// `journalctl -o short-iso -r` output as a server has it, newest first.
    const JOURNAL: &str = "\
2026-11-24T12:10:00+0100 vps sshd[4321]: Disconnected from authenticating user root 203.0.113.5 port 40022 [preauth]
2026-11-24T12:09:58+0100 vps sshd[4321]: Failed password for root from 203.0.113.5 port 40022 ssh2
2026-11-24T12:09:00+0100 vps kernel: [UFW BLOCK] IN=eth0 OUT= MAC=aa:bb SRC=203.0.113.5 DST=198.51.100.1 LEN=40 PROTO=TCP SPT=4 DPT=23
2026-11-24T12:08:00+0100 vps vps-panel-agent[77]: mailbox deleted; maildir moved to trash email=jane@example.com trash_id=x
2026-11-24T12:05:00+0100 vps dovecot[900]: imap-login: Login: user=<jane@example.com>, method=PLAIN, rip=203.0.113.5, lip=198.51.100.1, TLS
2026-11-24T12:04:00+0100 vps postfix/smtpd[1000]: connect from unknown[203.0.113.55]
2026-11-24T12:03:00+0100 vps postfix/submission/smtpd[2001]: 5BC23D4E5F: client=unknown[203.0.113.5], sasl_method=PLAIN, sasl_username=jane@example.com
2026-11-24T12:02:00+0100 vps postfix/lmtp[1003]: 4AB12C3D4E: to=<maryjane@example.com>, relay=vps[private/dovecot-lmtp], status=sent
2026-11-24T12:01:00+0100 vps postfix/lmtp[1003]: 4AB12C3D4E: to=<jane@example.com>, orig_to=<sales@example.com>, status=sent (250 2.0.0 <jane@example.com> Saved)
2026-11-24T12:00:00+0100 vps myapp[55]: request failed for 203.0.113.5
    at handler (203.0.113.5)
2026-11-24T11:59:00+0100 vps sshd-session[4400]: Accepted publickey for root from 203.0.113.50 port 1 ssh2
";

    fn journal_hits(subj: &Subject) -> JournalHits {
        let mut hits = JournalHits::default();
        let mut class = JournalClass::Other;
        for line in JOURNAL.lines() {
            hits.take(line.as_bytes(), subj, &mut class);
        }
        hits
    }

    #[test]
    fn journal_lines_are_sorted_by_the_service_that_wrote_them() {
        assert_eq!(journal_class("2026-11-24T12:10:00+0100 vps sshd[4321]: x"), Some(JournalClass::Ssh));
        assert_eq!(journal_class("2026-11-24T12:10:00+0100 vps sshd-session[1]: x"), Some(JournalClass::Ssh));
        assert_eq!(journal_class("2026-11-24T12:10:00+0100 vps postfix/smtps/smtpd[1]: x"), Some(JournalClass::Mail));
        assert_eq!(journal_class("2026-11-24T12:10:00+0100 vps dovecot[1]: x"), Some(JournalClass::Mail));
        assert_eq!(journal_class("2026-11-24T12:10:00+0100 vps kernel: x"), Some(JournalClass::Other));
        assert_eq!(journal_class("2026-11-24T12:10:00+0100 vps sudo[1]: sshd postfix"), Some(JournalClass::Other));
        assert_eq!(journal_class("    at handler (x)"), None);
        assert_eq!(journal_class(""), None);

        let hits = journal_hits(&ip("203.0.113.5"));
        // .55 and .50 are other addresses.
        assert_eq!((hits.mail.count, hits.ssh.count, hits.other.count), (2, 2, 3));
        assert!(hits.ssh.samples[0].contains("Disconnected from authenticating user root 203.0.113.5"));
        assert!(hits.mail.samples[0].contains("imap-login"));
        // The second line of an app's message belongs to that app.
        assert_eq!(hits.other.samples[2], "    at handler (203.0.113.5)");
        let sources = journal_sources(&ip("203.0.113.5"), hits);
        let ids: Vec<&str> = sources.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["journal_mail", "journal_ssh", "journal_other"]);
        assert!(sources.iter().all(|s| !s.erasable && !s.truncated && s.note.ends_with(JOURNAL_KEPT)));
        assert_eq!(sources.iter().map(|s| s.count).collect::<Vec<_>>(), [2, 2, 3]);

        let hits = journal_hits(&email("jane@example.com"));
        assert_eq!((hits.mail.count, hits.ssh.count, hits.other.count), (3, 0, 1));
        let sources = journal_sources(&email("jane@example.com"), hits);
        let ids: Vec<&str> = sources.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["journal_mail", "journal_other"]);
        assert!(sources[1].samples[0].contains("vps-panel-agent"));

        // A journal that could not be read to the end says so on each source.
        let cut = JournalHits { truncated: true, problem: Some("the search stopped at its time limit".into()), ..Default::default() };
        let sources = journal_sources(&ip("203.0.113.5"), cut);
        assert!(sources.iter().all(|s| s.truncated && s.count == 0 && s.note.ends_with("The search stopped at its time limit.")));
    }

    #[tokio::test]
    async fn a_command_is_read_line_by_line_within_its_limits() {
        // Long lines are cut, the count is honoured, the rest is not waited for.
        let args = ["-c".to_string(), "head -c 200000 /dev/zero | tr '\\0' x; echo; echo two; echo three; sleep 30".to_string()];
        let mut lines: Vec<usize> = Vec::new();
        let started = Instant::now();
        let ran = stream_lines("sh", &args, tokio::time::Instant::now() + Duration::from_secs(20), |l| {
            lines.push(l.len());
            lines.len() < 2
        })
        .await
        .unwrap();
        assert_eq!(lines, [JOURNAL_MAX_LINE_BYTES, 3]);
        assert!(ran.exit.is_none() && !ran.timed_out);
        assert!(started.elapsed() < Duration::from_secs(10));
        // The deadline.
        let args = ["-c".to_string(), "echo one; sleep 30".to_string()];
        let mut n = 0;
        let ran = stream_lines("sh", &args, tokio::time::Instant::now() + Duration::from_millis(300), |_| {
            n += 1;
            true
        })
        .await
        .unwrap();
        assert!(ran.timed_out && ran.exit.is_none() && n == 1);
        assert!(started.elapsed() < Duration::from_secs(10));
        // An exit code and what it said.
        let args = ["-c".to_string(), "echo out; echo why >&2; exit 3".to_string()];
        let ran = stream_lines("sh", &args, tokio::time::Instant::now() + Duration::from_secs(20), |_| true).await.unwrap();
        assert_eq!((ran.exit, ran.stderr.trim()), (Some(3), "why"));
        assert!(stream_lines("/nonexistent/journalctl", &[], tokio::time::Instant::now(), |_| true).await.is_err());
    }

    // --- fail2ban, mail, tasks ---------------------------------------------

    #[test]
    fn fail2ban_status_output_is_read_for_one_address() {
        let status = "Status\n|- Number of jail:\t3\n`- Jail list:\tdovecot, postfix, sshd\n";
        assert_eq!(fail2ban_jails(status), ["dovecot", "postfix", "sshd"]);
        assert!(fail2ban_jails("Status\n`- Jail list:\t\n").is_empty());
        assert!(fail2ban_jails("something else").is_empty());
        let jail = "Status for the jail: sshd\n|- Filter\n|  |- Currently failed:\t1\n`- Actions\n   |- Currently banned:\t3\n   |- Total banned:\t9\n   `- Banned IP list:\t203.0.113.5 198.51.100.77 2001:db8::1\n";
        assert!(fail2ban_bans(jail, "203.0.113.5".parse().unwrap()));
        assert!(fail2ban_bans(jail, "2001:db8::1".parse().unwrap()));
        assert!(!fail2ban_bans(jail, "203.0.113.50".parse().unwrap()));
        assert!(!fail2ban_bans(jail, "198.51.100.7".parse().unwrap()));
        assert!(!fail2ban_bans("   `- Banned IP list:\t\n", "203.0.113.5".parse().unwrap()));
    }

    #[test]
    fn fail2ban_database_records_are_read_for_one_address() {
        let dir = scratch("f2b");
        let db = dir.join("fail2ban.sqlite3");
        let conn = rusqlite::Connection::open(&db).unwrap();
        // The tables as fail2ban 1.0 creates them (server/database.py).
        conn.execute_batch(
            "CREATE TABLE bans(jail TEXT NOT NULL, ip TEXT, timeofban INTEGER NOT NULL, bantime INTEGER NOT NULL, bancount INTEGER NOT NULL default 1, data JSON);
             CREATE TABLE bips(ip TEXT NOT NULL, jail TEXT NOT NULL, timeofban INTEGER NOT NULL, bantime INTEGER NOT NULL, bancount INTEGER NOT NULL default 1, data JSON, PRIMARY KEY(ip, jail));
             INSERT INTO bans VALUES ('sshd', '203.0.113.5', 1795000000, 3600, 1, '{\"matches\":[\"secret line\"]}');
             INSERT INTO bans VALUES ('sshd', '203.0.113.5', 1795500000, 7200, 2, '{}');
             INSERT INTO bans VALUES ('dovecot', '203.0.113.50', 1795500000, 7200, 1, '{}');
             INSERT INTO bips VALUES ('203.0.113.5', 'sshd', 1795500000, 7200, 2, '{}');
             INSERT INTO bips VALUES ('203.0.113.5', 'postfix', 1795400000, 600, 1, '{}');",
        )
        .unwrap();
        drop(conn);
        let rows = fail2ban_records(&db, "203.0.113.5").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "postfix");
        assert!(rows[0].1.starts_with("1 ban on record, the last one on 2026-"), "{}", rows[0].1);
        assert_eq!(rows[1].0, "sshd");
        assert!(rows[1].1.starts_with("2 bans on record"), "{}", rows[1].1);
        assert!(fail2ban_records(&db, "203.0.113.50").unwrap().len() == 1);
        assert!(fail2ban_records(&db, "203.0.113.").unwrap().is_empty());
        assert!(fail2ban_records(&db, "' OR 1=1 --").unwrap().is_empty());
        assert!(fail2ban_records(&dir.join("missing.sqlite3"), "203.0.113.5").is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn queued_mail_is_matched_by_sender_and_recipient() {
        let line = br#"{"queue_name": "deferred", "queue_id": "4AB12C3D4E", "arrival_time": 1795500000, "message_size": 512, "forced_expire": false, "sender": "Jane@Example.com", "recipients": [{"address": "bob@slow.example", "delay_reason": "connect to slow.example[198.51.100.2]:25: Connection refused"}, {"address": "x@y.example", "orig_address": "sales@example.com"}]}"#;
        let got = queue_line(line, "jane@example.com").unwrap();
        assert!(got.starts_with("deferred 4AB12C3D4E: from Jane@Example.com to bob@slow.example, x@y.example, arrived 2026-"), "{got}");
        assert!(queue_line(line, "bob@slow.example").is_some());
        assert!(queue_line(line, "sales@example.com").is_some());
        assert!(queue_line(line, "ane@example.com").is_none());
        assert!(queue_line(line, "slow.example").is_none());
        assert!(queue_line(b"postqueue: warning: Mail system is down", "jane@example.com").is_none());
        assert!(queue_line(b"{}", "jane@example.com").is_none());
    }

    #[test]
    fn a_maildir_and_its_trash_entry_are_found_by_address() {
        assert_eq!(maildir_of("jane.doe@example.com"), Some(PathBuf::from("/var/vmail/example.com/jane.doe")));
        // An address no mailbox here can have has no maildir to look for.
        assert_eq!(maildir_of("o'brien@example.com"), None);
        assert_eq!(maildir_of("a/b@example.com"), None);

        let dir = scratch("maildir");
        std::fs::create_dir_all(dir.join("cur")).unwrap();
        std::fs::create_dir_all(dir.join("new")).unwrap();
        std::fs::write(dir.join("cur/1"), vec![0u8; 3000]).unwrap();
        std::fs::write(dir.join("new/2"), vec![0u8; 500]).unwrap();
        std::fs::write(dir.join("dovecot.index"), vec![0u8; 100]).unwrap();
        std::os::unix::fs::symlink("/etc", dir.join("link")).unwrap();
        assert_eq!(folder_usage(&dir, far(), 1000), (3, 3600, true));
        assert!(!folder_usage(&dir, far(), 2).2);
        assert_eq!(human_size(3600), "4 kB");
        assert_eq!(human_size(5 * 1024 * 1024 + 1), "5.0 MB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 GB");

        let trash = scratch("trash");
        for (id, original) in [
            ("20261101000000-aaaaaaaa", "/var/vmail/example.com/jane"),
            ("20261120000000-bbbbbbbb", "/var/vmail/example.com/jane"),
            ("20261121000000-cccccccc", "/var/vmail/example.com/maryjane"),
            ("20261122000000-dddddddd", "/var/vmail/example.com/jane/cur"),
        ] {
            std::fs::create_dir_all(trash.join(id)).unwrap();
            let meta = crate::handlers::files::TrashMeta {
                id: id.into(),
                name: "jane".into(),
                kind: "dir".into(),
                root_id: "fs".into(),
                original_path: original.into(),
                size: 4096,
                trashed_at: format!("2026-11-{}T00:00:00Z", &id[6..8]),
            };
            std::fs::write(trash.join(id).join("meta.json"), serde_json::to_vec(&meta).unwrap()).unwrap();
        }
        std::fs::create_dir_all(trash.join("broken")).unwrap();
        std::fs::write(trash.join("broken/meta.json"), b"{").unwrap();
        let s = search_trash(&trash, &email("jane@example.com"));
        assert_eq!((s.id.as_str(), s.count, s.erasable), ("mail_trash", 2, false));
        assert!(s.samples[0].contains("20261120000000-bbbbbbbb"), "{:?}", s.samples);
        assert!(s.note.contains("30 days"));
        assert_eq!(search_trash(&trash, &email("joe@example.com")).count, 0);
        assert_eq!(search_trash(&dir.join("missing"), &email("jane@example.com")).count, 0);
        std::fs::remove_dir_all(dir).unwrap();
        std::fs::remove_dir_all(trash).unwrap();
    }

    fn tasks_db(dir: &Path) -> PathBuf {
        let db = dir.join("panel.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE tasks (id TEXT PRIMARY KEY, kind TEXT NOT NULL, status TEXT NOT NULL, started_at TEXT NOT NULL, finished_at TEXT, log_blob TEXT NOT NULL DEFAULT '');
             CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO settings VALUES ('privacy.log_ip', 'masked');
             INSERT INTO tasks VALUES ('t1', 'apps.deploy:shop', 'succeeded', '2026-11-20T10:00:00+00:00', '2026-11-20T10:01:00+00:00',
               '$ git clone' || char(10) || 'Author: Jane <jane@example.com>' || char(10) || 'connect from 203.0.113.87 ok' || char(10) || 'built café' || char(10) || 'done for jane@example.com and 203.0.113.87');
             INSERT INTO tasks VALUES ('t2', 'apps.deploy:blog', 'running', '2026-11-24T10:00:00+00:00', NULL,
               'peer 203.0.113.87 connected');
             INSERT INTO tasks VALUES ('t3', 'ssl.issue:example.com', 'failed', '2026-11-22T10:00:00+00:00', '2026-11-22T10:00:09+00:00',
               'nothing here' || char(10) || '203.0.113.870 is another address');",
        )
        .unwrap();
        db
    }

    #[test]
    fn task_logs_are_searched_and_erased_in_finished_tasks_only() {
        let dir = scratch("tasks");
        let db = tasks_db(&dir);
        let s = ip("203.0.113.87");
        let found = search_tasks(&db, &s, far());
        assert_eq!((found.id.as_str(), found.count, found.erasable, found.truncated), ("task_logs", 3, true, false));
        // Newest task first, and within a task its last line first.
        assert_eq!(found.samples[0], "apps.deploy:blog (2026-11-24T10:00:00+00:00): peer 203.0.113.87 connected");
        assert!(found.samples[1].ends_with("done for jane@example.com and 203.0.113.87"));
        assert_eq!(search_tasks(&db, &email("jane@example.com"), far()).count, 2);

        let out = erase_tasks(&db, &s, far());
        assert_eq!((out.ok, out.changed), (false, 2), "{}", out.detail);
        assert_eq!(
            out.detail,
            "Replaced with 203.0.113.0 in 2 lines of finished tasks. 1 running task holds it and was left alone; run the erase again when it has finished."
        );
        let conn = rusqlite::Connection::open(&db).unwrap();
        let log = |id: &str| conn.query_row("SELECT log_blob FROM tasks WHERE id = ?1", [id], |r| r.get::<_, String>(0)).unwrap();
        assert_eq!(
            log("t1"),
            "$ git clone\nAuthor: Jane <jane@example.com>\nconnect from 203.0.113.0 ok\nbuilt café\ndone for jane@example.com and 203.0.113.0"
        );
        assert_eq!(log("t2"), "peer 203.0.113.87 connected");
        assert_eq!(log("t3"), "nothing here\n203.0.113.870 is another address");
        // Again: only the running task still has it.
        let out = erase_tasks(&db, &s, far());
        assert_eq!((out.ok, out.changed), (false, 0));
        conn.execute("UPDATE tasks SET status = 'failed' WHERE id = 't2'", []).unwrap();
        let out = erase_tasks(&db, &s, far());
        assert_eq!((out.ok, out.changed), (true, 1), "{}", out.detail);
        assert_eq!(search_tasks(&db, &s, far()).count, 0);
        let out = erase_tasks(&db, &email("jane@example.com"), far());
        assert_eq!((out.ok, out.changed), (true, 2));
        assert!(log("t1").contains("Author: Jane <[erased]>"));
        let out = erase_tasks(&db, &s, far());
        assert_eq!((out.ok, out.changed, out.detail.as_str()), (true, 0, "No finished task's log holds it."));

        // A database that is not there is a note, not an error.
        let missing = search_tasks(&dir.join("missing.db"), &s, far());
        assert!(missing.count == 0 && missing.truncated && missing.note.ends_with("The task logs could not be read."));
        assert!(!erase_tasks(&dir.join("missing.db"), &s, far()).ok);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sentences_and_outcomes_read_as_text() {
        assert_eq!(sentence("stopped at the time limit"), "Stopped at the time limit.");
        assert_eq!(sentence("access.log.2.gz is not a complete gzip file"), "access.log.2.gz is not a complete gzip file.");
        assert_eq!(sentence("Already one."), "Already one.");
        let done = ErasedFiles { changed: 1, files: 1, problems: vec!["x.log: no".into()], notes: vec![] };
        let out = erased_files_outcome("nginx_access", &email("jane@example.com"), done);
        assert_eq!((out.ok, out.changed), (false, 1));
        assert_eq!(out.detail, "Replaced with [erased] in 1 line of 1 file. x.log: no.");
        // The erasable sources are the ones erase handles.
        let erasable: Vec<&str> = SOURCES.iter().filter(|s| s.1).map(|s| s.0).collect();
        assert_eq!(erasable, ["nginx_access", "nginx_error", "task_logs"]);
    }

    // --- the handlers -------------------------------------------------------

    fn state(dir: &Path) -> AppState {
        let db = tasks_db(dir);
        let config = crate::config::Config {
            socket_path: dir.join("agent.sock"),
            agent_token: "t".repeat(40),
            panel_db_path: db.clone(),
            // Nothing listens there: the mail database cannot be read.
            mariadb_url: "mysql://root@127.0.0.1:1/".into(),
            restic_password: String::new(),
            restic_repository: String::new(),
            panel_host: "panel.example.com".into(),
            acme_email: "ops@example.com".into(),
            acme_directory: String::new(),
            vps_panel_gid: 0,
            vps_panel_uid: None,
            apps_uid: 0,
            apps_gid: 0,
        };
        let panel_db = crate::db::PanelDb::open(&db).unwrap();
        AppState {
            config: std::sync::Arc::new(config),
            tasks: crate::ipc::tasks::TaskTracker::new(panel_db.clone()),
            panel_db,
            mariadb: crate::db::MariaDb::connect("mysql://root@127.0.0.1:1/").unwrap(),
        }
    }

    /// On a machine with no mail database, no journal, no fail2ban and
    /// whatever /var/log holds: every source still answers.
    #[tokio::test]
    async fn a_search_answers_for_every_source_whatever_the_server_has() {
        let dir = scratch("handler");
        let state = state(&dir);
        let Json(report) = search(
            State(state.clone()),
            Json(SubjectReq { kind: "ip".into(), value: " ::FFFF:203.0.113.87 ".into() }),
        )
        .await
        .unwrap();
        assert_eq!((report.kind, report.value.as_str()), ("ip", "203.0.113.87"));
        let ids: Vec<&str> = report.sources.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            ["nginx_access", "nginx_error", "journal_mail", "journal_ssh", "journal_other", "system_logs", "fail2ban", "task_logs"]
        );
        for s in &report.sources {
            assert!(!s.note.is_empty() && !s.label.is_empty(), "{}", s.id);
            assert!(s.samples.len() <= MAX_SAMPLES && s.samples.len() as u64 <= s.count, "{}", s.id);
            assert_eq!(s.erasable, SOURCES.iter().any(|(id, e)| *id == s.id && *e), "{}", s.id);
        }
        let tasks = report.sources.last().unwrap();
        assert_eq!(tasks.count, 3);
        // privacy.log_ip is `masked` in this panel.db.
        assert!(masking_is_on(&state));
        assert!(report.sources[0].note.contains("only lines written before that can be found under the full address"));
        state.panel_db.with(|c| c.execute("UPDATE settings SET value = 'full'", [])).unwrap();
        assert!(!masking_is_on(&state));
        state.panel_db.with(|c| c.execute("DELETE FROM settings", [])).unwrap();
        assert!(!masking_is_on(&state));

        let Json(report) = search(
            State(state.clone()),
            Json(SubjectReq { kind: "email".into(), value: "Jane@Example.com".into() }),
        )
        .await
        .unwrap();
        assert_eq!((report.kind, report.value.as_str()), ("email", "jane@example.com"));
        let ids: Vec<&str> = report.sources.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "nginx_access", "nginx_error", "journal_mail", "journal_other", "system_logs", "mailboxes",
                "forwarders", "maildir", "mail_trash", "mail_queue", "task_logs"
            ]
        );
        let boxes = report.sources.iter().find(|s| s.id == "mailboxes").unwrap();
        assert!(boxes.count == 0 && boxes.truncated && boxes.note.contains("could not be read"), "{}", boxes.note);
        // The JSON has exactly the fields of the contract.
        let json = serde_json::to_value(&report.sources[0]).unwrap();
        let mut keys: Vec<&str> = json.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["count", "erasable", "id", "label", "note", "samples", "truncated"]);

        // A value that is not one address is a 400 before anything is read.
        for (kind, value) in [("ip", "203.0.113.0/24"), ("email", "a@b.example, c@d.example"), ("name", "jane")] {
            let err = search(State(state.clone()), Json(SubjectReq { kind: kind.into(), value: value.into() })).await;
            assert!(matches!(err, Err(AgentError::Invalid(_))), "{kind} {value}");
            let err = erase(
                State(state.clone()),
                Json(EraseReq { kind: kind.into(), value: value.into(), sources: vec!["task_logs".into()] }),
            )
            .await;
            assert!(matches!(err, Err(AgentError::Invalid(_))), "{kind} {value}");
        }

        // Erase acts on the sources it is given and on nothing else.
        let Json(done) = erase(
            State(state.clone()),
            Json(EraseReq {
                kind: "ip".into(),
                value: "203.0.113.87".into(),
                sources: vec!["task_logs".into(), "journal_ssh".into(), "task_logs".into(), "panel_logins".into()],
            }),
        )
        .await
        .unwrap();
        let got: Vec<(&str, bool, u64)> = done.results.iter().map(|r| (r.id.as_str(), r.ok, r.changed)).collect();
        assert_eq!(got, [("task_logs", false, 2), ("journal_ssh", false, 0), ("panel_logins", false, 0)]);
        let Json(none) = erase(
            State(state),
            Json(EraseReq { kind: "ip".into(), value: "203.0.113.87".into(), sources: vec![] }),
        )
        .await
        .unwrap();
        assert!(none.results.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    // --- against the real thing (cargo test -- --ignored) --------------------

    /// `lines` access log lines of about a hundred bytes; every fourth is
    /// from `from`.
    fn access_lines(tag: &str, lines: usize, from: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(lines * 110);
        for i in 0..lines {
            let ip = if i % 4 == 0 { from.to_string() } else { format!("198.51.{}.{}", i % 250, (i / 250) % 250) };
            out.extend_from_slice(
                format!(
                    "{ip} - - [24/Nov/2026:10:00:00 +0000] \"GET /{tag}/{i}?u=user{i}%40example.org HTTP/1.1\" 200 512 \"-\" \"Mozilla/5.0 (X11; Linux x86_64)\"\n"
                )
                .as_bytes(),
            );
        }
        out
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "slow: 1.2 million log lines searched and rewritten within the limits (cargo test --release -- --ignored)"]
    fn large_logs_are_searched_and_rewritten_within_the_limits() {
        let dir = scratch("large");
        const LINES: usize = 400_000;
        std::fs::write(dir.join("access.log"), access_lines("live", LINES, "203.0.113.87")).unwrap();
        std::fs::write(dir.join("access.log.1"), access_lines("one", LINES, "203.0.113.87")).unwrap();
        gzip(&dir.join("access.log.2.gz"), &access_lines("two", LINES, "2001:db8:85a3:8d3::7"));
        set_mtime(&dir.join("access.log.1"), 86_400);
        let gz_mtime = set_mtime(&dir.join("access.log.2.gz"), 2 * 86_400);
        let files = nginx_logs(&dir, false).unwrap();
        let total: u64 = files.iter().map(|f| std::fs::metadata(&f.path).unwrap().len()).sum();

        for (subj, expect) in [
            (ip("203.0.113.87"), 2 * LINES as u64 / 4),
            (ip("203.0.113.8"), 0),
            (ip("2001:db8:85a3:8d3::7"), LINES as u64 / 4),
            (email("user77@example.org"), 3),
        ] {
            let started = Instant::now();
            let hits = search_files(&files, &subj, Instant::now() + SEARCH_SOURCE_LIMIT, SEARCH_MAX_BYTES);
            eprintln!("search {:>22}: {} lines in {:?} ({} MB on disk)", subj.value, hits.count, started.elapsed(), total / 1_000_000);
            assert_eq!(hits.count, expect, "{}", subj.value);
            assert!(!hits.truncated, "{:?}", hits.problems);
            assert!(started.elapsed() < SEARCH_SOURCE_LIMIT);
        }

        for (subj, expect) in [
            (ip("203.0.113.87"), 2 * LINES as u64 / 4),
            (ip("2001:db8:85a3:8d3::7"), LINES as u64 / 4),
            (email("user77@example.org"), 3),
        ] {
            let started = Instant::now();
            let out = erase_source("nginx_access", &subj, &dir, Path::new(NO_DB));
            eprintln!("erase  {:>22}: {} lines in {:?}", subj.value, out.changed, started.elapsed());
            assert!(out.ok, "{}", out.detail);
            assert_eq!(out.changed, expect, "{}", subj.value);
            assert!(started.elapsed() < ERASE_SOFT_LIMIT);
            let started = Instant::now();
            let again = erase_source("nginx_access", &subj, &dir, Path::new(NO_DB));
            eprintln!("again  {:>22}: {} lines in {:?}", subj.value, again.changed, started.elapsed());
            assert_eq!((again.ok, again.changed), (true, 0));
        }
        for name in ["access.log", "access.log.1"] {
            assert_eq!(std::fs::read(dir.join(name)).unwrap().iter().filter(|&&b| b == b'\n').count(), LINES);
        }
        let unpacked = gunzip(&dir.join("access.log.2.gz"));
        assert_eq!(unpacked.iter().filter(|&&b| b == b'\n').count(), LINES);
        assert!(unpacked.starts_with(b"2001:db8:85a3:: - - ["));
        assert_eq!(std::fs::metadata(dir.join("access.log.2.gz")).unwrap().modified().unwrap(), gz_mtime);
        no_copies_left(&dir);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(target_os = "linux")]
    fn get(port: u16, path: &str) -> Option<u16> {
        let mut conn = std::net::TcpStream::connect(("127.0.0.1", port)).ok()?;
        conn.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
        conn.write_all(format!("GET {path} HTTP/1.0\r\nHost: test\r\n\r\n").as_bytes()).ok()?;
        let mut answer = String::new();
        conn.read_to_string(&mut answer).ok()?;
        answer.split(' ').nth(1)?.parse().ok()
    }

    /// nginx as these tests run it. When a test fails with it still running,
    /// the master is asked to stop first: killed outright, it would leave
    /// its workers behind.
    #[cfg(target_os = "linux")]
    struct Nginx(std::process::Child);

    #[cfg(target_os = "linux")]
    impl Drop for Nginx {
        fn drop(&mut self) {
            if !matches!(self.0.try_wait(), Ok(None)) {
                return;
            }
            let pid = nix::unistd::Pid::from_raw(self.0.id() as i32);
            let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM);
            for _ in 0..60 {
                if !matches!(self.0.try_wait(), Ok(None)) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// A real nginx (a master and two workers that are not root) with its
    /// logs in `<dir>/logs`, which already hold three files of old lines.
    #[cfg(target_os = "linux")]
    fn start_nginx(dir: &Path, old: usize) -> (Nginx, u16, PathBuf) {
        let logs = dir.join("logs");
        std::fs::create_dir(&logs).unwrap();
        std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::create_dir(dir.join("tmp")).unwrap();
        std::fs::set_permissions(dir.join("tmp"), std::fs::Permissions::from_mode(0o777)).unwrap();
        std::fs::write(logs.join("access.log"), access_lines("old", old, "127.0.0.1")).unwrap();
        std::fs::write(logs.join("access.log.1"), access_lines("older", old, "127.0.0.1")).unwrap();
        gzip(&logs.join("access.log.2.gz"), &access_lines("oldest", old, "127.0.0.1"));
        // As Ubuntu's nginx and logrotate leave them.
        for name in ["access.log", "access.log.1", "access.log.2.gz"] {
            std::os::unix::fs::chown(logs.join(name), Some(33), Some(4)).unwrap();
            std::fs::set_permissions(logs.join(name), std::fs::Permissions::from_mode(0o640)).unwrap();
        }
        set_mtime(&logs.join("access.log.1"), 86_400);
        set_mtime(&logs.join("access.log.2.gz"), 2 * 86_400);

        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let (d, l) = (dir.display(), logs.display());
        let conf = format!(
            "user www-data;\nworker_processes 2;\ndaemon off;\npid {d}/nginx.pid;\nerror_log {l}/error.log;\n\
             events {{ worker_connections 512; }}\n\
             http {{\n  access_log {l}/access.log;\n  client_body_temp_path {d}/tmp/body;\n  proxy_temp_path {d}/tmp/proxy;\n  \
             fastcgi_temp_path {d}/tmp/fastcgi;\n  uwsgi_temp_path {d}/tmp/uwsgi;\n  scgi_temp_path {d}/tmp/scgi;\n  \
             server {{\n    listen 127.0.0.1:{port};\n    location /hit {{ return 200 \"ok\\n\"; }}\n    \
             location /missing {{ root /nonexistent; }}\n  }}\n}}\n"
        );
        std::fs::write(dir.join("nginx.conf"), conf).unwrap();
        let nginx = Nginx(
            std::process::Command::new("nginx")
                .arg("-p")
                .arg(dir)
                .arg("-c")
                .arg(dir.join("nginx.conf"))
                .arg("-e")
                .arg(logs.join("error.log"))
                .spawn()
                .expect("nginx is not installed"),
        );
        let mut up = false;
        for _ in 0..100 {
            if get(port, "/hit/warmup") == Some(200) {
                up = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(up, "nginx did not start");
        (nginx, port, logs)
    }

    /// Four clients that ask for /hit/<id>, and every tenth time for
    /// /missing/<id> (a 404 and an error log line), until `stop` is set.
    #[cfg(target_os = "linux")]
    struct Clients {
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        threads: Vec<std::thread::JoinHandle<(Vec<String>, Vec<String>, usize)>>,
    }

    #[cfg(target_os = "linux")]
    impl Clients {
        fn start(port: u16) -> Clients {
            use std::sync::atomic::Ordering;
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let threads = (0..4)
                .map(|t| {
                    let stop = stop.clone();
                    std::thread::spawn(move || {
                        let (mut hits, mut missing, mut asked) = (Vec::new(), Vec::new(), 0);
                        while !stop.load(Ordering::Relaxed) && asked < 500_000 {
                            let i = asked;
                            asked += 1;
                            if i % 10 == 9 {
                                if get(port, &format!("/missing/t{t}-{i}")) == Some(404) {
                                    missing.push(format!("t{t}-{i}"));
                                }
                            } else if get(port, &format!("/hit/t{t}-{i}")) == Some(200) {
                                hits.push(format!("t{t}-{i}"));
                            }
                        }
                        (hits, missing, asked)
                    })
                })
                .collect();
            Clients { stop, threads }
        }

        /// The ids nginx answered for, sorted: /hit/ and /missing/. Every
        /// request must have been answered.
        fn finish(self) -> (Vec<String>, Vec<String>) {
            self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
            let (mut hits, mut missing, mut asked) = (Vec::new(), Vec::new(), 0);
            for c in self.threads {
                let (h, m, a) = c.join().unwrap();
                hits.extend(h);
                missing.extend(m);
                asked += a;
            }
            assert_eq!(hits.len() + missing.len(), asked, "some requests were not answered");
            assert!(asked > 2000, "only {asked} requests were made");
            hits.sort();
            missing.sort();
            (hits, missing)
        }
    }

    /// QUIT: nginx finishes what it is doing and writes its last lines.
    #[cfg(target_os = "linux")]
    fn quit(nginx: &mut Nginx) {
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(nginx.0.id() as i32), nix::sys::signal::Signal::SIGQUIT).unwrap();
        assert!(nginx.0.wait().unwrap().success());
    }

    /// The text of every log in `logs` whose name `errors` selects.
    #[cfg(target_os = "linux")]
    fn all_text(logs: &Path, errors: bool) -> String {
        let mut text = String::new();
        for f in nginx_logs(logs, errors).unwrap() {
            let bytes = if f.packed == Packed::Gzip { gunzip(&f.path) } else { std::fs::read(&f.path).unwrap() };
            text.push_str(&String::from_utf8(bytes).unwrap());
        }
        text
    }

    /// The ids after `marker` in the lines of `text`, sorted.
    #[cfg(target_os = "linux")]
    fn ids_after(text: &str, marker: &str, end: char) -> Vec<String> {
        let mut ids: Vec<String> = text
            .lines()
            .filter_map(|l| l.split(marker).nth(1))
            .map(|rest| rest.split(end).next().unwrap().to_string())
            .filter(|id| id != "warmup")
            .collect();
        ids.sort();
        ids
    }

    /// A real nginx serves requests while its access log and its error log
    /// are rewritten under it. Every request it answered is in the log
    /// afterwards, once, and erase reports exactly the lines it changed.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "needs nginx and root: a running nginx keeps logging through an erase (cargo test --release -- --ignored)"]
    fn a_running_nginx_loses_no_line_when_its_logs_are_rewritten() {
        let dir = scratch("nginx");
        const OLD: usize = 300_000;
        let (mut nginx, port, logs) = start_nginx(&dir, OLD);
        let before = std::fs::metadata(logs.join("access.log")).unwrap();
        let clients = Clients::start(port);

        let s = ip("127.0.0.1");
        let mut changed = (0u64, 0u64);
        for round in 0..5 {
            std::thread::sleep(Duration::from_millis(150));
            let started = Instant::now();
            let out = erase_source("nginx_access", &s, &logs, Path::new(NO_DB));
            eprintln!("round {round}: access logs, {} lines in {:?}: {}", out.changed, started.elapsed(), out.detail);
            assert!(out.ok && !out.detail.contains("had not reopened"), "{}", out.detail);
            changed.0 += out.changed;
            let started = Instant::now();
            let out = erase_source("nginx_error", &s, &logs, Path::new(NO_DB));
            eprintln!("round {round}: error log, {} lines in {:?}: {}", out.changed, started.elapsed(), out.detail);
            assert!(out.ok && !out.detail.contains("had not reopened"), "{}", out.detail);
            changed.1 += out.changed;
        }
        // No nginx process is left writing to a file that has no name.
        for proc in std::fs::read_dir("/proc").unwrap().flatten() {
            let pid = proc.file_name().to_string_lossy().into_owned();
            if std::fs::read_to_string(format!("/proc/{pid}/comm")).map(|c| c.trim() == "nginx").unwrap_or(false) {
                for fd in std::fs::read_dir(format!("/proc/{pid}/fd")).into_iter().flatten().flatten() {
                    let target = std::fs::read_link(fd.path()).unwrap_or_default();
                    let target = target.to_string_lossy();
                    assert!(
                        !(target.starts_with(&*logs.to_string_lossy()) && target.contains("(deleted)")),
                        "nginx {pid} still holds {target}"
                    );
                }
            }
        }

        let (hits, missing) = clients.finish();
        quit(&mut nginx);
        eprintln!("{} requests", hits.len() + missing.len());

        let access = std::fs::read_to_string(logs.join("access.log")).unwrap();
        assert!(ids_after(&access, "\"GET /hit/", ' ') == hits, "access log lines were lost or doubled");
        assert!(ids_after(&access, "\"GET /missing/", ' ') == missing, "access log lines were lost or doubled");
        assert_eq!(access.lines().filter(|l| l.contains("\"GET /old/")).count(), OLD);
        // The old lines are at the top, in their order, the address masked.
        assert!(access.starts_with("127.0.0.0 - - [24/Nov/2026:10:00:00 +0000] \"GET /old/0?"));
        assert!(!access.lines().take(OLD).any(|l| l.starts_with("127.0.0.1 ")));
        let errors = std::fs::read_to_string(logs.join("error.log")).unwrap();
        assert!(ids_after(&errors, "open() \"/nonexistent/missing/", '"') == missing, "error log lines were lost or doubled");
        // What erase reports is what the files show.
        let masked = |text: &str| text.lines().filter(|l| l.contains("127.0.0.0")).count() as u64;
        let rotated = OLD as u64 / 4 * 2;
        assert_eq!(changed.0, masked(&access) + rotated);
        assert_eq!(changed.1, masked(&errors));
        assert!(changed.1 > 0, "no error log line was written before an erase");
        // Owner and mode as before, for nginx's workers and for logrotate.
        let after = std::fs::metadata(logs.join("access.log")).unwrap();
        assert_eq!((after.uid(), after.gid(), after.mode()), (before.uid(), before.gid(), before.mode()));
        assert_eq!(gunzip(&logs.join("access.log.2.gz")).iter().filter(|&&b| b == b'\n').count(), OLD);
        no_copies_left(&logs);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The same with logrotate rotating the logs (create, delaycompress,
    /// USR1 in postrotate, as Ubuntu's nginx package has it) while they are
    /// rewritten. An erase may find a file rotated away under it and say so;
    /// no line may be lost.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "needs nginx, logrotate and root: rotation during an erase loses no line (cargo test --release -- --ignored)"]
    fn rotation_during_an_erase_loses_no_line() {
        let dir = scratch("rotate");
        const OLD: usize = 100_000;
        let (mut nginx, port, logs) = start_nginx(&dir, OLD);
        let (d, l) = (dir.display(), logs.display());
        std::fs::write(
            dir.join("logrotate.conf"),
            format!(
                "{l}/access.log {l}/error.log {{\n  rotate 9\n  compress\n  delaycompress\n  missingok\n  \
                 create 0640 www-data adm\n  sharedscripts\n  postrotate\n    kill -USR1 `cat {d}/nginx.pid`\n  endscript\n}}\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(dir.join("logrotate.conf"), std::fs::Permissions::from_mode(0o644)).unwrap();
        let clients = Clients::start(port);

        let rotating = {
            let dir = dir.clone();
            std::thread::spawn(move || {
                let mut rotations = 0;
                for _ in 0..6 {
                    std::thread::sleep(Duration::from_millis(700));
                    let ran = std::process::Command::new("logrotate")
                        .arg("-f")
                        .arg("-s")
                        .arg(dir.join("logrotate.state"))
                        .arg(dir.join("logrotate.conf"))
                        .output()
                        .expect("logrotate is not installed");
                    assert!(ran.status.success(), "logrotate: {}", String::from_utf8_lossy(&ran.stderr));
                    rotations += 1;
                }
                rotations
            })
        };
        let s = ip("127.0.0.1");
        let (mut erases, mut said_rotated) = (0, 0);
        while !rotating.is_finished() {
            for id in ["nginx_access", "nginx_error"] {
                let out = erase_source(id, &s, &logs, Path::new(NO_DB));
                erases += 1;
                if !out.ok {
                    said_rotated += 1;
                    assert!(out.detail.contains("was rotated while it was being rewritten"), "{}", out.detail);
                }
                assert!(!out.detail.contains("had not reopened"), "{}", out.detail);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(rotating.join().unwrap(), 6);
        let (hits, missing) = clients.finish();
        quit(&mut nginx);
        eprintln!("{erases} erases, {said_rotated} of them found a file rotated under them; {} requests", hits.len() + missing.len());

        let access = all_text(&logs, false);
        assert!(ids_after(&access, "\"GET /hit/", ' ') == hits, "access log lines were lost or doubled");
        assert!(ids_after(&access, "\"GET /missing/", ' ') == missing, "access log lines were lost or doubled");
        for tag in ["old", "older", "oldest"] {
            assert_eq!(access.lines().filter(|l| l.contains(&format!("\"GET /{tag}/"))).count(), OLD, "{tag}");
        }
        let errors = all_text(&logs, true);
        assert!(ids_after(&errors, "open() \"/nonexistent/missing/", '"') == missing, "error log lines were lost or doubled");
        no_copies_left(&logs);
        // With nothing rotating any more, one more erase leaves no trace of the address.
        for id in ["nginx_access", "nginx_error"] {
            let out = erase_source(id, &s, &logs, Path::new(NO_DB));
            assert!(out.ok, "{}", out.detail);
        }
        assert!(!all_text(&logs, false).contains("127.0.0.1") && !all_text(&logs, true).contains("127.0.0.1"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// PRIVACY_TEST_JOURNAL names a journal directory that a real journald
    /// wrote (one started by hand in `unshare -m`, with a tmpfs on /run and
    /// the directory bound on /var/log/journal), fed with `systemd-cat -t`:
    ///
    ///   sshd           Failed password for root from 203.0.113.5 port 40022 ssh2
    ///   sshd           Failed password for root from 203.0.113.55 port 40023 ssh2
    ///   sshd           Disconnected from authenticating user root 203.0.113.5 port 40022 [preauth]
    ///   sshd           Failed password for root from 2001:db8::1 port 40022 ssh2
    ///   sshd           Failed password for root from 2001:db8::12 port 40022 ssh2
    ///   kernel         [UFW BLOCK] IN=eth0 OUT= SRC=2001:0db8:0000:0000:0000:0000:0000:0001 DST=2001:0db8:0000:0000:0000:0000:0000:0002 LEN=40
    ///   kernel         [UFW BLOCK] IN=eth0 OUT= SRC=203.0.113.5 DST=198.51.100.1 LEN=40 PROTO=TCP SPT=4 DPT=23
    ///   postfix/smtpd  connect from unknown[203.0.113.5]
    ///   postfix/smtpd  connect from unknown[203a0b113c5]
    ///   postfix/lmtp   4AB12C3D4E: to=<jane@example.com>, orig_to=<sales@example.com>, status=sent (250 2.0.0 <jane@example.com> Saved)
    ///   postfix/lmtp   4AB12C3D4F: to=<maryjane@example.com>, status=sent
    ///   postfix/lmtp   4AB12C3D50: to=<Jane+News@Example.com>, status=sent
    ///   postfix/lmtp   4AB12C3D51: to=<janeXexample.com>, status=sent
    ///   dovecot        imap-login: Login: user=<JANE@example.com>, method=PLAIN, rip=203.0.113.5, lip=198.51.100.1, TLS
    ///   myapp          GET /unsubscribe?email=jane%40example.com from 203.0.113.5
    ///   myapp          GET /unsubscribe?email=jane%2Bnews%40example.com
    ///   myapp          nothing to see
    #[tokio::test]
    #[ignore = "needs a journal directory in PRIVACY_TEST_JOURNAL: journalctl --grep with the escaped pattern"]
    async fn a_real_journal_is_searched_through_journalctl() {
        let dir = std::env::var("PRIVACY_TEST_JOURNAL").expect("PRIVACY_TEST_JOURNAL is not set");
        let limit = || tokio::time::Instant::now() + Duration::from_secs(20);
        let counts = |h: &JournalHits| (h.mail.count, h.ssh.count, h.other.count);

        // 203.0.113.55 and "203a0b113c5" are in the journal too.
        let hits = search_journal(&ip("203.0.113.5"), limit(), &["-D", &dir]).await;
        assert!(hits.problem.is_none() && !hits.truncated, "{:?}", hits.problem);
        assert_eq!(counts(&hits), (2, 2, 2));
        assert!(hits.ssh.samples[0].contains("Disconnected from authenticating user root 203.0.113.5 port"));
        // The compressed and the kernel's spelling; 2001:db8::12 is another address.
        let hits = search_journal(&ip("2001:db8::1"), limit(), &["-D", &dir]).await;
        assert_eq!(counts(&hits), (0, 1, 1), "{:?}", hits.problem);
        assert!(hits.other.samples[0].contains("SRC=2001:0db8:0000:0000:0000:0000:0000:0001"));
        // Any case, percent-encoded; not maryjane@, jane+news@ or janeXexample.com.
        let hits = search_journal(&email("jane@example.com"), limit(), &["-D", &dir]).await;
        assert_eq!(counts(&hits), (2, 0, 1), "{:?}", hits.problem);
        let hits = search_journal(&email("jane+news@example.com"), limit(), &["-D", &dir]).await;
        assert_eq!(counts(&hits), (1, 0, 1), "{:?}", hits.problem);
        // Nothing found is not an error.
        let hits = search_journal(&ip("198.18.0.1"), limit(), &["-D", &dir]).await;
        assert!(hits.problem.is_none() && !hits.truncated && counts(&hits) == (0, 0, 0), "{:?}", hits.problem);
        // A journal that is not there is a note on the sources.
        let hits = search_journal(&ip("203.0.113.5"), limit(), &["-D", "/nonexistent/journal"]).await;
        assert!(hits.truncated && hits.problem.as_deref().is_some_and(|p| p.starts_with("the journal could not be read: ")), "{:?}", hits.problem);
        let sources = journal_sources(&ip("203.0.113.5"), hits);
        assert!(sources.iter().all(|s| s.count == 0 && s.truncated && s.note.contains("The journal could not be read: ")));
        // The deadline.
        let hits = search_journal(&ip("203.0.113.5"), tokio::time::Instant::now(), &["-D", &dir]).await;
        assert!(hits.truncated && hits.problem.as_deref().is_some_and(|p| p.contains("time limit")), "{:?}", hits.problem);
    }

    /// PRIVACY_TEST_QUEUE: Postfix's queue holds a message from
    /// jane@example.com to bob@slow.example (sendmail -f, Postfix stopped).
    #[tokio::test]
    #[ignore = "needs a queued message and PRIVACY_TEST_QUEUE=1: the real postqueue -j"]
    async fn the_real_mail_queue_is_searched_through_postqueue() {
        std::env::var("PRIVACY_TEST_QUEUE").expect("PRIVACY_TEST_QUEUE is not set");
        let s = search_mail_queue(&email("jane@example.com")).await;
        assert!(s.count >= 1 && !s.truncated, "{s:?}");
        assert!(s.samples[0].contains("from Jane@Example.com to bob@slow.example"), "{:?}", s.samples);
        assert!(!s.note.contains("could not be read"), "{}", s.note);
        assert_eq!(search_mail_queue(&email("bob@slow.example")).await.count, s.count);
        assert_eq!(search_mail_queue(&email("ob@slow.example")).await.count, 0);
    }

    /// PRIVACY_TEST_MARIADB_URL: a MariaDB that is only this test's (a
    /// scratch data directory and socket). The `mail` database is created
    /// the way the agent creates it.
    #[tokio::test]
    #[ignore = "needs a scratch MariaDB in PRIVACY_TEST_MARIADB_URL: the mailbox and forwarder rows"]
    async fn mailboxes_and_forwarders_are_found_in_a_real_mail_database() {
        let url = std::env::var("PRIVACY_TEST_MARIADB_URL").expect("PRIVACY_TEST_MARIADB_URL is not set");
        let dir = scratch("maria");
        let mut state = state(&dir);
        state.mariadb = crate::db::MariaDb::connect(&url).unwrap();
        let mut conn = mail_conn(&state).await.unwrap();
        conn.query_drop("DELETE FROM virtual_domains").await.unwrap();
        conn.query_drop("INSERT INTO virtual_domains (id, name) VALUES (1, 'example.com')").await.unwrap();
        conn.query_drop(
            "INSERT INTO virtual_users (domain_id, email, password, quota_mb) VALUES \
             (1, 'jane@example.com', 'x', 2048), (1, 'maryjane@example.com', 'x', 1024)",
        )
        .await
        .unwrap();
        conn.query_drop(
            "INSERT INTO virtual_aliases (domain_id, source, destination) VALUES \
             (1, 'sales@example.com', 'jane@example.com'), (1, 'jane@example.com', 'jane@gmail.example'), \
             (1, 'sales@example.com', 'maryjane@example.com'), (1, '@example.com', 'jane@example.com')",
        )
        .await
        .unwrap();
        drop(conn);

        let (boxes, fwd) = search_mail_rows(&state, &email("Jane@Example.com")).await;
        assert_eq!((boxes.count, boxes.truncated, boxes.erasable), (1, false, false), "{}", boxes.note);
        assert_eq!(boxes.samples, ["jane@example.com (quota 2048 MB)"]);
        assert_eq!((fwd.count, fwd.truncated), (3, false), "{}", fwd.note);
        assert_eq!(
            fwd.samples,
            ["@example.com -> jane@example.com", "jane@example.com -> jane@gmail.example", "sales@example.com -> jane@example.com"]
        );
        // A forwarder's target that has no mailbox here.
        let (boxes, fwd) = search_mail_rows(&state, &email("jane@gmail.example")).await;
        assert_eq!((boxes.count, fwd.count), (0, 1));
        // SQL wildcards and quotes in an address are only characters.
        for odd in ["%@example.com", "_ane@example.com", "x'or'1'='1@example.com"] {
            let (boxes, fwd) = search_mail_rows(&state, &email(odd)).await;
            assert_eq!((boxes.count, fwd.count, boxes.truncated), (0, 0, false), "{odd}");
        }
        // The whole report, with the database there.
        let Json(report) = search(
            State(state.clone()),
            Json(SubjectReq { kind: "email".into(), value: "jane@example.com".into() }),
        )
        .await
        .unwrap();
        let by_id = |id: &str| report.sources.iter().find(|s| s.id == id).unwrap().count;
        assert_eq!((by_id("mailboxes"), by_id("forwarders")), (1, 3));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
