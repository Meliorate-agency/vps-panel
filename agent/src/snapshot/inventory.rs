//! What a server snapshot says about the server it was made on: the facts
//! the plan needs, the manifest's description, and the `system/*.json`
//! files the import reads to rebuild what lives outside files and databases
//! (accounts made on /users, Docker containers, Tailscale, the firewall,
//! swap). Nothing here fails the export: a fact that cannot be read is
//! left empty and noted.

use super::format::{Inventory, PanelInfo, Platform, SourceInfo};
use super::plan::Facts;
use crate::exec;
use crate::state::AppState;
use mysql_async::prelude::*;
use serde::Serialize;
use serde_json::{json, Value};
use std::path::PathBuf;

pub struct Gathered {
    pub facts: Facts,
    pub platform: Platform,
    pub source: SourceInfo,
    pub panel: PanelInfo,
    pub inventory: Inventory,
    /// `system/<name>.json` documents.
    pub system_files: Vec<(String, Vec<u8>)>,
    /// Running containers that use a volume in the snapshot: frozen while
    /// their volumes are copied.
    pub pause: Vec<String>,
    pub notes: Vec<String>,
}

pub async fn gather(state: &AppState) -> Gathered {
    let mut notes: Vec<String> = Vec::new();
    let platform = platform().await;
    let panel = panel_info().await;
    let source = source(state).await;
    let users = system_users().await;
    let docker = docker().await;
    let tailscale = tailscale().await;
    let firewall = firewall().await;
    let settings = settings().await;

    let mut facts = Facts {
        home_dirs: users
            .iter()
            .map(|u| PathBuf::from(&u.home))
            .filter(|h| h.starts_with("/home/") && h.components().count() == 3)
            .collect(),
        ..Default::default()
    };
    for v in &docker.volumes {
        if is_anonymous_volume(&v.name) {
            notes.push(format!("Docker volume {} has no name and is left out", v.name));
            continue;
        }
        facts.docker_volumes.push((v.name.clone(), PathBuf::from(&v.mountpoint)));
    }
    if tokio::fs::metadata("/var/lib/redis").await.is_ok_and(|m| m.is_dir()) {
        facts.redis_dir = Some(PathBuf::from("/var/lib/redis"));
    }
    let kept: Vec<&str> = facts.docker_volumes.iter().map(|(n, _)| n.as_str()).collect();
    let pause: Vec<String> = docker
        .containers
        .iter()
        .filter(|c| c.running && c.volumes.iter().any(|v| kept.contains(&v.as_str())))
        .map(|c| c.name.clone())
        .collect();

    let mut inventory = inventory(state).await;
    inventory.system_users = users.iter().map(|u| u.name.clone()).collect();
    inventory.docker_volumes = kept.iter().map(|s| s.to_string()).collect();
    inventory.containers = docker.containers.iter().map(|c| c.name.clone()).collect();
    inventory.tailscale = tailscale.get("installed").and_then(Value::as_bool).unwrap_or(false);

    let system_files = vec![
        ("system/platform.json".to_string(), pretty(&platform)),
        ("system/users.json".to_string(), pretty(&users)),
        ("system/docker.json".to_string(), pretty(&docker.raw)),
        ("system/tailscale.json".to_string(), pretty(&tailscale)),
        ("system/firewall.json".to_string(), pretty(&firewall)),
        ("system/settings.json".to_string(), pretty(&settings)),
    ];
    Gathered { facts, platform, source, panel, inventory, system_files, pause, notes }
}

fn pretty<T: Serialize>(v: &T) -> Vec<u8> {
    serde_json::to_vec_pretty(v).unwrap_or_else(|_| b"null".to_vec())
}

async fn stdout(cmd: &str, args: &[&str]) -> Option<String> {
    exec::run(cmd, args).await.ok().map(|o| o.stdout.trim().to_string()).filter(|s| !s.is_empty())
}

/// This server's platform, for comparing with a snapshot's.
pub async fn gather_platform() -> Platform {
    platform().await
}

async fn platform() -> Platform {
    let release = tokio::fs::read_to_string("/etc/os-release").await.unwrap_or_default();
    let field = |key: &str| {
        release
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{key}=")))
            .map(|v| v.trim_matches('"').to_string())
            .unwrap_or_default()
    };
    Platform {
        os: field("ID"),
        os_version: field("VERSION_ID"),
        arch: stdout("uname", &["-m"]).await.unwrap_or_default(),
        mariadb: stdout("mariadb", &["--version"]).await,
        postgres: stdout("psql", &["--version"]).await,
        node: stdout("node", &["--version"]).await,
        docker: stdout("docker", &["version", "--format", "{{.Server.Version}}"]).await,
    }
}

