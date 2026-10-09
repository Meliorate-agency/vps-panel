use crate::error::{AgentError, AgentResult};
use crate::exec;
use crate::fs_atomic;
use crate::ipc::OkResponse;
use axum::{
    extract::{Path, Query},
    Json,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::fs;

const SSHD_CONFIG_PATH: &str = "/etc/ssh/sshd_config";
/// Every SSH server setting the panel changes lives in this drop-in. Ubuntu's
/// sshd_config starts with `Include /etc/ssh/sshd_config.d/*.conf` and sshd
/// keeps the first value it reads for a directive, so a file that sorts first
/// wins over the cloud-image drop-ins (`50-cloud-init.conf`,
/// `60-cloudimg-settings.conf`) and over sshd_config itself. The panel used
/// to edit sshd_config, which lost to those drop-ins: the toggle said
/// password login was off while a drop-in kept it on.
const PANEL_DROPIN_PATH: &str = "/etc/ssh/sshd_config.d/00-vps-panel.conf";
const DROPIN_HEADER: &str = "# Managed by vps-panel (Settings > SSH server). Sorts first in\n\
                             # sshd_config.d, so these values win over the other files.\n";
/// fail2ban's sshd jail only bans on port `ssh` (22) unless told otherwise.
const SSHD_JAIL_PORT_PATH: &str = "/etc/fail2ban/jail.d/zzz-vps-panel-sshd-port.conf";
const ROOT_LOGIN_MODES: [&str; 4] = ["yes", "prohibit-password", "forced-commands-only", "no"];

#[derive(Serialize)]
pub struct SshServerConfig {
    /// First port sshd listens on.
    pub port: u16,
    /// Minutes without traffic before sshd closes a session
    /// (`ChannelTimeout session=`); 0 = never.
    pub idle_timeout_minutes: u32,
    /// `PasswordAuthentication`.
    pub password_auth: bool,
    /// `PermitRootLogin`: yes | prohibit-password | forced-commands-only | no.
    pub permit_root_login: String,
}

#[derive(Deserialize)]
pub struct SshServerConfigUpdate {
    pub port: Option<u16>,
    pub idle_timeout_minutes: Option<u32>,
    pub password_auth: Option<bool>,
    pub permit_root_login: Option<String>,
}

/// The settings sshd actually runs with.
struct Effective {
    ports: Vec<u16>,
    idle_timeout_minutes: u32,
    password_auth: bool,
    kbd_interactive: bool,
    permit_root_login: String,
}

pub async fn config_get() -> AgentResult<Json<SshServerConfig>> {
    let e = effective_config().await?;
    Ok(Json(SshServerConfig {
        port: e.ports.first().copied().unwrap_or(22),
        idle_timeout_minutes: e.idle_timeout_minutes,
        password_auth: e.password_auth,
        permit_root_login: e.permit_root_login,
    }))
}

/// Ports sshd listens on, for callers that must keep SSH reachable (enabling
/// UFW). Falls back to 22 when the config cannot be read.
pub(crate) async fn effective_ports() -> Vec<u16> {
    match effective_config().await {
        Ok(e) if !e.ports.is_empty() => e.ports,
        _ => vec![22],
    }
}

/// `sshd -T` prints the merged config after every Include, so drop-ins are
/// accounted for. Reading sshd_config alone showed values a drop-in had
/// overridden.
async fn effective_config() -> AgentResult<Effective> {
    // sshd will not parse its config without its privilege separation
    // directory, which systemd only creates while ssh.service runs. With
    // socket activation that is not until the first connection.
    let _ = fs_atomic::ensure_dir(std::path::Path::new("/run/sshd"), 0o755).await;
    let out = exec::run("sshd", ["-T"])
        .await
        .map_err(|e| AgentError::Invalid(format!("could not read the sshd config (sshd -T): {e}")))?;
    Ok(parse_effective(&out.stdout))
}

fn dump_values<'a>(dump: &'a str, key: &str) -> Vec<&'a str> {
    dump.lines()
        .filter_map(|l| l.split_once(' '))
        .filter(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v.trim())
        .collect()
}

fn parse_effective(dump: &str) -> Effective {
    let first = |key: &str| {
        dump_values(dump, key)
            .first()
            .map(|v| v.to_ascii_lowercase())
    };
    let permit_root_login = match first("permitrootlogin").as_deref() {
        // sshd -T prints the older alias of prohibit-password.
        None | Some("without-password") => "prohibit-password".to_string(),
        Some(v) => v.to_string(),
    };
    Effective {
        ports: dump_values(dump, "port")
            .iter()
            .filter_map(|v| v.parse().ok())
            .collect(),
        idle_timeout_minutes: session_timeout_minutes(&dump_values(dump, "channeltimeout")),
        password_auth: first("passwordauthentication").as_deref() != Some("no"),
        kbd_interactive: first("kbdinteractiveauthentication").as_deref() != Some("no"),
        permit_root_login,
    }
}

/// Minutes from a `channeltimeout` value such as `session=30m`; 0 when no
/// timeout covers interactive sessions.
fn session_timeout_minutes(values: &[&str]) -> u32 {
    values
        .iter()
        .flat_map(|v| v.split_whitespace())
        .filter_map(|entry| entry.split_once('='))
        .find(|(kind, _)| *kind == "session" || *kind == "*")
        .and_then(|(_, t)| parse_sshd_time(t))
        .map(|secs| u32::try_from(secs.saturating_add(59) / 60).unwrap_or(u32::MAX))
        .unwrap_or(0)
}

/// sshd's TIME FORMAT: `90`, `30m`, `1h30m` (s, m, h, d, w; bare = seconds).
fn parse_sshd_time(s: &str) -> Option<u64> {
    let mut total: u64 = 0;
    let mut num = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        let n: u64 = num.parse().ok()?;
        num.clear();
        let unit = match c.to_ascii_lowercase() {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            'd' => 86_400,
            'w' => 604_800,
            _ => return None,
        };
        total = total.checked_add(n.checked_mul(unit)?)?;
    }
    if !num.is_empty() {
        total = total.checked_add(num.parse().ok()?)?;
    }
    Some(total)
}

