mod auth;
mod config;
mod db;
mod error;
mod exec;
mod fs_atomic;
mod handlers;
mod ipc;
mod nginx_template;
mod notify;
mod server_ip;
mod state;
mod systemd_template;
mod validate;

use crate::config::Config;
use crate::db::{MariaDb, PanelDb};
use crate::ipc::tasks::TaskTracker;
use crate::state::AppState;
use anyhow::{Context, Result};
use axum::{middleware, Router};
use std::sync::Arc;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        None | Some("serve") => serve().await,
        Some("admin-reset") => admin_reset(args.collect()).await,
        Some("access-reset") => access_reset().await,
        Some(other) => {
            eprintln!("unknown subcommand: {other}");
            std::process::exit(2);
        }
    }
}

async fn serve() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let config = Arc::new(Config::from_env().context("loading config")?);
    info!(socket = %config.socket_path.display(), "vps-panel-agent starting");

    let panel_db = PanelDb::open(&config.panel_db_path)?;
    notify::init(&config.panel_host);
    ensure_agent_schema(&panel_db)?;

    let mariadb = MariaDb::connect(&config.mariadb_url)?;
    let tasks = TaskTracker::new(panel_db.clone());
    // Reconcile any task rows left in 'pending' / 'running' from a previous
    // agent process. Self-update tasks resolve to 'succeeded' (reaching this
    // boot IS the success condition); anything else resolves to 'failed'
    // (we can't know the outcome of an interrupted task).
    tasks.sweep_interrupted();

    let state = AppState {
        config: config.clone(),
        panel_db,
        mariadb,
        tasks,
    };

    let app = handlers::router(state.clone())
        .layer(middleware::from_fn_with_state(state.clone(), auth::bearer_auth));

    tokio::spawn(handlers::ssl::renewal_loop(state.clone()));
    // Settles a panel self-update that restarted this agent part-way through.
    tokio::spawn(handlers::system::finalize_self_update(state.clone()));
    // Backfills /srv/drive on boxes installed before the Drive root existed.
    handlers::files::ensure_drive_dir().await;
    handlers::apps::ensure_app_unit_defaults().await;
    // Server names longer than 46 characters fail nginx -t without it.
    handlers::nginx::ensure_http_defaults().await;
    // The panel vhost includes the access allowlist; nginx -t fails without it.
    if let Err(e) = handlers::panel_access::ensure_file().await {
        tracing::warn!(error = %e, "could not create the panel access file");
    }
    // Collects /files trash entries past their 30-day retention.
    tokio::spawn(handlers::files::purge_loop());
    // Failed units and a filling disk, for /notifications.
    tokio::spawn(notify::monitor_loop(state.panel_db.clone()));
    // Task rows keep whole build logs; drop them after the retention window.
    let tasks = state.tasks.clone();
    tokio::spawn(async move {
        loop {
            tasks.prune_old();
            tokio::time::sleep(std::time::Duration::from_secs(86_400)).await;
        }
    });

    bind_and_serve(&config, app).await
}

#[cfg(unix)]
async fn bind_and_serve(config: &Config, app: Router) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    use tokio::net::UnixListener;

    if let Some(parent) = config.socket_path.parent() {
        std::fs::create_dir_all(parent).context("create socket parent dir")?;
    }
    let _ = std::fs::remove_file(&config.socket_path);

    let listener = UnixListener::bind(&config.socket_path).context("bind unix socket")?;

    let perms = std::fs::Permissions::from_mode(0o660);
    std::fs::set_permissions(&config.socket_path, perms).context("chmod 0660")?;

    if config.vps_panel_gid != 0 {
        nix::unistd::chown(
            config.socket_path.as_path(),
            None,
            Some(nix::unistd::Gid::from_raw(config.vps_panel_gid)),
        )
        .context("chown socket to vps-panel group")?;
    }

    info!("listening on unix:{}", config.socket_path.display());
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<crate::auth::PeerCred>(),
    )
    .await
    .context("axum::serve")?;
    Ok(())
}

#[cfg(not(unix))]
async fn bind_and_serve(_config: &Config, _app: Router) -> Result<()> {
    anyhow::bail!("vps-panel-agent only runs on unix targets");
}

fn ensure_agent_schema(db: &PanelDb) -> Result<()> {
    db.with(|conn| {
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS tasks (
                id           TEXT PRIMARY KEY,
                kind         TEXT NOT NULL,
                status       TEXT NOT NULL,
                started_at   TEXT NOT NULL,
                finished_at  TEXT,
                log_blob     TEXT NOT NULL DEFAULT ''
            );
            "#,
        )
    })
    .context("ensure agent schema")?;
    Ok(())
}

/// `vps-panel-agent access-reset`: lift the panel access allowlist when it
/// locks the operator out (a new home IP). Run as root on the server.
async fn access_reset() -> Result<()> {
    use handlers::panel_access::{ACCESS_CONF, EMPTY_BODY};
    std::fs::write(ACCESS_CONF, EMPTY_BODY).context("write the access file")?;
    let test = std::process::Command::new("nginx").arg("-t").status()?;
    anyhow::ensure!(test.success(), "nginx -t failed; fix nginx before reloading");
    std::process::Command::new("systemctl").args(["reload", "nginx"]).status()?;
    let path: std::path::PathBuf = std::env::var("PANEL_DB_PATH")
        .unwrap_or_else(|_| "/var/lib/vps-panel/panel.db".to_string())
        .into();
    let conn = rusqlite::Connection::open(&path).context("open panel db")?;
    conn.execute("DELETE FROM settings WHERE key = 'panel.access_allowlist'", [])?;
    println!("panel access allowlist cleared: every address can reach the panel again.");
    Ok(())
}

async fn admin_reset(args: Vec<String>) -> Result<()> {
    use argon2::password_hash::{rand_core::OsRng, PasswordHasher, SaltString};
    use argon2::Argon2;
    use std::io::Read;

    let mut email: Option<String> = None;
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--email" => email = iter.next(),
            other => anyhow::bail!("unknown arg: {other}"),
        }
    }
    let email = email.context("--email is required")?;

    eprint!("New password for {email}: ");
    let mut password = String::new();
    std::io::stdin().read_to_string(&mut password)?;
    let password = password.trim();
    if password.len() < 12 {
        anyhow::bail!("password must be at least 12 characters");
    }

    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("argon2: {e}"))?
        .to_string();

    let path: std::path::PathBuf = std::env::var("PANEL_DB_PATH")
        .unwrap_or_else(|_| "/var/lib/vps-panel/panel.db".to_string())
        .into();
    let conn = rusqlite::Connection::open(&path).context("open panel db")?;
    let updated = conn.execute(
        "UPDATE users SET password_hash = ?1, totp_enabled = 0, totp_secret = NULL WHERE email = ?2",
        rusqlite::params![hash, email],
    )?;
    if updated == 0 {
        anyhow::bail!("no user found with email {email}");
    }
    println!("password reset for {email}; TOTP cleared.");
    Ok(())
}
