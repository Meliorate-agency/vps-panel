use crate::error::{AgentError, AgentResult};
use crate::exec;
use crate::fs_atomic;
use crate::ipc::OkResponse;
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    Json,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize)]
pub struct FirewallStatus {
    pub ufw_enabled: bool,
    pub rules: Vec<String>,
    pub listening_ports: Vec<ListeningPort>,
    pub fail2ban_active: bool,
}

#[derive(Serialize)]
pub struct ListeningPort {
    pub proto: String,
    pub address: String,
    pub port: u16,
    pub process: String,
}

#[derive(Deserialize)]
pub struct PortReq {
    pub port: u16,
    #[serde(default = "default_proto")]
    pub proto: String,
    /// Deny the port although closing it can cut the operator off.
    #[serde(default)]
    pub force: bool,
}

fn default_proto() -> String {
    "tcp".into()
}

#[derive(Deserialize)]
pub struct UnbanReq {
    pub jail: String,
    pub ip: String,
}

#[derive(Deserialize)]
pub struct UnbanAllReq {
    pub jail: String,
}

#[derive(Serialize)]
pub struct JailRow {
    pub name: String,
    pub banned: Vec<String>,
    pub total_failed: i64,
}

#[derive(Serialize)]
pub struct FailedByIp {
    pub ip: String,
    pub attempts: u32,
    pub last_seen: String,
}

pub async fn status() -> AgentResult<Json<FirewallStatus>> {
    let ufw_status = exec::run("ufw", ["status", "verbose"]).await.unwrap_or_default();
    let ufw_enabled = ufw_status.stdout.contains("Status: active");
    // `ufw show added` lists every queued rule even when ufw is inactive,
    // so a freshly added port always appears here. `ufw status verbose`
    // only shows rules when ufw is active.
    let added_out = exec::run("ufw", ["show", "added"]).await.unwrap_or_default();
    let rules: Vec<String> = added_out
        .stdout
        .lines()
        .filter(|l| l.starts_with("ufw "))
        .map(|s| s.to_string())
        .collect();

    let listening_ports = list_listening_ports().await;

    let active = exec::run("systemctl", ["is-active", "fail2ban"]).await.is_ok();
    Ok(Json(FirewallStatus {
        ufw_enabled,
        rules,
        listening_ports,
        fail2ban_active: active,
    }))
}

async fn list_listening_ports() -> Vec<ListeningPort> {
    let Ok(out) = exec::run("ss", ["-tulpnHl"]).await else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for line in out.stdout.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 5 {
            continue;
        }
        let proto = match cols[0] {
            "tcp" | "tcp6" => "tcp",
            "udp" | "udp6" => "udp",
            _ => continue,
        };
        let local = cols[4];
        let Some((address, port_str)) = split_addr_port(local) else {
            continue;
        };
        let Ok(port) = port_str.parse::<u16>() else {
            continue;
        };
        let process = cols
            .get(6)
            .map(|s| extract_process_name(s))
            .unwrap_or_default();
        let key = (proto.to_string(), address.clone(), port);
        if !seen.insert(key) {
            continue;
        }
        rows.push(ListeningPort {
            proto: proto.to_string(),
            address,
            port,
            process,
        });
    }
    rows.sort_by_key(|r| (r.port, r.proto.clone(), r.address.clone()));
    rows
}

fn split_addr_port(local: &str) -> Option<(String, String)> {
    let idx = local.rfind(':')?;
    let addr = local[..idx].trim_matches(|c| c == '[' || c == ']');
    let port = &local[idx + 1..];
    Some((addr.to_string(), port.to_string()))
}

fn extract_process_name(s: &str) -> String {
    let Some(start) = s.find("((\"") else {
        return String::new();
    };
    let rest = &s[start + 3..];
    let Some(end) = rest.find('"') else {
        return String::new();
    };
    rest[..end].to_string()
}

pub async fn allow(Json(req): Json<PortReq>) -> AgentResult<Json<OkResponse>> {
    validate_proto(&req.proto)?;
    exec::run("ufw", ["allow", &format!("{}/{}", req.port, req.proto)]).await?;
    Ok(Json(OkResponse::default()))
}

pub async fn enable_ufw() -> AgentResult<Json<OkResponse>> {
    // Allow SSH first so enabling ufw can never lock the user out, on
    // whatever port sshd really listens on (22 was hard-coded here).
    for port in crate::handlers::ssh::effective_ports().await {
        let _ = exec::run("ufw", ["allow", &format!("{port}/tcp")]).await;
    }
    exec::run("ufw", ["--force", "enable"]).await?;
    Ok(Json(OkResponse::default()))
}