/// Set `name` to `value` in the panel drop-in (one line, replacing earlier
/// ones), or drop it when `value` is None.
fn set_directive(body: &str, name: &str, value: Option<&str>) -> String {
    let mut out = String::with_capacity(body.len() + 64);
    let mut written = false;
    for line in body.lines() {
        let key = line.split_whitespace().next().unwrap_or("");
        if key.eq_ignore_ascii_case(name) {
            if let (Some(v), false) = (value, written) {
                out.push_str(&format!("{name} {v}\n"));
                written = true;
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if let (Some(v), false) = (value, written) {
        out.push_str(&format!("{name} {v}\n"));
    }
    out
}

/// Comment out every active `name` line. sshd listens on every `Port` it
/// reads, so a leftover line would keep the old port open next to the new
/// one.
fn comment_out_directive(body: &str, name: &str) -> String {
    let mut out = String::with_capacity(body.len() + 128);
    for line in body.lines() {
        let key = line.split_whitespace().next().unwrap_or("");
        if key.eq_ignore_ascii_case(name) {
            out.push_str(&format!("#{line} # moved to {PANEL_DROPIN_PATH} by vps-panel\n"));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// What the config files held before a save, to put back on failure.
struct SshdSnapshot {
    dropin: Option<String>,
    /// Only set when the save rewrote sshd_config.
    main: Option<String>,
}

async fn restore_sshd_files(snap: &SshdSnapshot) {
    let dropin = std::path::Path::new(PANEL_DROPIN_PATH);
    match &snap.dropin {
        Some(body) => {
            let _ = fs_atomic::write(dropin, body.as_bytes(), 0o644).await;
        }
        None => {
            let _ = fs_atomic::remove_if_exists(dropin).await;
        }
    }
    if let Some(body) = &snap.main {
        let _ = fs_atomic::write(std::path::Path::new(SSHD_CONFIG_PATH), body.as_bytes(), 0o644)
            .await;
    }
}

async fn port_in_use(port: u16) -> bool {
    let filter = format!("sport = :{port}");
    exec::run("ss", ["-Hltn", filter.as_str()])
        .await
        .is_ok_and(|o| !o.stdout.trim().is_empty())
}

async fn wait_for_listen(port: u16) -> bool {
    for _ in 0..20 {
        if port_in_use(port).await {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    false
}

/// Make the running sshd use the files on disk. With socket activation
/// (Ubuntu 24.04's default) systemd owns the listening socket: a reload
/// re-reads the settings but keeps the old port, so a port change has to
/// regenerate the socket from the config and restart it. Open sessions
/// survive both (ssh.service runs with KillMode=process).
async fn apply_sshd(port_changed: bool, socket_activated: bool) -> AgentResult<()> {
    if port_changed && socket_activated {
        exec::run("systemctl", ["daemon-reload"]).await?;
        exec::run("systemctl", ["restart", "ssh.socket"]).await?;
        return Ok(());
    }
    // A no-op while a socket-activated ssh.service has not started yet; it
    // reads the new files when it does.
    if exec::run("systemctl", ["try-reload-or-restart", "ssh"]).await.is_err() {
        exec::run("systemctl", ["try-reload-or-restart", "sshd"]).await?;
    }
    Ok(())
}

async fn set_jail_port(port: u16) {
    let path = std::path::Path::new(SSHD_JAIL_PORT_PATH);
    if !path.parent().is_some_and(|d| d.exists()) {
        return;
    }
    let res = if port == 22 {
        fs_atomic::remove_if_exists(path).await
    } else {
        let body = format!("# Managed by vps-panel (Settings > SSH server).\n[sshd]\nport = {port}\n");
        fs_atomic::write(path, body.as_bytes(), 0o644).await
    };
    if res.is_ok() {
        let _ = exec::run("fail2ban-client", ["reload", "sshd"]).await;
    }
}

pub async fn config_set(
    Json(req): Json<SshServerConfigUpdate>,
) -> AgentResult<Json<OkResponse>> {
    if req.port.is_none()
        && req.idle_timeout_minutes.is_none()
        && req.password_auth.is_none()
        && req.permit_root_login.is_none()
    {
        return Err(AgentError::Invalid("no fields to update".into()));
    }
    if req.port == Some(0) {
        return Err(AgentError::Invalid("port must be 1..=65535".into()));
    }
    if req.idle_timeout_minutes.is_some_and(|m| m > 24 * 60) {
        return Err(AgentError::Invalid("idle timeout must be 0..=1440 minutes".into()));
    }
    if let Some(mode) = &req.permit_root_login {
        if !ROOT_LOGIN_MODES.contains(&mode.as_str()) {
            return Err(AgentError::Invalid(format!(
                "permit_root_login must be one of: {}",
                ROOT_LOGIN_MODES.join(", ")
            )));
        }
    }

    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    let _guard = LOCK.get_or_init(|| tokio::sync::Mutex::new(())).lock().await;

    let before = effective_config().await?;
    let old_port = before.ports.first().copied().unwrap_or(22);
    let new_port = req.port.filter(|p| *p != old_port);
    if let Some(p) = new_port {
        if port_in_use(p).await {
            return Err(AgentError::Conflict(format!(
                "port {p} is already in use by another service"
            )));
        }
    }

    let dropin_path = std::path::Path::new(PANEL_DROPIN_PATH);
    let old_dropin = fs::read_to_string(dropin_path).await.ok();
    let mut dropin = old_dropin
        .clone()
        .unwrap_or_else(|| DROPIN_HEADER.to_string());
    if let Some(p) = new_port {
        dropin = set_directive(&dropin, "Port", Some(&p.to_string()));
    }
    if let Some(mins) = req.idle_timeout_minutes {
        // ChannelTimeout closes a session with no traffic; the old
        // ClientAliveInterval + ClientAliveCountMax 0 pair never did (on
        // OpenSSH 8.2+ a zero count disables the disconnect).
        let session = (mins > 0).then(|| format!("session={mins}m"));
        dropin = set_directive(&dropin, "ChannelTimeout", session.as_deref());
        let unused = (mins > 0).then(|| format!("{mins}m"));
        dropin = set_directive(&dropin, "UnusedConnectionTimeout", unused.as_deref());
    }
    if let Some(allow) = req.password_auth {
        let v = if allow { "yes" } else { "no" };
        dropin = set_directive(&dropin, "PasswordAuthentication", Some(v));
        // Keyboard-interactive (PAM) is a second way to type a password.
        dropin = set_directive(&dropin, "KbdInteractiveAuthentication", Some(v));
    }
    if let Some(mode) = &req.permit_root_login {
        dropin = set_directive(&dropin, "PermitRootLogin", Some(mode));
    }

    let main_path = std::path::Path::new(SSHD_CONFIG_PATH);
    let mut snap = SshdSnapshot { dropin: old_dropin, main: None };
    let mut new_main = None;
    if new_port.is_some() {
        let current = fs::read_to_string(main_path).await.map_err(AgentError::Io)?;
        let commented = comment_out_directive(&current, "Port");
        if commented != current {
            snap.main = Some(current);
            new_main = Some(commented);
        }
    }

    fs_atomic::ensure_dir(dropin_path.parent().unwrap(), 0o755).await?;
    fs_atomic::write(dropin_path, dropin.as_bytes(), 0o644).await?;
    if let Some(body) = &new_main {
        if let Err(e) = fs_atomic::write(main_path, body.as_bytes(), 0o644).await {
            restore_sshd_files(&snap).await;
            return Err(e);
        }
    }

    if let Err(e) = exec::run("sshd", ["-t"]).await {
        restore_sshd_files(&snap).await;
        return Err(AgentError::Invalid(format!(
            "sshd rejected the new settings, nothing was changed: {e}"
        )));
    }

    // Prove sshd will use what was written before touching the live daemon.
    let after = match effective_config().await {
        Ok(a) => a,
        Err(e) => {
            restore_sshd_files(&snap).await;
            return Err(e);
        }
    };
    let mut ignored = Vec::new();
    if let Some(p) = new_port {
        if after.ports != [p] {
            ignored.push(format!("Port {p} (sshd would listen on {:?})", after.ports));
        }
    }
    if let Some(m) = req.idle_timeout_minutes {
        if after.idle_timeout_minutes != m {
            ignored.push(format!("idle timeout {m} min"));
        }
    }
    if let Some(a) = req.password_auth {
        if after.password_auth != a || after.kbd_interactive != a {
            ignored.push(format!("PasswordAuthentication {}", if a { "yes" } else { "no" }));
        }
    }
    if let Some(mode) = &req.permit_root_login {
        if &after.permit_root_login != mode {
            ignored.push(format!("PermitRootLogin {mode}"));
        }
    }
    if !ignored.is_empty() {
        restore_sshd_files(&snap).await;
        return Err(AgentError::Invalid(format!(
            "sshd would not use: {}. Another sshd config file overrides {PANEL_DROPIN_PATH}; \
             check that {SSHD_CONFIG_PATH} starts with `Include /etc/ssh/sshd_config.d/*.conf`. \
             Nothing was changed.",
            ignored.join("; ")
        )));
    }

    let socket_activated = exec::run("systemctl", ["is-active", "--quiet", "ssh.socket"])
        .await
        .is_ok();

    let Some(p) = new_port else {
        if let Err(e) = apply_sshd(false, socket_activated).await {
            restore_sshd_files(&snap).await;
            return Err(AgentError::Invalid(format!(
                "sshd did not reload, the previous settings were restored: {e}"
            )));
        }
        return Ok(Json(OkResponse::default()));
    };

    // Open the new port before sshd moves to it, and close the old one only
    // once sshd is listening on the new one, so a failed switch can never
    // leave SSH unreachable.
    let _ = exec::run("ufw", ["allow", &format!("{p}/tcp")]).await;
    let applied = apply_sshd(true, socket_activated).await;
    if applied.is_err() || !wait_for_listen(p).await {
        restore_sshd_files(&snap).await;
        let _ = apply_sshd(true, socket_activated).await;
        let _ = exec::run("ufw", ["delete", "allow", &format!("{p}/tcp")]).await;
        return Err(AgentError::Invalid(format!(
            "sshd did not start listening on port {p}{}; the previous settings were restored and \
             port {old_port} stays open",
            applied.err().map(|e| format!(" ({e})")).unwrap_or_default()
        )));
    }
    let _ = exec::run("ufw", ["delete", "allow", &format!("{old_port}/tcp")]).await;
    if old_port == 22 {
        let _ = exec::run("ufw", ["delete", "allow", "OpenSSH"]).await;
    }
    set_jail_port(p).await;

    Ok(Json(OkResponse::default()))
}

#[derive(Deserialize)]
pub struct UserQuery {
    pub user: String,
}

#[derive(Deserialize)]
pub struct AddKey {
    pub user: String,
    pub key: String,
    pub label: Option<String>,
}

#[derive(Serialize)]
pub struct KeyRow {
    pub fingerprint: String,
    pub key_type: String,
    pub comment: String,
    /// When this row corresponds to a keypair the panel itself generated
    /// (via `generate_keypair`), this is the filename under `~/.ssh/`
    /// where the private half lives. Lets the UI surface a "View private
    /// key" action only when there's actually a private key on disk for
    /// the row — vs paste-imported keys where only the public half exists.
    pub keypair_name: Option<String>,
    /// False for a keypair in `~/.ssh/` whose public half is not in
    /// `authorized_keys`: this server can use it to reach other hosts (a
    /// deploy key for git over SSH, say), but it does not log in here.
    /// Listed so a keypair put there by hand shows up too; nothing adds it
    /// to `authorized_keys`.
    pub authorized: bool,
}

pub async fn list(Query(q): Query<UserQuery>) -> AgentResult<Json<Vec<KeyRow>>> {
    let path = authorized_keys_path(&q.user)?;
    let ssh_dir = ssh_home_dir_lookup(&q.user)?;
    Ok(Json(list_rows(&path, &ssh_dir).await))
}

/// The `authorized_keys` lines in file order, then the keypairs in `ssh_dir`
/// that are not among them (`authorized: false`), by name.
async fn list_rows(
    authorized_keys: &std::path::Path,
    ssh_dir: &std::path::Path,
) -> Vec<KeyRow> {
    let body = fs::read_to_string(authorized_keys).await.unwrap_or_default();
    // Index every `<name>.pub` in ~/.ssh/ by its fingerprint, but only if
    // the matching private file `<name>` is also on disk. Lets us match an
    // authorized_keys entry back to its keypair regardless of the comment
    // (which the operator may have changed, or which may have been imported
    // without our marker in the first place).
    let pub_index = scan_pub_files(ssh_dir).await;
    let mut rows = Vec::new();
    for (i, line) in body.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut parts = trimmed.splitn(3, ' ');
        let key_type = parts.next().unwrap_or("").to_string();
        let _key_data = parts.next();
        let comment = parts.next().unwrap_or("").to_string();
        match fingerprint(trimmed).await {
            Ok(fp) => {
                let keypair_name = pub_index.get(&fp).cloned();
                rows.push(KeyRow {
                    fingerprint: fp,
                    key_type,
                    comment,
                    keypair_name,
                    authorized: true,
                })
            }
            Err(_) => rows.push(KeyRow {
                fingerprint: format!("line-{}", i + 1),
                key_type,
                comment,
                keypair_name: None,
                authorized: true,
            }),
        }
    }

    // Only names the private-key and purge endpoints accept, so every
    // action on the row works.
    let name_ok = regex::Regex::new(KEYPAIR_NAME_RE).unwrap();
    let mut not_login: Vec<(&String, &String)> = pub_index
        .iter()
        .filter(|(fp, name)| {
            name_ok.is_match(name) && !rows.iter().any(|r| &r.fingerprint == *fp)
        })
        .collect();
    not_login.sort_by(|a, b| a.1.cmp(b.1));
    for (fp, name) in not_login {
        let content = fs::read_to_string(ssh_dir.join(format!("{name}.pub")))
            .await
            .unwrap_or_default();
        let mut parts = content.trim().splitn(3, ' ');
        let key_type = parts.next().unwrap_or("").to_string();
        let _key_data = parts.next();
        rows.push(KeyRow {
            fingerprint: fp.clone(),
            key_type,
            comment: parts.next().unwrap_or("").to_string(),
            keypair_name: Some(name.clone()),
            authorized: false,
        });
    }
    rows
}

/// Scan `<ssh_dir>` for `<name>.pub` files whose matching private file
/// `<name>` also exists, and index them by SSH fingerprint. The returned
/// map gives `fingerprint -> name`.
async fn scan_pub_files(
    ssh_dir: &std::path::Path,
) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let Ok(mut rd) = fs::read_dir(ssh_dir).await else {
        return out;
    };
    while let Ok(Some(entry)) = rd.next_entry().await {
        let file_name = entry.file_name();
        let s = file_name.to_string_lossy();
        let Some(base) = s.strip_suffix(".pub") else {
            continue;
        };
        // Require the matching private file. Stops orphan .pub files from
        // showing up as actionable keypairs in the UI.
        if !ssh_dir.join(base).exists() {
            continue;
        }
        let Ok(content) = fs::read_to_string(entry.path()).await else {
            continue;
        };
        let trimmed = content.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(fp) = fingerprint(trimmed).await {
            out.insert(fp, base.to_string());
        }
    }
    out
}

#[derive(Serialize)]
pub struct KeyContent {
    pub content: String,
}

/// Return the public-key line from `authorized_keys` matching the given
/// fingerprint. Lets the UI show + copy the same `ssh-ed25519 AAAA…` text
/// the operator would otherwise have to SSH in and `cat ~/.ssh/...pub` for.
pub async fn get_public_key(
    Path(fp): Path<String>,
    Query(q): Query<UserQuery>,
) -> AgentResult<Json<KeyContent>> {
    let path = authorized_keys_path(&q.user)?;
    let ssh_dir = ssh_home_dir_lookup(&q.user)?;
    public_key_line(&path, &ssh_dir, &fp)
        .await
        .map(|content| Json(KeyContent { content }))
        .ok_or(AgentError::NotFound)
}

/// The public-key line for `fp`: its `authorized_keys` line, or else the
/// `<name>.pub` of a keypair that is not a login key (`KeyRow::authorized`).
async fn public_key_line(
    authorized_keys: &std::path::Path,
    ssh_dir: &std::path::Path,
    fp: &str,
) -> Option<String> {
    let body = fs::read_to_string(authorized_keys).await.unwrap_or_default();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Ok(line_fp) = fingerprint(trimmed).await {
            if line_fp == fp {
                return Some(trimmed.to_string());
            }
        }
    }
    let name = scan_pub_files(ssh_dir).await.remove(fp)?;
    let content = fs::read_to_string(ssh_dir.join(format!("{name}.pub"))).await.ok()?;
    Some(content.trim().to_string())
}

#[derive(Deserialize)]
pub struct PrivateKeyQuery {
    pub user: String,
    pub name: String,
}

/// Return the private-key file content for a panel-generated keypair.
/// Refuses everything that doesn't look like a panel-managed keypair name
/// (same validator the create flow uses) so this endpoint can't be turned
/// into a generic "read any file under ~/.ssh/" sink.
pub async fn get_private_key(
    Query(q): Query<PrivateKeyQuery>,
) -> AgentResult<Json<KeyContent>> {
    if !regex::Regex::new(KEYPAIR_NAME_RE)
        .unwrap()
        .is_match(&q.name)
    {
        return Err(AgentError::Invalid("invalid keypair name".into()));
    }
    let ssh_dir = ssh_home_dir_lookup(&q.user)?;
    let priv_path = ssh_dir.join(&q.name);
    if !priv_path.exists() {
        return Err(AgentError::NotFound);
    }
    let content = fs::read_to_string(&priv_path)
        .await
        .map_err(AgentError::Io)?;
    Ok(Json(KeyContent { content }))
}

#[derive(Deserialize)]
pub struct UpdateKeyBody {
    pub content: String,
}

#[derive(Serialize)]
pub struct UpdateKeyResp {
    pub ok: bool,
    /// New fingerprint of the saved key (recomputed from the saved content).
    /// Lets the UI re-bind its "current fingerprint" so the next read uses
    /// the new value rather than 404'ing against the old one.
    pub fingerprint: Option<String>,
}

/// Replace the `authorized_keys` line matching `fp` with `body.content`.
/// Validates the new content is a parseable SSH public key, then rewrites
/// the file atomically. If `body.content` changes the underlying key (not
/// just the comment), the row's fingerprint moves with it.
///
/// When the OLD fingerprint also matches a `~/.ssh/<name>.pub` file (i.e.
/// this row was a panel-managed keypair), that `.pub` file gets rewritten
/// with the same new content. Otherwise the next `list()` would lose the
/// row's `keypair_name` association — the .pub-file index in `scan_pub_files`
/// matches by fingerprint, and a stale .pub file no longer matches the
/// new authorized_keys line.
pub async fn update_public_key(
    Path(fp): Path<String>,
    Query(q): Query<UserQuery>,
    Json(body): Json<UpdateKeyBody>,
) -> AgentResult<Json<UpdateKeyResp>> {
    let new_line = body.content.trim().to_string();
    if new_line.split_whitespace().count() < 2 {
        return Err(AgentError::Invalid(
            "public key must be at least: <type> <base64>".into(),
        ));
    }
    let new_fp = fingerprint(&new_line)
        .await
        .map_err(|_| AgentError::Invalid("not a valid SSH public key".into()))?;
    let ssh_dir = ssh_home_dir_lookup(&q.user)?;
    let path = authorized_keys_path(&q.user)?;
    let existing = fs::read_to_string(&path).await.unwrap_or_default();
    let mut out_lines: Vec<String> = Vec::with_capacity(existing.lines().count() + 1);
    let mut replaced = false;
    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            out_lines.push(line.to_string());
            continue;
        }
        if !replaced {
            if let Ok(line_fp) = fingerprint(trimmed).await {
                if line_fp == fp {
                    out_lines.push(new_line.clone());
                    replaced = true;
                    continue;
                }
            }
        }
        out_lines.push(line.to_string());
    }
    if !replaced {
        return Err(AgentError::NotFound);
    }
    let mut body_out = out_lines.join("\n");
    if !body_out.ends_with('\n') {
        body_out.push('\n');
    }
    fs_atomic::ensure_dir(path.parent().unwrap(), 0o700).await?;
    fs_atomic::write(&path, body_out.as_bytes(), 0o600).await?;
    chown_for_user(&q.user, &path)?;

    // Keep the matching `~/.ssh/<name>.pub` in sync if one exists. Scan the
    // dir with the OLD fingerprint because that's what's still on disk
    // until we touch the file. Best-effort: a failure here doesn't roll
    // back the authorized_keys write (which already succeeded), but is
    // logged so the operator can see the divergence.
    let pub_index = scan_pub_files(&ssh_dir).await;
    if let Some(name) = pub_index.get(&fp) {
        let pub_path = ssh_dir.join(format!("{name}.pub"));
        let mut content = new_line.clone();
        if !content.ends_with('\n') {
            content.push('\n');
        }
        if let Err(e) = fs_atomic::write(&pub_path, content.as_bytes(), 0o644).await {
            tracing::warn!(
                target: "ssh.update_public_key",
                error = %e,
                path = %pub_path.display(),
                "synced authorized_keys but failed to rewrite matching .pub file"
            );
        } else {
            let _ = chown_for_user(&q.user, &pub_path);
        }
    }

    Ok(Json(UpdateKeyResp {
        ok: true,
        fingerprint: Some(new_fp),
    }))
}

// ────────────────────────────────────────────────────────────────────────
// Raw whole-file GET/PUT for authorized_keys + ~/.ssh/config
// ────────────────────────────────────────────────────────────────────────
//
// The /ssh/keys CRUD operates line-by-line; this surface gives the
// operator the full file body so they can hand-tweak directives that the
// structured API doesn't model (e.g. `from="…"` options on a key,
// arbitrary Host blocks in config). Same auth (panel session) but the
// path is restricted to the two well-known files.

#[derive(Deserialize)]
pub struct SshFileQuery {
    pub user: String,
}

fn ssh_config_path(user: &str) -> AgentResult<PathBuf> {
    Ok(ssh_home_dir_lookup(user)?.join("config"))
}

pub async fn get_authorized_keys_file(
    Query(q): Query<SshFileQuery>,
) -> AgentResult<Json<KeyContent>> {
    let path = authorized_keys_path(&q.user)?;
    let content = fs::read_to_string(&path).await.unwrap_or_default();
    Ok(Json(KeyContent { content }))
}

pub async fn put_authorized_keys_file(
    Query(q): Query<SshFileQuery>,
    Json(body): Json<UpdateKeyBody>,
) -> AgentResult<Json<UpdateKeyResp>> {
    let path = authorized_keys_path(&q.user)?;
    let mut content = body.content;
    if !content.ends_with('\n') {
        content.push('\n');
    }
    fs_atomic::ensure_dir(path.parent().unwrap(), 0o700).await?;
    fs_atomic::write(&path, content.as_bytes(), 0o600).await?;
    chown_for_user(&q.user, &path)?;
    Ok(Json(UpdateKeyResp { ok: true, fingerprint: None }))
}

pub async fn get_ssh_config_file(
    Query(q): Query<SshFileQuery>,
) -> AgentResult<Json<KeyContent>> {
    let path = ssh_config_path(&q.user)?;
    let content = fs::read_to_string(&path).await.unwrap_or_default();
    Ok(Json(KeyContent { content }))
}

pub async fn put_ssh_config_file(
    Query(q): Query<SshFileQuery>,
    Json(body): Json<UpdateKeyBody>,
) -> AgentResult<Json<UpdateKeyResp>> {
    let path = ssh_config_path(&q.user)?;
    let mut content = body.content;
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    fs_atomic::ensure_dir(path.parent().unwrap(), 0o700).await?;
    fs_atomic::write(&path, content.as_bytes(), 0o600).await?;
    chown_for_user(&q.user, &path)?;
    Ok(Json(UpdateKeyResp { ok: true, fingerprint: None }))
}

/// Overwrite the private-key file at `~/<user_home>/.ssh/<name>` with
/// `body.content`. Validates `name` against the same allowlist used by
/// `generate_keypair` and `get_private_key` so this endpoint can't write
/// to arbitrary paths.
pub async fn update_private_key(
    Query(q): Query<PrivateKeyQuery>,
    Json(body): Json<UpdateKeyBody>,
) -> AgentResult<Json<UpdateKeyResp>> {
    if !regex::Regex::new(KEYPAIR_NAME_RE)
        .unwrap()
        .is_match(&q.name)
    {
        return Err(AgentError::Invalid("invalid keypair name".into()));
    }
    let trimmed = body.content.trim();
    if !trimmed.starts_with("-----BEGIN") {
        return Err(AgentError::Invalid(
            "private key must be a PEM/OpenSSH block starting with -----BEGIN".into(),
        ));
    }
    let ssh_dir = ssh_home_dir_lookup(&q.user)?;
    fs_atomic::ensure_dir(&ssh_dir, 0o700).await?;
    let priv_path = ssh_dir.join(&q.name);
    let mut to_write = trimmed.to_string();
    if !to_write.ends_with('\n') {
        to_write.push('\n');
    }
    fs_atomic::write(&priv_path, to_write.as_bytes(), 0o600).await?;
    chown_for_user(&q.user, &priv_path)?;
    Ok(Json(UpdateKeyResp {
        ok: true,
        fingerprint: None,
    }))
}

pub async fn add(Json(req): Json<AddKey>) -> AgentResult<Json<OkResponse>> {
    let path = authorized_keys_path(&req.user)?;
    let key = req.key.trim();
    if key.split_whitespace().count() < 2 {
        return Err(AgentError::Invalid(
            "key must be at least: <type> <base64>".into(),
        ));
    }
    fingerprint(key)
        .await
        .map_err(|_| AgentError::Invalid("not a valid SSH public key".into()))?;
    let mut content = fs::read_to_string(&path).await.unwrap_or_default();
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    let label = req.label.unwrap_or_default();
    if label.is_empty() {
        content.push_str(key);
    } else {
        content.push_str(&format!("{key} {label}"));
    }
    content.push('\n');
    fs_atomic::ensure_dir(path.parent().unwrap(), 0o700).await?;
    fs_atomic::write(&path, content.as_bytes(), 0o600).await?;
    chown_for_user(&req.user, &path)?;
    Ok(Json(OkResponse::default()))
}

#[derive(Deserialize)]
pub struct GenerateKeypair {
    pub user: String,
    /// Filename of the keypair inside `~/.ssh/`. Becomes the Host alias in
    /// `~/.ssh/config` too. Must be lowercase alphanumeric / `_` / `-` so it's
    /// safe as both a filename and an SSH Host token.
    pub name: String,
}

#[derive(Serialize)]
pub struct GenerateKeypairResp {
    pub ok: bool,
    /// Full content of `<name>.pub` so the panel UI can show + copy it (the
    /// operator typically pastes this into GitHub's Deploy Keys).
    pub public_key: String,
    pub private_key_path: String,
    pub public_key_path: String,
}

const KEYPAIR_NAME_RE: &str = r"^[a-z0-9][a-z0-9_-]{0,30}[a-z0-9]$";

pub async fn generate_keypair(
    Json(req): Json<GenerateKeypair>,
) -> AgentResult<Json<GenerateKeypairResp>> {
    if !is_safe_user(&req.user) {
        return Err(AgentError::Invalid("invalid user".into()));
    }
    if !regex::Regex::new(KEYPAIR_NAME_RE)
        .unwrap()
        .is_match(&req.name)
    {
        return Err(AgentError::Invalid(
            "keypair name must be lowercase alphanumeric (with `_` or `-`), 2-32 chars".into(),
        ));
    }
    let ssh_dir = ssh_home_dir_lookup(&req.user)?;
    let priv_path = ssh_dir.join(&req.name);
    let pub_path = ssh_dir.join(format!("{}.pub", req.name));
    let authorized_keys = ssh_dir.join("authorized_keys");
    let config_path = ssh_dir.join("config");

    if priv_path.exists() || pub_path.exists() {
        return Err(AgentError::Conflict(format!(
            "keypair {} already exists at {}",
            req.name,
            priv_path.display()
        )));
    }

    // Ensure ~/.ssh/ exists with 0o700, owned by the target user.
    fs_atomic::ensure_dir(&ssh_dir, 0o700).await?;
    chown_for_user_dir(&req.user, &ssh_dir)?;

    // Generate the keypair. ed25519 is the modern default; no passphrase so the
    // server can use it non-interactively (typical for git+ssh from CI/cron).
    // Set the comment to `<name>@<user>` (e.g. `github_vps_panel@apps`) so the
    // public-key line in authorized_keys is self-describing: which keypair, on
    // which account. We deliberately don't use the ssh-keygen default of
    // `root@<hostname>` — the agent always runs as root, and that comment
    // would leak the panel hostname into every public key the operator pastes
    // into GitHub or hands to a teammate. `<name>@<user>` carries the same
    // amount of useful information (which key, where it lives) without the
    // leak.
    let key_comment = format!("{}@{}", req.name, req.user);
    exec::run(
        "ssh-keygen",
        [
            "-t",
            "ed25519",
            "-f",
            priv_path.to_str().unwrap(),
            "-N",
            "",
            "-C",
            key_comment.as_str(),
            "-q",
        ],
    )
    .await?;

    // ssh-keygen creates with 0600/0644 already; reaffirm + chown to target.
    fs_atomic::chmod(&priv_path, 0o600).await?;
    fs_atomic::chmod(&pub_path, 0o644).await?;
    chown_for_user(&req.user, &priv_path)?;
    chown_for_user(&req.user, &pub_path)?;

    let pub_content = fs::read_to_string(&pub_path).await.map_err(AgentError::Io)?;

    // Append the public half to authorized_keys so the same keypair can also
    // be used to SSH back into this account.
    append_unique_line(&authorized_keys, pub_content.trim()).await?;
    fs_atomic::chmod(&authorized_keys, 0o600).await?;
    chown_for_user(&req.user, &authorized_keys)?;

    // Append a Host block to ~/.ssh/config pointing IdentityFile at the new
    // private key. Idempotent: if a Host block with the same alias already
    // exists, we don't write a second one (would otherwise let ssh pick the
    // last one which is what the operator wants, but messy).
    let host_block = format!(
        "\nHost {name}\n  HostName github.com\n  User git\n  IdentityFile ~/.ssh/{name}\n  IdentitiesOnly yes\n",
        name = req.name
    );
    append_unique_host_block(&config_path, &req.name, &host_block).await?;
    fs_atomic::chmod(&config_path, 0o600).await?;
    chown_for_user(&req.user, &config_path)?;

    Ok(Json(GenerateKeypairResp {
        ok: true,
        public_key: pub_content,
        private_key_path: priv_path.display().to_string(),
        public_key_path: pub_path.display().to_string(),
    }))
}

#[cfg(unix)]
fn chown_for_user_dir(user: &str, path: &std::path::Path) -> AgentResult<()> {
    use nix::unistd::User;
    let u = User::from_name(user)
        .map_err(|e| AgentError::Other(anyhow::anyhow!("getpwnam: {e}")))?
        .ok_or_else(|| AgentError::Invalid(format!("no such user: {user}")))?;
    fs_atomic::chown_blocking(path, u.uid.as_raw(), u.gid.as_raw())
}

#[cfg(not(unix))]
fn chown_for_user_dir(_user: &str, _path: &std::path::Path) -> AgentResult<()> {
    Ok(())
}

async fn append_unique_line(path: &std::path::Path, line: &str) -> AgentResult<()> {
    let mut existing = fs::read_to_string(path).await.unwrap_or_default();
    if existing.lines().any(|l| l.trim() == line.trim()) {
        return Ok(());
    }
    if !existing.is_empty() && !existing.ends_with('\n') {
        existing.push('\n');
    }
    existing.push_str(line);
    existing.push('\n');
    fs_atomic::write(path, existing.as_bytes(), 0o600).await
}

async fn append_unique_host_block(
    path: &std::path::Path,
    alias: &str,
    block: &str,
) -> AgentResult<()> {
    let existing = fs::read_to_string(path).await.unwrap_or_default();
    // Cheap dup check: any line that's `Host <alias>` (with possible extra
    // hosts on the same line) — refuse to write a second block.
    let needle = format!("Host {alias}");
    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed == needle
            || trimmed.starts_with(&format!("{needle} "))
            || trimmed.starts_with(&format!("{needle}\t"))
        {
            return Ok(());
        }
    }
    let mut new_content = existing;
    if !new_content.is_empty() && !new_content.ends_with('\n') {
        new_content.push('\n');
    }
    new_content.push_str(block);
    fs_atomic::write(path, new_content.as_bytes(), 0o600).await
}

/// Remove every `Host <alias>` block (exact single-alias form, as written by
/// `generate_keypair`) from an ssh_config body. A block runs from its `Host`
/// line up to (excluding) the next `Host`/`Match` line or EOF. Multi-alias
/// lines (`Host a b`) are left alone: removing them would take other aliases'
/// config with them. One blank separator line immediately before the block is
/// dropped too (generate writes one). Returns the new body and whether
/// anything was removed.
fn strip_host_block(body: &str, alias: &str) -> (String, bool) {
    let mut out: Vec<&str> = Vec::new();
    let mut removed = false;
    let mut skipping = false;
    for line in body.lines() {
        let mut tokens = line.split_whitespace();
        let first = tokens.next().unwrap_or("");
        let is_host = first.eq_ignore_ascii_case("host");
        if is_host || first.eq_ignore_ascii_case("match") {
            let rest: Vec<&str> = tokens.collect();
            if is_host && rest.len() == 1 && rest[0] == alias {
                if out.last().is_some_and(|l| l.trim().is_empty()) {
                    out.pop();
                }
                skipping = true;
                removed = true;
                continue;
            }
            skipping = false;
        }
        if !skipping {
            out.push(line);
        }
    }
    let mut new_body = out.join("\n");
    if new_body.trim().is_empty() {
        new_body = String::new();
    } else if !new_body.ends_with('\n') {
        new_body.push('\n');
    }
    (new_body, removed)
}

#[derive(Deserialize)]
pub struct RemoveKeyQuery {
    pub user: String,
    /// When set, also delete the panel-generated keypair files
    /// `~/.ssh/<name>` + `~/.ssh/<name>.pub` and strip the `Host <name>`
    /// block from `~/.ssh/config`. Without this, removing a generated key's
    /// authorized_keys line leaves the files behind - invisible in the UI
    /// (the list is driven by authorized_keys lines) but still blocking
    /// `generate_keypair` from reusing the name.
    pub purge_keypair: Option<String>,
}

pub async fn remove(
    Path(fp): Path<String>,
    Query(q): Query<RemoveKeyQuery>,
) -> AgentResult<Json<OkResponse>> {
    // Resolve and verify the purge target before mutating anything, so a bad
    // request (malformed name, fingerprint mismatch) fails clean with the
    // system untouched.
    let purge = match &q.purge_keypair {
        None => None,
        Some(name) => {
            if !regex::Regex::new(KEYPAIR_NAME_RE).unwrap().is_match(name) {
                return Err(AgentError::Invalid("invalid keypair name".into()));
            }
            let ssh_dir = ssh_home_dir_lookup(&q.user)?;
            let pub_path = ssh_dir.join(format!("{name}.pub"));
            // The files must belong to the key being removed: the on-disk
            // .pub has to fingerprint-match `fp` (the UI derives
            // `keypair_name` from exactly this match). A missing .pub is
            // allowed through - that's the half-purged recovery case, and
            // deleting the leftovers is the point of the flag.
            if pub_path.exists() {
                let content = fs::read_to_string(&pub_path)
                    .await
                    .map_err(AgentError::Io)?;
                let on_disk_fp = fingerprint(content.trim()).await.map_err(|_| {
                    AgentError::Invalid(format!("{name}.pub is not a valid public key"))
                })?;
                if on_disk_fp != fp {
                    return Err(AgentError::Invalid(format!(
                        "keypair '{name}' does not match this key's fingerprint; nothing was deleted"
                    )));
                }
            }
            Some((ssh_dir, name.clone()))
        }
    };

    let path = authorized_keys_path(&q.user)?;
    if drop_authorized_line(&path, &fp).await? {
        chown_for_user(&q.user, &path)?;
    }

    if let Some((ssh_dir, name)) = purge {
        fs_atomic::remove_if_exists(&ssh_dir.join(&name)).await?;
        fs_atomic::remove_if_exists(&ssh_dir.join(format!("{name}.pub"))).await?;
        let config_path = ssh_dir.join("config");
        let existing = fs::read_to_string(&config_path).await.unwrap_or_default();
        let (stripped, changed) = strip_host_block(&existing, &name);
        if changed {
            fs_atomic::write(&config_path, stripped.as_bytes(), 0o600).await?;
            chown_for_user(&q.user, &config_path)?;
        }
    }

    Ok(Json(OkResponse::default()))
}

/// Drop the `authorized_keys` line whose fingerprint is `fp`; returns whether
/// one went. The file is written only then: a keypair that is not a login
/// key has no line (purging its files is the whole delete), and writing
/// anyway would create the file for an account that never had one.
async fn drop_authorized_line(path: &std::path::Path, fp: &str) -> AgentResult<bool> {
    let body = fs::read_to_string(path).await.unwrap_or_default();
    let mut keep = Vec::new();
    let mut removed = false;
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            keep.push(line.to_string());
            continue;
        }
        match fingerprint(trimmed).await {
            Ok(this_fp) if this_fp == fp => removed = true,
            _ => keep.push(line.to_string()),
        }
    }
    if removed {
        let mut new_body = keep.join("\n");
        if !new_body.ends_with('\n') {
            new_body.push('\n');
        }
        fs_atomic::write(path, new_body.as_bytes(), 0o600).await?;
    }
    Ok(removed)
}

