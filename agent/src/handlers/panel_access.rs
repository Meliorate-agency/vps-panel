//! Panel access allowlist (Settings > Panel access): which client addresses
//! nginx lets through to the panel, the MCP endpoint included. Every server
//! block of the panel vhost includes ACCESS_CONF; with no entries the file
//! only holds a comment and nothing is restricted. The setting used to be
//! stored and never applied.
//!
//! Locked out (a new home IP)? On the server: `vps-panel-agent access-reset`.

use crate::error::{AgentError, AgentResult};
use crate::exec;
use crate::fs_atomic;
use crate::ipc::OkResponse;
use axum::Json;
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const ACCESS_CONF: &str = "/etc/nginx/vps-panel-access.conf";
pub const INCLUDE_LINE: &str = "include /etc/nginx/vps-panel-access.conf;";
pub const EMPTY_BODY: &str =
    "# Managed by vps-panel (Settings > Panel access). No entries: every address may reach the panel.\n";
const SITES_AVAILABLE: &str = "/etc/nginx/sites-available";

#[derive(Deserialize)]
pub struct AccessReq {
    pub entries: Vec<String>,
}

/// `1.2.3.4`, `10.0.0.0/8` or `2001:db8::/32`, normalized.
pub fn parse_entry(s: &str) -> Option<String> {
    let s = s.trim();
    let (addr, prefix) = match s.split_once('/') {
        Some((a, p)) => (a, Some(p)),
        None => (s, None),
    };
    let ip: std::net::IpAddr = addr.parse().ok()?;
    match prefix {
        None => Some(ip.to_string()),
        Some(p) => {
            let p: u8 = p.parse().ok()?;
            let max = if ip.is_ipv4() { 32 } else { 128 };
            (p <= max).then(|| format!("{ip}/{p}"))
        }
    }
}

pub fn render(entries: &[String]) -> String {
    if entries.is_empty() {
        return EMPTY_BODY.to_string();
    }
    let mut out = String::from(
        "# Managed by vps-panel (Settings > Panel access). Only these addresses reach the panel.\n",
    );
    for e in entries {
        out.push_str(&format!("allow {e};\n"));
    }
    // The box itself (health checks, curl on the server) always gets in.
    out.push_str("allow 127.0.0.1;\nallow ::1;\ndeny all;\n");
    out
}

/// Add the include after every `server_name` line of a vhost that has none.
/// On the HTTP-to-HTTPS redirect block it is harmless: `return 301` runs
/// before nginx checks access, and the ACME location says `allow all`.
pub fn with_include(body: &str) -> Option<String> {
    if body.contains("vps-panel-access.conf") {
        return None;
    }
    let mut out = String::with_capacity(body.len() + 128);
    for line in body.lines() {
        out.push_str(line);
        out.push('\n');
        if line.trim_start().starts_with("server_name ") {
            let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
            out.push_str(&format!("{indent}{INCLUDE_LINE}\n"));
        }
    }
    Some(out)
}

/// Vhosts that include the file fail `nginx -t` without it.
pub async fn ensure_file() -> AgentResult<()> {
    if !Path::new(ACCESS_CONF).exists() {
        fs_atomic::write(Path::new(ACCESS_CONF), EMPTY_BODY.as_bytes(), 0o644).await?;
    }
    Ok(())
}

/// Files in sites-available whose first line is the `# vps-panel-host:`
/// marker every panel vhost writer puts there.
async fn panel_vhosts() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(mut rd) = tokio::fs::read_dir(SITES_AVAILABLE).await else {
        return out;
    };
    while let Ok(Some(entry)) = rd.next_entry().await {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Ok(body) = tokio::fs::read_to_string(&path).await else {
            continue;
        };
        if body.lines().next().is_some_and(|l| l.starts_with("# vps-panel-host:")) {
            out.push(path);
        }
    }
    out
}

/// PUT /system/panel-access
pub async fn set(Json(req): Json<AccessReq>) -> AgentResult<Json<OkResponse>> {
    let mut entries: Vec<String> = Vec::new();
    for raw in req.entries.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let e = parse_entry(raw)
            .ok_or_else(|| AgentError::Invalid(format!("not an IP address or CIDR: {raw}")))?;
        if !entries.contains(&e) {
            entries.push(e);
        }
    }

    ensure_file().await?;
    let vhosts = panel_vhosts().await;
    if vhosts.is_empty() && !entries.is_empty() {
        return Err(AgentError::Invalid(format!(
            "no panel vhost found (a file in {SITES_AVAILABLE} starting with '# vps-panel-host:'), \
             so an allowlist would not apply"
        )));
    }

    let previous = tokio::fs::read_to_string(ACCESS_CONF).await.unwrap_or_default();
    let mut patched: Vec<(PathBuf, String)> = Vec::new();
    let result = async {
        for vhost in &vhosts {
            let body = tokio::fs::read_to_string(vhost).await.map_err(AgentError::Io)?;
            if let Some(new_body) = with_include(&body) {
                fs_atomic::write(vhost, new_body.as_bytes(), 0o644).await?;
                patched.push((vhost.clone(), body));
            }
        }
        fs_atomic::write(Path::new(ACCESS_CONF), render(&entries).as_bytes(), 0o644).await?;
        exec::run("nginx", ["-t"])
            .await
            .map_err(|e| AgentError::Invalid(format!("nginx rejected the access list: {e}")))?;
        exec::run("systemctl", ["reload", "nginx"]).await.map(|_| ())
    }
    .await;
    if let Err(e) = result {
        let _ = fs_atomic::write(Path::new(ACCESS_CONF), previous.as_bytes(), 0o644).await;
        for (path, body) in &patched {
            let _ = fs_atomic::write(path, body.as_bytes(), 0o644).await;
        }
        return Err(e);
    }
    Ok(Json(OkResponse::default()))
}

#[cfg(test)]
mod tests {
    use super::{parse_entry, render, with_include, INCLUDE_LINE};

    #[test]
    fn entries_are_validated_and_normalized() {
        assert_eq!(parse_entry(" 203.0.113.7 ").as_deref(), Some("203.0.113.7"));
        assert_eq!(parse_entry("10.0.0.0/8").as_deref(), Some("10.0.0.0/8"));
        assert_eq!(parse_entry("2001:DB8::/32").as_deref(), Some("2001:db8::/32"));
        for bad in ["", "10.0.0.0/33", "::/129", "example.com", "1.2.3.4; deny all", "1.2.3/24"] {
            assert!(parse_entry(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn empty_list_restricts_nothing() {
        assert!(!render(&[]).contains("deny"));
        let body = render(&["203.0.113.7".into()]);
        assert!(body.contains("allow 203.0.113.7;\n") && body.ends_with("deny all;\n"));
    }

    #[test]
    fn include_goes_after_each_server_name_once() {
        let vhost = "# vps-panel-host:p.example\nserver {\n    listen 80;\n    server_name p.example;\n}\nserver {\n    listen 443 ssl;\n    server_name p.example;\n}\n";
        let out = with_include(vhost).unwrap();
        assert_eq!(out.matches(INCLUDE_LINE).count(), 2);
        assert!(out.contains(&format!("    server_name p.example;\n    {INCLUDE_LINE}\n")));
        assert!(with_include(&out).is_none());
    }
}