/// What closing `port` would cut the operator off from: SSH, or the web ports
/// the panel and every site are served on. `ssh_ports` are the ports sshd
/// listens on.
fn deny_lockout(port: u16, proto: &str, ssh_ports: &[u16]) -> Option<String> {
    if proto != "tcp" {
        return None;
    }
    let what = if ssh_ports.contains(&port) {
        "is the SSH port. Denying it locks you out of SSH."
    } else {
        match port {
            80 => {
                "is HTTP: the panel, every site and certificate renewal use it. \
                 Denying it cuts them off."
            }
            443 => {
                "is HTTPS: the panel and every site use it. \
                 Denying it cuts them off, this panel included."
            }
            _ => return None,
        }
    };
    Some(format!("port {port}/tcp {what}"))
}

pub async fn deny(Json(req): Json<PortReq>) -> AgentResult<Json<OkResponse>> {
    validate_proto(&req.proto)?;
    // `ufw deny` replaces an allow rule for the same port, so this is how a
    // port gets closed. It went straight through for the SSH port and for
    // 80 / 443; now that takes `force`.
    if !req.force {
        let ssh_ports = crate::handlers::ssh::effective_ports().await;
        if let Some(why) = deny_lockout(req.port, &req.proto, &ssh_ports) {
            return Err(AgentError::Conflict(why));
        }
    }
    exec::run("ufw", ["deny", &format!("{}/{}", req.port, req.proto)]).await?;
    Ok(Json(OkResponse::default()))
}

pub async fn jails() -> AgentResult<Json<Vec<JailRow>>> {
    let out = exec::run("fail2ban-client", ["status"]).await?;
    let mut rows = Vec::new();
    for jail in parse_jail_list(&out.stdout) {
        let detail = match exec::run("fail2ban-client", ["status", jail]).await {
            Ok(o) => o.stdout,
            Err(_) => continue,
        };
        let banned = parse_field_list(&detail, "Banned IP list:");
        let total_failed = parse_field_int(&detail, "Total failed:");
        rows.push(JailRow {
            name: jail.to_string(),
            banned,
            total_failed,
        });
    }
    Ok(Json(rows))
}