fn authorized_keys_path(user: &str) -> AgentResult<PathBuf> {
    Ok(ssh_home_dir_lookup(user)?.join("authorized_keys"))
}

/// Resolve the user's `~/.ssh` directory via `/etc/passwd` rather than
/// hardcoding `/root/.ssh` / `/home/<user>/.ssh`. Matters for system users
/// like `apps` whose home is `/srv/apps` (not `/home/apps`) — without this
/// the SSH-key handlers would write to a path the SSH daemon would never
/// check.
fn ssh_home_dir_lookup(user: &str) -> AgentResult<PathBuf> {
    if !is_safe_user(user) {
        return Err(AgentError::Invalid("invalid user".into()));
    }
    #[cfg(unix)]
    {
        use nix::unistd::User;
        let u = User::from_name(user)
            .map_err(|e| AgentError::Other(anyhow::anyhow!("getpwnam: {e}")))?
            .ok_or_else(|| AgentError::Invalid(format!("no such user: {user}")))?;
        Ok(u.dir.join(".ssh"))
    }
    #[cfg(not(unix))]
    {
        // Fallback for non-unix targets (dev only; agent only ships on linux).
        if user == "root" {
            Ok(PathBuf::from("/root/.ssh"))
        } else {
            Ok(PathBuf::from(format!("/home/{user}/.ssh")))
        }
    }
}