/// The panel's own commit, in full, and whether its source has local
/// changes (then the commit alone does not say what wrote the file).
pub async fn panel_info() -> PanelInfo {
    let dir = crate::handlers::system::repo_dir();
    let commit = stdout("git", &["-C", &dir, "rev-parse", "HEAD"]).await.unwrap_or_default();
    let dirty = stdout("git", &["-C", &dir, "status", "--porcelain", "--untracked-files=no"]).await.is_some();
    PanelInfo { commit, dirty }
}

async fn source(state: &AppState) -> SourceInfo {
    let panel_host = crate::server_ip::panel_host_now(&state.config.panel_host).await;
    SourceInfo {
        hostname: stdout("hostname", &[]).await.unwrap_or_default(),
        ipv4: crate::server_ip::resolve_ipv4(None, &panel_host).await,
        ipv6: crate::server_ip::server_ipv6(&state.panel_db, &state.config.panel_host).await,
        panel_host,
    }
}

/// An account made on /users: what `useradd` + `chpasswd -e` need to make
/// it again on another server (its home folder travels in the "homes"
/// section).
#[derive(Serialize)]
pub struct SystemUser {
    pub name: String,
    pub uid: u32,
    pub home: String,
    pub shell: String,
    pub sudo: bool,
    pub groups: Vec<String>,
    /// The hash from /etc/shadow, so the password stays the same.
    pub password_hash: Option<String>,
}

/// Accounts with a uid of 1000 or more that are not the OS's or the
/// panel's own (`apps`, `vmail`, the panel user, `ubuntu`, ...).
async fn system_users() -> Vec<SystemUser> {
    const NOT_OPERATORS: &[&str] = &["apps", "vmail", "vps-panel", "nobody", "ubuntu"];
    let passwd = tokio::fs::read_to_string("/etc/passwd").await.unwrap_or_default();
    let shadow = tokio::fs::read_to_string("/etc/shadow").await.unwrap_or_default();
    let group = tokio::fs::read_to_string("/etc/group").await.unwrap_or_default();
    let mut users = Vec::new();
    for line in passwd.lines() {
        let f: Vec<&str> = line.split(':').collect();
        if f.len() < 7 {
            continue;
        }
        let Ok(uid) = f[2].parse::<u32>() else { continue };
        let name = f[0];
        if !(1000..65534).contains(&uid) || NOT_OPERATORS.contains(&name) {
            continue;
        }
        let password_hash = shadow
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{name}:")))
            .and_then(|rest| rest.split(':').next())
            .filter(|h| !h.is_empty())
            .map(str::to_string);
        let groups: Vec<String> = group
            .lines()
            .filter_map(|l| {
                let g: Vec<&str> = l.split(':').collect();
                (g.len() >= 4 && g[3].split(',').any(|m| m == name)).then(|| g[0].to_string())
            })
            .collect();
        users.push(SystemUser {
            name: name.to_string(),
            uid,
            home: f[5].to_string(),
            shell: f[6].to_string(),
            sudo: groups.iter().any(|g| g == "sudo"),
            groups,
            password_hash,
        });
    }
    users
}

struct Volume {
    name: String,
    mountpoint: String,
}

struct Container {
    name: String,
    running: bool,
    volumes: Vec<String>,
}

struct Docker {
    volumes: Vec<Volume>,
    containers: Vec<Container>,
    /// `docker inspect` of every container and volume, for the import.
    raw: Value,
}

