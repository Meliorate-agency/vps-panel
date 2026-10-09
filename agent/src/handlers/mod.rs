pub mod app_git;
pub mod apps;
pub mod backups;
pub mod cron;
pub mod databases;
pub mod diag;
pub mod dns;
pub mod files;
pub mod firewall;
pub mod health;
pub mod hostname;
pub mod logs;
pub mod mail;
pub mod nginx;
pub mod panel_access;
pub mod postgres;
pub mod quick_install;
pub mod redirects;
pub mod sites;
pub mod ssh;
pub mod ssl;
pub mod swap;
pub mod system;
pub mod systemd;
pub mod tailscale;
pub mod terminal;
pub mod tasks;

use crate::state::AppState;
use axum::{
    routing::{delete, get, patch, post, put},
    Router,
};

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health::get))
        .route("/sysinfo", get(diag::sysinfo))
        // Apps
        .route("/apps", get(apps::list).post(apps::create))
        .route("/apps/{name}", get(apps::get).delete(apps::delete))
        .route("/apps/{name}/start", post(apps::start))
        .route("/apps/{name}/stop", post(apps::stop))
        .route("/apps/{name}/restart", post(apps::restart))
        .route("/apps/{name}/update", post(apps::update))
        .route("/apps/{name}/env", patch(apps::patch_env))
        .route("/apps/{name}/entrypoint", patch(apps::set_entrypoint))
        .route("/apps/{name}/routing", patch(apps::set_routing))
        .route(
            "/apps/{name}/env-files",
            get(apps::list_env_files)
                .put(apps::save_env_file)
                .delete(apps::delete_env_file),
        )
        .route("/apps/{name}/git-status", get(apps::git_status))
        .route("/apps/{name}/source-state", get(apps::source_state))
        // Files (visual filesystem browser — jailed roots + trash)
        .route("/files/roots", get(files::roots))
        .route("/files/list", get(files::list))
        .route("/files/mkdir", post(files::mkdir))
        .route("/files/new-file", post(files::new_file))
        .route("/files/rename", post(files::rename))
        .route("/files/move", post(files::move_items))
        .route("/files/upload", post(files::upload))
        .route("/files/download", get(files::download))
        .route(
            "/files/trash",
            get(files::trash_list).post(files::trash_items),
        )
        .route("/files/trash/restore", post(files::trash_restore))
        .route("/files/trash/empty", post(files::trash_empty))
        .route("/files/trash/{id}", delete(files::trash_purge_one))
        // Terminal (streamed one-shot shell execution)
        .route("/terminal/exec", post(terminal::exec))
        // Nginx + system services
        .route("/nginx/reload", post(nginx::reload))
        // Sites (domain → backend port routing)
        .route("/sites", get(sites::list).post(sites::create))
        .route(
            "/sites/{domain}",
            patch(sites::update).delete(sites::delete),
        )
        .route("/sites/{domain}/status", get(sites::status))
        // Redirects (301/302 rules rendered into a per-domain nginx include)
        .route("/redirects/{domain}", put(redirects::put))
        .route("/services", get(systemd::list_services))
        .route(
            "/services/{unit}/{action}",
            post(systemd::service_action),
        )
        // SSL
        .route("/ssl", get(ssl::list))
        .route("/ssl/issue", post(ssl::issue))
        .route("/ssl/renew", post(ssl::renew))
        // DNS
        .route("/dns/zones", get(dns::list_zones).post(dns::create_zone))
        .route("/dns/zones/{zone}", delete(dns::delete_zone))
        .route("/dns/zones/{zone}/sign", post(dns::sign_zone))
        .route("/dns/zones/{zone}/reconcile", post(dns::reconcile_zone))
        .route(
            "/dns/zones/{zone}/records",
            get(dns::list_records).post(dns::create_record),
        )
        .route(
            "/dns/records/{id}",
            patch(dns::update_record).delete(dns::delete_record),
        )
        // Mail
        .route("/mail/domains", post(mail::create_domain).get(mail::list_domains))
        .route("/mail/mailboxes", post(mail::create_mailbox).get(mail::list_mailboxes))
        .route(
            "/mail/mailboxes/{email}",
            patch(mail::update_mailbox).delete(mail::delete_mailbox),
        )
        .route("/mail/mailboxes/{email}/usage", get(mail::mailbox_usage))
        // Make sure a valid cert covers mail.<tenant> (re-issue site cert with
        // the SAN, or a standalone mail.<tenant> cert). Returns a task id.
        .route("/mail/domains/{name}/ssl", post(mail::ensure_ssl))
        // Databases — MariaDB
        .route("/db/users", post(databases::create_user).get(databases::list_users))
        .route("/db/databases", post(databases::create_database).get(databases::list_databases))
        .route("/db/import", post(databases::import_sql))
        .route("/db/grant", post(databases::grant))
        .route("/db/revoke", post(databases::revoke))
        .route("/db/users/{user}", delete(databases::delete_user))
        .route(
            "/db/users/{user}/password",
            post(databases::set_user_password),
        )
        .route("/db/databases/{name}", delete(databases::delete_database))
        // Databases — PostgreSQL (separate prefix so MariaDB endpoints
        // stay untouched). Mirrors the MariaDB shape; the Postgres handler
        // shells out to `sudo -u postgres psql` over peer auth.
        .route("/pg/users", post(postgres::create_role).get(postgres::list_users))
        .route(
            "/pg/databases",
            post(postgres::create_database).get(postgres::list_databases),
        )
        .route("/pg/grant", post(postgres::grant))
        .route("/pg/users/{user}", delete(postgres::delete_role))
        .route(
            "/pg/users/{user}/password",
            post(postgres::set_role_password),
        )
        .route(
            "/pg/databases/{name}",
            delete(postgres::delete_database).patch(postgres::update_database),
        )
        // pgvector: per-database enable/disable + a status probe the
        // databases page uses to render each row's Enable/Enabled control.
        .route("/pg/pgvector", get(postgres::pgvector_status))
        .route("/pg/pgvector/enable", post(postgres::enable_pgvector))
        .route("/pg/pgvector/disable", post(postgres::disable_pgvector))
        // Backups
        .route("/backups", get(backups::list).post(backups::run))
        .route("/backups/run", post(backups::run))
        .route("/backups/restore", post(backups::restore))
        .route("/notifications/test", post(crate::notify::test))
        .route("/notifications/send", post(crate::notify::send_event))
        // Firewall
        .route("/firewall/status", get(firewall::status))
        .route("/firewall/allow", post(firewall::allow))
        .route("/firewall/deny", post(firewall::deny))
        .route("/firewall/enable", post(firewall::enable_ufw))
        .route(
            "/firewall/sync-operator-ignoreip",
            post(firewall::sync_operator_ignoreip),
        )
        .route(
            "/fail2ban/policy",
            post(firewall::set_fail2ban_policy),
        )
        .route("/fail2ban/jails", get(firewall::jails))
        .route("/fail2ban/unban", post(firewall::unban))
        .route("/fail2ban/unban-all", post(firewall::unban_all))
        .route(
            "/fail2ban/jails/{jail}/failed-by-ip",
            get(firewall::failed_by_ip),
        )
        // SSH keys + server config
        .route("/ssh/keys", get(ssh::list).post(ssh::add))
        .route("/ssh/keys/{fp}", delete(ssh::remove))
        .route("/ssh/keys/{fp}/public", get(ssh::get_public_key).put(ssh::update_public_key))
        .route("/ssh/keypair", post(ssh::generate_keypair))
        .route(
            "/ssh/keypair/private",
            get(ssh::get_private_key).put(ssh::update_private_key),
        )
        .route(
            "/ssh/files/authorized_keys",
            get(ssh::get_authorized_keys_file).put(ssh::put_authorized_keys_file),
        )
        .route(
            "/ssh/files/config",
            get(ssh::get_ssh_config_file).put(ssh::put_ssh_config_file),
        )
        .route("/ssh/config", get(ssh::config_get).patch(ssh::config_set))
        // Cron
        .route("/cron/jobs", get(cron::list).post(cron::create))
        .route("/cron/jobs/{id}", delete(cron::delete))
        // System (first-run wizard: service health + bulk apt upgrade + hostname change + self-update)
        .route("/system/services", get(system::services_status))
        .route(
            "/system/users",
            get(system::list_system_users).post(system::create_system_user),
        )
        .route(
            "/system/users/{name}",
            delete(system::delete_system_user),
        )
        .route(
            "/system/users/{name}/password",
            post(system::set_system_user_password),
        )
        .route("/system/apt-upgrade", post(system::apt_upgrade))
        .route("/system/panel-host", post(system::set_panel_host))
        .route("/system/panel-access", put(panel_access::set))
        .route("/system/root-password", post(system::set_root_password))
        .route("/system/update-check", get(system::update_check))
        .route("/system/self-update", post(system::self_update))
        // Swap file (/swapfile) + vm.swappiness
        .route("/system/swap", get(swap::get).put(swap::set))
        // The server's own hostname (hostnamectl + /etc/hosts + cloud-init)
        .route("/system/hostname", get(hostname::get).put(hostname::set))
        // Quick install (apt + systemd recipes for common services)
        .route("/quick-install", get(quick_install::list))
        .route("/quick-install/{slug}", post(quick_install::run))
        .route("/quick-install/{slug}/bind", post(quick_install::set_bind))
        .route(
            "/quick-install/roundcube/settings",
            get(quick_install::get_roundcube_settings)
                .post(quick_install::update_roundcube_settings),
        )
        .route(
            "/quick-install/n8n/settings",
            get(quick_install::get_n8n_settings)
                .post(quick_install::update_n8n_settings),
        )
        .route(
            "/quick-install/n8n/update",
            post(quick_install::update_n8n),
        )
        .route(
            "/quick-install/phpmyadmin/settings",
            get(quick_install::get_phpmyadmin_settings)
                .post(quick_install::update_phpmyadmin_settings),
        )
        // Tailscale (the quick-install recipe installs it; login, settings and
        // connect/disconnect/logout go through the tailscale CLI)
        .route("/tailscale/status", get(tailscale::status))
        .route("/tailscale/login", post(tailscale::login))
        .route("/tailscale/settings", put(tailscale::settings))
        .route("/tailscale/connect", post(tailscale::connect))
        .route("/tailscale/disconnect", post(tailscale::disconnect))
        .route("/tailscale/logout", post(tailscale::logout))
        // Tasks + log streams
        .route("/tasks", get(tasks::list))
        .route("/tasks/{id}", get(tasks::get))
        .route("/tasks/{id}/stream", get(tasks::stream))
        .route("/logs/{unit}/stream", get(logs::stream))
        .with_state(state)
}