fn is_safe_user(user: &str) -> bool {
    let re = regex::Regex::new(r"^[a-z_][a-z0-9_-]{0,30}$").unwrap();
    re.is_match(user)
}

#[cfg(unix)]
fn chown_for_user(user: &str, path: &std::path::Path) -> AgentResult<()> {
    use nix::unistd::User;
    let u = User::from_name(user)
        .map_err(|e| AgentError::Other(anyhow::anyhow!("getpwnam: {e}")))?
        .ok_or_else(|| AgentError::Invalid(format!("no such user: {user}")))?;
    let parent = path.parent().unwrap();
    let _ = fs_atomic::chown_blocking(parent, u.uid.as_raw(), u.gid.as_raw());
    fs_atomic::chown_blocking(path, u.uid.as_raw(), u.gid.as_raw())
}

#[cfg(not(unix))]
fn chown_for_user(_user: &str, _path: &std::path::Path) -> AgentResult<()> {
    Ok(())
}

async fn fingerprint(key_line: &str) -> AgentResult<String> {
    let out = exec::run_with_input("ssh-keygen", ["-lf", "/dev/stdin"], key_line).await?;
    let parts: Vec<&str> = out.stdout.split_whitespace().collect();
    let fp = parts.get(1).copied().unwrap_or("").to_string();
    Ok(fp)
}