/// The jails fail2ban runs, from the `Jail list:` line of
/// `fail2ban-client status`.
fn parse_jail_list(status: &str) -> Vec<&str> {
    status
        .lines()
        .find(|l| l.contains("Jail list:"))
        .and_then(|l| l.split(':').nth(1))
        .map(|s| s.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default()
}

/// `fail2ban-client set <jail> ...` is only sent for a jail fail2ban runs.
/// `set logtarget <target>`, `set dbfile <file>` and `set loglevel <level>`
/// are commands of their own, so with one of those names in the jail's place
/// an unban changed that setting of the server (its log went to a file
/// called `unbanip`).
async fn require_running_jail(jail: &str) -> AgentResult<()> {
    validate_jail(jail)?;
    let out = exec::run("fail2ban-client", ["status"]).await?;
    if !parse_jail_list(&out.stdout).contains(&jail) {
        return Err(AgentError::Invalid(format!("fail2ban has no jail named {jail}")));
    }
    Ok(())
}

pub async fn unban(Json(req): Json<UnbanReq>) -> AgentResult<Json<OkResponse>> {
    require_running_jail(&req.jail).await?;
    if !is_valid_ip(&req.ip) {
        return Err(AgentError::Invalid("invalid ip".into()));
    }
    exec::run("fail2ban-client", ["set", &req.jail, "unbanip", &req.ip]).await?;
    Ok(Json(OkResponse::default()))
}

/// Unban every IP in the given jail in one shot, then restart fail2ban so the
/// in-memory state is fresh. `fail2ban-client set <jail> unbanip --all` clears
/// both the runtime ban list and the persisted database for that jail; the
/// subsequent restart guarantees no stale state in any other in-memory cache.
///
/// Destructive: confirm in the UI before calling. Useful as an "I've fixed
/// the misconfigured client, let everyone retry" panic button after an iOS
/// retry storm bans the operator's home / phone IPs.
pub async fn unban_all(Json(req): Json<UnbanAllReq>) -> AgentResult<Json<OkResponse>> {
    require_running_jail(&req.jail).await?;
    exec::run(
        "fail2ban-client",
        ["set", &req.jail, "unbanip", "--all"],
    )
    .await?;
    // Restart (not reload) so any in-memory failure counter resets too.
    // Reload would clear the bans but keep the per-IP "Found" counters that
    // can re-trigger a ban in seconds. Restart wipes the slate.
    exec::run("systemctl", ["restart", "fail2ban"]).await?;
    Ok(Json(OkResponse::default()))
}

/// Per-IP failure breakdown for a given jail, derived by parsing
/// `/var/log/fail2ban.log` for `[<jail>] Found <ip>` entries. Returns the
/// distinct source IPs sorted by attempt count (descending), with the most
/// recent timestamp each was seen at.
///
/// Useful for the operator to see *who* is contributing to a jail's total
/// failed count without having to grep the log by hand. Capped at 100 rows
/// to keep the response bounded on a server under heavy brute-force.
pub async fn failed_by_ip(Path(jail): Path<String>) -> AgentResult<Json<Vec<FailedByIp>>> {
    validate_jail(&jail)?;
    let mut content = String::new();
    for path in ["/var/log/fail2ban.log", "/var/log/fail2ban.log.1"] {
        if let Ok(s) = tokio::fs::read_to_string(path).await {
            content.push_str(&s);
            content.push('\n');
        }
    }
    let needle = format!("[{jail}] Found ");
    let mut counts: HashMap<String, (u32, String)> = HashMap::new();
    for line in content.lines() {
        let Some(idx) = line.find(&needle) else {
            continue;
        };
        let rest = &line[idx + needle.len()..];
        // Lines look like `... [dovecot] Found 203.0.113.5 - 2026-05-24 13:27:45`
        // — strip after the IP. fail2ban-client status reports per-IP attempts
        // through `Found` matches, which is what builds toward the ban threshold.
        let ip = rest
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        if ip.is_empty() || !is_valid_ip(&ip) {
            continue;
        }
        // Timestamp is the first 19 chars of the log line: `YYYY-MM-DD HH:MM:SS`.
        let ts = line.get(0..19).unwrap_or("").to_string();
        let entry = counts.entry(ip).or_insert((0, ts.clone()));
        entry.0 += 1;
        // Keep the latest timestamp; since the log is append-only and read
        // top-to-bottom, every iteration is at least as new as the previous.
        if ts > entry.1 {
            entry.1 = ts;
        }
    }
    let mut rows: Vec<FailedByIp> = counts
        .into_iter()
        .map(|(ip, (attempts, last_seen))| FailedByIp { ip, attempts, last_seen })
        .collect();
    rows.sort_by(|a, b| b.attempts.cmp(&a.attempts).then_with(|| b.last_seen.cmp(&a.last_seen)));
    rows.truncate(100);
    Ok(Json(rows))
}

const OPERATOR_IGNOREIP_PATH: &str = "/etc/fail2ban/jail.d/zz-vps-panel-operator.conf";

// Wizard policy file: sorts AFTER zz-vps-panel.conf (the base install file) and
// AFTER zz-vps-panel-operator.conf (the ignoreip mirror), so fail2ban applies it
// last and the operator's choices win for [DEFAULT] thresholds and which jails
// are enabled. The triple-z prefix is deliberate: '-' (0x2D) sorts before '.'
// (0x2E), so `zz-vps-panel-policy.conf` would be loaded BEFORE `zz-vps-panel.conf`
// and the base file would override. `zzz-` is the simplest fix that keeps the
// install.sh filename unchanged.
const POLICY_PATH: &str = "/etc/fail2ban/jail.d/zzz-vps-panel-policy.conf";

// The jails the wizard surfaces as toggleable. install.sh's base file enables
// all of these by default; the wizard's policy file writes an explicit
// enabled=true|false for every one in this list so unchecking a jail in the UI
// actually disables it. Keeps the operator's choice meaningful.
const KNOWN_JAILS: &[&str] = &["sshd", "postfix-sasl", "dovecot"];

/// Jails the panel used to enable that can never ban anyone: nothing in its
/// nginx uses basic auth or limit_req, and nginx logs to files the systemd
/// backend never reads. Never written, even when an old saved policy lists
/// them.
const RETIRED_JAILS: &[&str] = &["nginx-http-auth", "nginx-limit-req"];

#[derive(Deserialize)]
pub struct Fail2banPolicyReq {
    pub bantime: String,
    pub findtime: String,
    pub maxretry: u32,
    pub jails: Vec<String>,
}

/// Writes the operator's fail2ban policy choices to a jail.d override file and
/// reloads fail2ban. Idempotent. Sorts after the install.sh base file so it
/// wins on conflicts; preserves the dovecot per-jail safety overrides shipped
/// with the base config (this file does NOT set `[dovecot]`).
pub async fn set_fail2ban_policy(
    Json(req): Json<Fail2banPolicyReq>,
) -> AgentResult<Json<OkResponse>> {
    let body = policy_body(&req)?;
    fs_atomic::write(std::path::Path::new(POLICY_PATH), body.as_bytes(), 0o644).await?;
    // Reload (not restart) so existing bans persist. Best-effort: if fail2ban
    // isn't running yet (fresh install), the file is on disk for the next start.
    let _ = exec::run("systemctl", ["reload", "fail2ban"]).await;
    Ok(Json(OkResponse::default()))
}

/// The policy file for `req`, or why it is refused. A jail name becomes a
/// `[section]` with `enabled = true`, so only the jails the panel knows are
/// taken: `DEFAULT` there switched on every jail fail2ban ships, and
/// `INCLUDES` is special to its config parser too.
fn policy_body(req: &Fail2banPolicyReq) -> AgentResult<String> {
    validate_duration(&req.bantime, "bantime")?;
    validate_duration(&req.findtime, "findtime")?;
    if !(1..=1000).contains(&req.maxretry) {
        return Err(AgentError::Invalid("maxretry must be 1..=1000".into()));
    }
    for j in &req.jails {
        if !KNOWN_JAILS.contains(&j.as_str()) && !RETIRED_JAILS.contains(&j.as_str()) {
            return Err(AgentError::Invalid(format!(
                "unknown jail {j:?}; the panel manages {}",
                KNOWN_JAILS.join(", ")
            )));
        }
    }

    let mut body = String::new();
    body.push_str(
        "# vps-panel operator policy.\n\
         # Written by the first-run setup wizard / Settings page. Edits here will\n\
         # be overwritten on the next save from the panel UI.\n\
         #\n\
         # Sorts after zz-vps-panel.conf (base) and zz-vps-panel-operator.conf\n\
         # (ignoreip mirror) so these [DEFAULT] values win over the base file's\n\
         # defaults but do not clobber the dovecot per-jail overrides.\n",
    );
    body.push_str("\n[DEFAULT]\n");
    body.push_str(&format!("bantime = {}\n", req.bantime));
    body.push_str(&format!("findtime = {}\n", req.findtime));
    body.push_str(&format!("maxretry = {}\n", req.maxretry));

    // Explicit enabled=true|false for every known jail so unchecking a box in
    // the wizard actually turns the jail off.
    for jail in KNOWN_JAILS {
        let on = req.jails.iter().any(|j| j == jail);
        body.push_str(&format!("\n[{jail}]\nenabled = {}\n", if on { "true" } else { "false" }));
    }
    Ok(body)
}

fn validate_duration(s: &str, label: &str) -> AgentResult<()> {
    // fail2ban accepts seconds (int) or with unit suffix: s/m/h/d/w. Examples:
    // "600", "10m", "1h", "2d". Reject anything else to keep the conf parseable.
    if s.is_empty() || s.len() > 16 {
        return Err(AgentError::Invalid(format!("{label} must be 1..=16 chars")));
    }
    let (num, unit) = match s.chars().last() {
        Some(c) if c.is_ascii_alphabetic() => (&s[..s.len() - 1], Some(c)),
        _ => (s, None),
    };
    if num.is_empty() || !num.chars().all(|c| c.is_ascii_digit()) {
        return Err(AgentError::Invalid(format!("{label} must be a positive integer (with optional s/m/h/d/w suffix)")));
    }
    if let Some(u) = unit {
        if !matches!(u, 's' | 'm' | 'h' | 'd' | 'w') {
            return Err(AgentError::Invalid(format!(
                "{label} suffix must be one of s, m, h, d, w"
            )));
        }
    }
    Ok(())
}

/// Reads `panel.ip_allowlist` from the panel SQLite DB and mirrors those IPs
/// into the dovecot jail's `ignoreip` so the operator can never lock themselves
/// out by misconfiguring a mail client during setup.
///
/// Writes `/etc/fail2ban/jail.d/zz-vps-panel-operator.conf` and reloads
/// fail2ban. Idempotent — safe to call on every settings.ip_allowlist write.
///
/// The override targets the `[dovecot]` jail (not `[DEFAULT]`) because
/// fail2ban resolves jail-specific `ignoreip` over the default — a `[DEFAULT]`
/// override in a separate jail.d file does NOT merge into an already-set
/// dovecot ignoreip from another file. Confirmed empirically in the field.
///
/// Best-effort on fail2ban reload: if the daemon isn't installed or is down,
/// the file write still wins and the next fail2ban start picks it up.
pub async fn sync_operator_ignoreip(
    State(state): State<AppState>,
) -> AgentResult<Json<OkResponse>> {
    let mut ignoreip = String::from("127.0.0.0/8 ::1 10.0.0.0/8 172.16.0.0/12 192.168.0.0/16");
    for entry in read_operator_allowlist(&state.panel_db) {
        ignoreip.push(' ');
        ignoreip.push_str(&entry);
    }
    let body = format!(
        "[dovecot]\n\
         # Operator's panel.ip_allowlist mirrored from panel.db. Regenerated by\n\
         # the agent whenever the panel writes the setting. Prevents the operator\n\
         # from locking themselves out via fail2ban while debugging mail clients.\n\
         # Targets the [dovecot] jail (not [DEFAULT]) because fail2ban's jail-\n\
         # specific ignoreip supersedes the default rather than merging.\n\
         ignoreip = {ignoreip}\n"
    );
    fs_atomic::write(
        std::path::Path::new(OPERATOR_IGNOREIP_PATH),
        body.as_bytes(),
        0o644,
    )
    .await?;
    // Reload (not restart) so existing in-memory bans persist. If fail2ban
    // isn't installed/running, swallow the error — the file is on disk and
    // will be picked up next time fail2ban starts.
    let _ = exec::run("systemctl", ["reload", "fail2ban"]).await;
    Ok(Json(OkResponse::default()))
}

fn read_operator_allowlist(db: &crate::db::PanelDb) -> Vec<String> {
    // Mirrors the pattern used by ssl::resolve_acme_email — read a single
    // settings row, treat "row missing" as "empty value".
    let raw: String = db
        .with(|conn| {
            conn.query_row(
                "SELECT value FROM settings WHERE key = 'panel.ip_allowlist'",
                [],
                |row| row.get::<_, String>(0),
            )
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(String::new()),
                other => Err(other),
            })
        })
        .ok()
        .unwrap_or_default();
    allowlist_entries(&raw)
}

