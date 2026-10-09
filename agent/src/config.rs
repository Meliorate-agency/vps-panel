use anyhow::{anyhow, Context, Result};
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
    pub socket_path: PathBuf,
    pub agent_token: String,
    pub panel_db_path: PathBuf,
    pub mariadb_url: String,
    pub restic_password: String,
    pub restic_repository: String,
    pub panel_host: String,
    pub acme_email: String,
    pub acme_directory: String,
    pub vps_panel_gid: u32,
    /// The panel's uid: with root, the only peer the agent socket serves.
    pub vps_panel_uid: Option<u32>,
    pub apps_uid: u32,
    pub apps_gid: u32,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let agent_token = std::env::var("AGENT_TOKEN").context("AGENT_TOKEN missing")?;
        if agent_token.len() < 32 {
            return Err(anyhow!("AGENT_TOKEN too short (need >= 32 chars)"));
        }

        let socket_path = std::env::var("AGENT_SOCKET")
            .unwrap_or_else(|_| "/run/vps-panel/agent.sock".to_string())
            .into();

        let panel_db_path = std::env::var("PANEL_DB_PATH")
            .unwrap_or_else(|_| "/var/lib/vps-panel/panel.db".to_string())
            .into();

        let mariadb_url = std::env::var("MARIADB_URL").unwrap_or_else(|_| {
            "mysql://root@localhost:3306/mysql?socket=/var/run/mysqld/mysqld.sock".to_string()
        });

        let restic_password =
            std::env::var("RESTIC_PASSWORD").context("RESTIC_PASSWORD missing")?;
        let restic_repository = std::env::var("RESTIC_REPOSITORY")
            .unwrap_or_else(|_| "/var/backups/vps-panel".to_string());

        let panel_host = std::env::var("PANEL_HOST").context("PANEL_HOST missing")?;
        let acme_email = std::env::var("ACME_EMAIL").unwrap_or_else(|_| format!("admin@{panel_host}"));
        let acme_directory = std::env::var("ACME_DIRECTORY").unwrap_or_else(|_| {
            "https://acme-v02.api.letsencrypt.org/directory".to_string()
        });

        let vps_panel_gid = lookup_gid("vps-panel").unwrap_or(0);
        let vps_panel_uid = lookup_uid("vps-panel");
        let apps_uid = lookup_uid("apps").unwrap_or(0);
        let apps_gid = lookup_gid("apps").unwrap_or(0);

        Ok(Config {
            socket_path,
            agent_token,
            panel_db_path,
            mariadb_url,
            restic_password,
            restic_repository,
            panel_host,
            acme_email,
            acme_directory,
            vps_panel_gid,
            vps_panel_uid,
            apps_uid,
            apps_gid,
        })
    }
}

#[cfg(unix)]
fn lookup_uid(name: &str) -> Option<u32> {
    use nix::unistd::User;
    User::from_name(name).ok().flatten().map(|u| u.uid.as_raw())
}

#[cfg(unix)]
fn lookup_gid(name: &str) -> Option<u32> {
    use nix::unistd::Group;
    Group::from_name(name).ok().flatten().map(|g| g.gid.as_raw())
}

#[cfg(not(unix))]
fn lookup_uid(_name: &str) -> Option<u32> {
    None
}

#[cfg(not(unix))]
fn lookup_gid(_name: &str) -> Option<u32> {
    None
}