#[cfg(test)]
mod tests {
    use super::strip_host_block;

    use super::{
        comment_out_directive, parse_effective, parse_sshd_time, session_timeout_minutes,
        set_directive,
    };

    const DUMP: &str = "port 2222\nport 22\npermitrootlogin without-password\n\
                        passwordauthentication no\nkbdinteractiveauthentication no\n\
                        channeltimeout session=30m\n";

    #[test]
    fn effective_config_from_sshd_dump() {
        let e = parse_effective(DUMP);
        assert_eq!(e.ports, vec![2222, 22]);
        assert_eq!(e.permit_root_login, "prohibit-password");
        assert!(!e.password_auth);
        assert!(!e.kbd_interactive);
        assert_eq!(e.idle_timeout_minutes, 30);
    }

    #[test]
    fn effective_config_keeps_root_login_no_distinct() {
        let e = parse_effective("port 22\npermitrootlogin no\nchanneltimeout none\n");
        assert_eq!(e.permit_root_login, "no");
        assert!(e.password_auth);
        assert_eq!(e.idle_timeout_minutes, 0);
    }

    #[test]
    fn channel_timeout_forms() {
        assert_eq!(session_timeout_minutes(&["agent-connection=1m session=1h30m"]), 90);
        assert_eq!(session_timeout_minutes(&["*=90"]), 2);
        assert_eq!(session_timeout_minutes(&["global=5m"]), 0);
        assert_eq!(parse_sshd_time("1w"), Some(604_800));
        assert_eq!(parse_sshd_time("none"), None);
    }