/// The entries of a stored `panel.ip_allowlist` that are an IP address or a
/// CIDR range, normalized. The rest is left out and logged: the list went
/// into `ignoreip` as stored, and the panel checking it on save does not
/// cover a row saved before it did, or anything else that writes panel.db.
fn allowlist_entries(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for entry in raw
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
    {
        match crate::handlers::panel_access::parse_entry(entry) {
            Some(e) => {
                if !out.contains(&e) {
                    out.push(e);
                }
            }
            None => tracing::warn!(
                ?entry,
                "panel.ip_allowlist entry is not an IP address or CIDR range; \
                 left out of fail2ban's ignoreip"
            ),
        }
    }
    out
}

fn validate_proto(s: &str) -> AgentResult<()> {
    if matches!(s, "tcp" | "udp") {
        Ok(())
    } else {
        Err(AgentError::Invalid("proto must be tcp or udp".into()))
    }
}

fn validate_jail(s: &str) -> AgentResult<()> {
    let re = regex::Regex::new(r"^[a-zA-Z0-9_.-]{1,64}$").unwrap();
    if !re.is_match(s) {
        return Err(AgentError::Invalid("invalid jail name".into()));
    }
    Ok(())
}

fn is_valid_ip(s: &str) -> bool {
    s.parse::<std::net::IpAddr>().is_ok()
}