/// Docker's own state is recorded, not copied: the import re-creates
/// volumes and containers through Docker. Images are recorded by reference
/// and digest and pulled or built again there.
async fn docker() -> Docker {
    let empty = Docker { volumes: vec![], containers: vec![], raw: json!({"installed": false}) };
    if stdout("docker", &["info", "--format", "{{.ServerVersion}}"]).await.is_none() {
        return empty;
    }
    let names: Vec<String> = stdout("docker", &["ps", "-a", "--format", "{{.Names}}"])
        .await
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    let vol_names: Vec<String> = stdout("docker", &["volume", "ls", "-q"])
        .await
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    let inspect = |args: Vec<String>| async move {
        if args.len() <= 2 {
            return Value::Array(vec![]);
        }
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        stdout("docker", &refs).await.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Array(vec![]))
    };
    let mut cargs = vec!["inspect".to_string(), "--type=container".to_string()];
    cargs.extend(names.iter().cloned());
    let containers_raw = inspect(cargs).await;
    let mut vargs = vec!["volume".to_string(), "inspect".to_string()];
    vargs.extend(vol_names.iter().cloned());
    let volumes_raw = inspect(vargs).await;

    let volumes = volumes_raw
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| {
                    Some(Volume {
                        name: v.get("Name")?.as_str()?.to_string(),
                        mountpoint: v.get("Mountpoint")?.as_str()?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let containers = containers_raw
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| {
                    let name = c.get("Name")?.as_str()?.trim_start_matches('/').to_string();
                    let running = c.pointer("/State/Running").and_then(Value::as_bool).unwrap_or(false);
                    let volumes = c
                        .get("Mounts")
                        .and_then(Value::as_array)
                        .map(|m| {
                            m.iter()
                                .filter(|m| m.get("Type").and_then(Value::as_str) == Some("volume"))
                                .filter_map(|m| m.get("Name").and_then(Value::as_str).map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    Some(Container { name, running, volumes })
                })
                .collect()
        })
        .unwrap_or_default();
    Docker {
        volumes,
        containers,
        raw: json!({"installed": true, "containers": containers_raw, "volumes": volumes_raw}),
    }
}

/// Docker names a volume nobody named with 64 hex digits.
fn is_anonymous_volume(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Tailscale's settings, never its node state: a copy of that would be a
/// second machine with this one's identity. The new server logs in itself.
async fn tailscale() -> Value {
    if stdout("sh", &["-c", "command -v tailscale"]).await.is_none() {
        return json!({"installed": false});
    }
    let prefs: Value = stdout("tailscale", &["debug", "prefs"])
        .await
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    json!({"installed": true, "prefs": prefs})
}

async fn firewall() -> Value {
    json!({
        "ufw_status": stdout("ufw", &["status", "verbose"]).await,
        "ufw_added": stdout("ufw", &["show", "added"]).await,
    })
}

/// What the import takes from this server's settings: the few `.env` keys
/// that are not per-install secrets, swap and the host name.
async fn settings() -> Value {
    const PORTABLE: &[&str] = &["MCP_TOKEN", "ADMIN_EMAIL", "ACME_EMAIL", "NETWORK_PUBLIC_URL", "PANEL_HOST"];
    let env = tokio::fs::read_to_string("/etc/vps-panel/.env").await.unwrap_or_default();
    let mut keys = serde_json::Map::new();
    for line in env.lines() {
        if let Some((k, v)) = line.split_once('=') {
            if PORTABLE.contains(&k.trim()) {
                keys.insert(k.trim().to_string(), Value::String(v.trim().to_string()));
            }
        }
    }
    let swaps = tokio::fs::read_to_string("/proc/swaps").await.unwrap_or_default();
    let swappiness = tokio::fs::read_to_string("/proc/sys/vm/swappiness").await.unwrap_or_default();
    json!({
        "env": keys,
        "hostname": stdout("hostname", &[]).await,
        "swaps": swaps.lines().skip(1).collect::<Vec<_>>(),
        "swappiness": swappiness.trim().parse::<u32>().ok(),
    })
}

async fn inventory(state: &AppState) -> Inventory {
    let mut inv = Inventory::default();
    let _ = state.panel_db.with(|conn| {
        let list = |sql: &str| -> Vec<String> {
            conn.prepare(sql)
                .and_then(|mut s| s.query_map([], |r| r.get::<_, String>(0)).and_then(|rows| rows.collect()))
                .unwrap_or_default()
        };
        inv.apps = list("SELECT name FROM apps ORDER BY name");
        inv.sites = list("SELECT domain FROM sites ORDER BY domain");
        inv.panel_users = conn.query_row("SELECT COUNT(*) FROM users", [], |r| r.get::<_, i64>(0)).unwrap_or(0) as u64;
        Ok(())
    });
    if let Ok(mut conn) = state.mariadb.pool().get_conn().await {
        inv.dns_zones = conn.query("SELECT name FROM pdns.domains ORDER BY name").await.unwrap_or_default();
        inv.mail_domains = conn.query("SELECT name FROM mail.virtual_domains ORDER BY name").await.unwrap_or_default();
        inv.mailboxes = conn
            .query_first::<u64, _>("SELECT COUNT(*) FROM mail.virtual_users")
            .await
            .ok()
            .flatten()
            .unwrap_or(0);
    }
    for dir in ["/etc/vps-panel/certs", "/etc/letsencrypt/live"] {
        if let Ok(mut rd) = tokio::fs::read_dir(dir).await {
            while let Ok(Some(e)) = rd.next_entry().await {
                let name = e.file_name().to_string_lossy().into_owned();
                if name != "README" && !inv.certs.contains(&name) {
                    inv.certs.push(name);
                }
            }
        }
    }
    inv.certs.sort();
    if let Ok(mut rd) = tokio::fs::read_dir("/etc/cron.d").await {
        while let Ok(Some(e)) = rd.next_entry().await {
            if e.file_name().to_string_lossy().starts_with("vps-panel-") {
                inv.cron_jobs += 1;
            }
        }
    }
    inv
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anonymous_volumes_are_told_apart() {
        assert!(is_anonymous_volume(&"a1".repeat(32)));
        assert!(!is_anonymous_volume("n8n_data"));
        assert!(!is_anonymous_volume("vps-app-shop-data"));
    }
}