    #[test]
    fn set_directive_replaces_appends_and_removes() {
        let body = "# header\nPort 22\nPasswordAuthentication yes\nport 2200\n";
        assert_eq!(
            set_directive(body, "Port", Some("2222")),
            "# header\nPort 2222\nPasswordAuthentication yes\n"
        );
        assert_eq!(
            set_directive(body, "PermitRootLogin", Some("no")),
            "# header\nPort 22\nPasswordAuthentication yes\nport 2200\nPermitRootLogin no\n"
        );
        assert_eq!(
            set_directive(body, "Port", None),
            "# header\nPasswordAuthentication yes\n"
        );
    }

    #[test]
    fn comment_out_leaves_comments_and_other_keys() {
        let body = "Include /etc/ssh/sshd_config.d/*.conf\n#Port 22\nPort 22\nPortless yes\n";
        let out = comment_out_directive(body, "Port");
        assert!(out.starts_with("Include /etc/ssh/sshd_config.d/*.conf\n#Port 22\n#Port 22 # moved"));
        assert!(out.ends_with("Portless yes\n"));
    }

    /// Two blocks exactly as `generate_keypair` writes them (leading blank
    /// separator before each appended block).
    const TWO_BLOCKS: &str = "Host github_a\n  HostName github.com\n  User git\n  IdentityFile ~/.ssh/github_a\n  IdentitiesOnly yes\n\nHost github_b\n  HostName github.com\n  User git\n  IdentityFile ~/.ssh/github_b\n  IdentitiesOnly yes\n";