/// The values after `key` on its line. Everything after the key, not the
/// text up to the next colon: that cut a banned IPv6 address down to its
/// first group (`2001`), which showed in the list and could not be unbanned.
fn parse_field_list(content: &str, key: &str) -> Vec<String> {
    content
        .lines()
        .find_map(|l| l.split_once(key))
        .map(|(_, s)| {
            s.split_whitespace()
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn parse_field_int(content: &str, key: &str) -> i64 {
    content
        .lines()
        .find(|l| l.contains(key))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{parse_field_int, parse_field_list};

    #[test]
    fn denying_the_ssh_or_web_ports_needs_force() {
        use super::{deny_lockout, PortReq};
        // sshd moved to 2222: that port is guarded, 22 no longer is.
        let ssh = [2222];
        assert!(deny_lockout(2222, "tcp", &ssh).unwrap().contains("SSH"));
        assert!(deny_lockout(22, "tcp", &ssh).is_none());
        assert!(deny_lockout(80, "tcp", &ssh).unwrap().contains("HTTP"));
        assert!(deny_lockout(443, "tcp", &ssh).unwrap().contains("HTTPS"));
        // Nothing listens for these over udp.
        assert!(deny_lockout(2222, "udp", &ssh).is_none());
        assert!(deny_lockout(443, "udp", &ssh).is_none());
        assert!(deny_lockout(3306, "tcp", &ssh).is_none());

        let req: PortReq = serde_json::from_str(r#"{"port":22}"#).unwrap();
        assert!(!req.force && req.proto == "tcp");
        let req: PortReq = serde_json::from_str(r#"{"port":22,"proto":"tcp","force":true}"#).unwrap();
        assert!(req.force);
    }

    fn policy(jails: &[&str]) -> super::Fail2banPolicyReq {
        super::Fail2banPolicyReq {
            bantime: "1h".into(),
            findtime: "10m".into(),
            maxretry: 5,
            jails: jails.iter().map(|j| j.to_string()).collect(),
        }
    }

    #[test]
    fn policy_only_takes_the_jails_the_panel_knows() {
        use super::policy_body;
        let body = policy_body(&policy(&["sshd", "dovecot"])).unwrap();
        assert!(body.contains("\n[sshd]\nenabled = true\n"));
        assert!(body.contains("\n[postfix-sasl]\nenabled = false\n"));
        assert!(body.contains("\n[dovecot]\nenabled = true\n"));
        // The only [DEFAULT] is the thresholds; it never gets `enabled`.
        assert_eq!(body.matches("[DEFAULT]").count(), 2);
        assert_eq!(body.matches("enabled").count(), 3);

        for bad in ["DEFAULT", "INCLUDES", "default", "recidive", "sshd]\n[DEFAULT", ""] {
            assert!(policy_body(&policy(&["sshd", bad])).is_err(), "{bad:?}");
        }
        // A retired jail in an old saved policy is dropped, not refused.
        let body = policy_body(&policy(&["sshd", "nginx-http-auth"])).unwrap();
        assert!(!body.contains("nginx"));
    }

    #[test]
    fn running_jails_from_the_status_output() {
        use super::parse_jail_list;
        let status = "Status\n|- Number of jail:\t3\n`- Jail list:\tdovecot, postfix-sasl, sshd\n";
        assert_eq!(parse_jail_list(status), vec!["dovecot", "postfix-sasl", "sshd"]);
        // The names an unban must not be sent for are never in it.
        assert!(!parse_jail_list(status).contains(&"logtarget"));
        assert!(parse_jail_list("Status\n|- Number of jail:\t0\n`- Jail list:\t\n").is_empty());
        assert!(parse_jail_list("").is_empty());
    }

    #[test]
    fn trusted_ips_that_are_not_addresses_stay_out_of_ignoreip() {
        use super::allowlist_entries;
        assert_eq!(
            allowlist_entries("203.0.113.7, 10.1.0.0/16,2001:DB8::/32"),
            vec!["203.0.113.7", "10.1.0.0/16", "2001:db8::/32"]
        );
        assert_eq!(
            allowlist_entries("203.0.113.7,\nexample.com, 198.51.100.0/24 [sshd], 1.2.3.4/33, 203.0.113.7"),
            vec!["203.0.113.7", "198.51.100.0/24"]
        );
        assert!(allowlist_entries("").is_empty());
        assert!(allowlist_entries(" , ").is_empty());
    }

    const JAIL_STATUS: &str = "Status for the jail: sshd\n\
        |- Filter\n\
        |  |- Currently failed:\t1\n\
        |  |- Total failed:\t42\n\
        |  `- Journal matches:\t_SYSTEMD_UNIT=ssh.service + _COMM=sshd\n\
        `- Actions\n\
        \x20  |- Currently banned:\t2\n\
        \x20  |- Total banned:\t5\n\
        \x20  `- Banned IP list:\t203.0.113.7 2001:db8::1\n";

    #[test]
    fn banned_list_keeps_ipv6_addresses_whole() {
        assert_eq!(
            parse_field_list(JAIL_STATUS, "Banned IP list:"),
            vec!["203.0.113.7".to_string(), "2001:db8::1".to_string()]
        );
        assert_eq!(parse_field_int(JAIL_STATUS, "Total failed:"), 42);
        assert!(parse_field_list("no such line\n", "Banned IP list:").is_empty());
    }
}