    #[test]
    fn strips_leading_block() {
        let (out, removed) = strip_host_block(TWO_BLOCKS, "github_a");
        assert!(removed);
        assert_eq!(
            out,
            "Host github_b\n  HostName github.com\n  User git\n  IdentityFile ~/.ssh/github_b\n  IdentitiesOnly yes\n"
        );
    }

    #[test]
    fn strips_trailing_block_and_blank_separator() {
        let (out, removed) = strip_host_block(TWO_BLOCKS, "github_b");
        assert!(removed);
        assert_eq!(
            out,
            "Host github_a\n  HostName github.com\n  User git\n  IdentityFile ~/.ssh/github_a\n  IdentitiesOnly yes\n"
        );
    }

    #[test]
    fn noop_when_alias_absent() {
        let (out, removed) = strip_host_block(TWO_BLOCKS, "github_c");
        assert!(!removed);
        assert_eq!(out, TWO_BLOCKS);
    }

    #[test]
    fn leaves_multi_alias_host_lines_alone() {
        let body = "Host github_a github_b\n  HostName github.com\n";
        let (out, removed) = strip_host_block(body, "github_a");
        assert!(!removed);
        assert_eq!(out, body);
    }

    #[test]
    fn matches_host_keyword_case_insensitively() {
        let body = "host github_a\n  HostName github.com\n\nHost other\n  HostName example.com\n";
        let (out, removed) = strip_host_block(body, "github_a");
        assert!(removed);
        assert_eq!(out, "Host other\n  HostName example.com\n");
    }

    #[test]
    fn whole_file_block_leaves_empty_body() {
        let body = "Host solo\n  HostName github.com\n  User git\n";
        let (out, removed) = strip_host_block(body, "solo");
        assert!(removed);
        assert_eq!(out, "");
    }

    #[test]
    fn block_ends_at_match_keyword() {
        let body = "Host gone\n  HostName github.com\nMatch all\n  Compression yes\n";
        let (out, removed) = strip_host_block(body, "gone");
        assert!(removed);
        assert_eq!(out, "Match all\n  Compression yes\n");
    }

    #[test]
    fn empty_config_is_noop() {
        let (out, removed) = strip_host_block("", "anything");
        assert!(!removed);
        assert_eq!(out, "");
    }

    // A scratch ~/.ssh with real keys from ssh-keygen, which also computes
    // the fingerprints the code under test matches on.
    fn scratch_ssh_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ssh-keys-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn keygen(dir: &std::path::Path, name: &str, comment: &str) -> String {
        let status = std::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", comment, "-f"])
            .arg(dir.join(name))
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::read_to_string(dir.join(format!("{name}.pub")))
            .unwrap()
            .trim()
            .to_string()
    }

    #[tokio::test]
    async fn lists_login_keys_then_keypairs_that_do_not_log_in() {
        let dir = scratch_ssh_dir();
        let login = keygen(&dir, "login_key", "login_key@root");
        // Only the public half of a key added by paste is on the server.
        let pasted = keygen(&dir, "pasted", "laptop");
        std::fs::remove_file(dir.join("pasted")).unwrap();
        std::fs::remove_file(dir.join("pasted.pub")).unwrap();
        // Put in ~/.ssh by hand, never added to authorized_keys.
        let deploy = keygen(&dir, "github_vps_panel", "github_vps_panel");
        // Not a name the private-key and purge endpoints accept.
        keygen(&dir, "Bad.Name", "bad name");
        // A .pub without its private half is not a keypair.
        keygen(&dir, "orphan", "orphan");
        std::fs::remove_file(dir.join("orphan")).unwrap();
        let ak = dir.join("authorized_keys");
        std::fs::write(&ak, format!("{login}\n# a comment\n{pasted}\n")).unwrap();

        let rows = super::list_rows(&ak, &dir).await;
        let summary: Vec<(&str, Option<&str>, bool)> = rows
            .iter()
            .map(|r| (r.comment.as_str(), r.keypair_name.as_deref(), r.authorized))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("login_key@root", Some("login_key"), true),
                ("laptop", None, true),
                ("github_vps_panel", Some("github_vps_panel"), false),
            ]
        );
        assert_eq!(rows[2].key_type, "ssh-ed25519");

        // View keypair: a login key's public half comes from authorized_keys,
        // the other one's from its .pub.
        let line = |fp: String| {
            let (ak, dir) = (ak.clone(), dir.clone());
            async move { super::public_key_line(&ak, &dir, &fp).await }
        };
        assert_eq!(line(rows[1].fingerprint.clone()).await, Some(pasted));
        assert_eq!(line(rows[2].fingerprint.clone()).await, Some(deploy));
        assert_eq!(line("SHA256:unknown".into()).await, None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn drops_only_a_matching_line_and_writes_only_then() {
        let dir = scratch_ssh_dir();
        let a = keygen(&dir, "a", "a");
        let b = keygen(&dir, "b", "b");
        let ak = dir.join("authorized_keys");
        std::fs::write(&ak, format!("{a}\n{b}\n")).unwrap();
        let rows = super::list_rows(&ak, &dir).await;

        assert!(super::drop_authorized_line(&ak, &rows[0].fingerprint).await.unwrap());
        assert_eq!(std::fs::read_to_string(&ak).unwrap(), format!("{b}\n"));
        assert!(!super::drop_authorized_line(&ak, &rows[0].fingerprint).await.unwrap());
        assert_eq!(std::fs::read_to_string(&ak).unwrap(), format!("{b}\n"));

        // No authorized_keys at all: deleting a keypair must not create one.
        let none = dir.join("none");
        assert!(!super::drop_authorized_line(&none, &rows[1].fingerprint).await.unwrap());
        assert!(!none.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
