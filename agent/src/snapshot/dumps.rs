//! The databases of a server snapshot, dumped into the export's staging
//! folder before the archive is written.
//!
//! Not one `--all-databases` dump: that one carries the `mysql` schema, and
//! replaying it on another server would replace that server's own `pdns` and
//! `mail` accounts, whose passwords live in its PowerDNS, Postfix and Dovecot
//! configuration. So: the operators' accounts and grants, one dump per
//! operator database, and the panel's `pdns` / `mail` schemas as data only.

use crate::error::{AgentError, AgentResult};
use crate::exec;
use crate::ipc::tasks::TaskHandle;
use crate::state::AppState;
use mysql_async::prelude::*;
use std::path::{Path, PathBuf};

/// One staged file and where it goes in the archive.
pub struct Staged {
    pub archive_path: String,
    pub path: PathBuf,
}

#[derive(Default)]
pub struct Dumps {
    pub files: Vec<Staged>,
    pub mariadb_databases: Vec<String>,
    pub mariadb_users: u64,
    pub postgres_databases: Vec<String>,
}

pub async fn dump_all(state: &AppState, staging: &Path, task: &TaskHandle) -> AgentResult<Dumps> {
    let mut dumps = Dumps::default();
    dump_mariadb(state, staging, task, &mut dumps).await?;
    dump_postgres(staging, task, &mut dumps).await?;
    dump_panel(state, staging, task, &mut dumps).await?;
    Ok(dumps)
}

async fn dump_mariadb(state: &AppState, staging: &Path, task: &TaskHandle, dumps: &mut Dumps) -> AgentResult<()> {
    let mut conn = state.mariadb.pool().get_conn().await?;

    // Accounts and grants, without MariaDB's own accounts and the panel's
    // service accounts (databases::RESERVED_USERS). Roles have no CREATE USER.
    let accounts: Vec<(String, String, String)> = match conn
        .query("SELECT User, Host, IFNULL(is_role, 'N') FROM mysql.user")
        .await
    {
        Ok(rows) => rows,
        Err(_) => conn
            .query::<(String, String), _>("SELECT User, Host FROM mysql.user")
            .await?
            .into_iter()
            .map(|(u, h)| (u, h, "N".to_string()))
            .collect(),
    };
    let mut sql = String::from("-- MariaDB accounts and grants (vps-panel server snapshot)\n");
    for (user, host, is_role) in &accounts {
        if is_role == "Y" || crate::handlers::databases::is_reserved_user(user) {
            continue;
        }
        let account = crate::handlers::databases::quote_account(user, host);
        let create: Option<String> = match conn.query_first(format!("SHOW CREATE USER {account}")).await {
            Ok(v) => v,
            Err(e) => {
                task.log(format!("account {user}@{host} skipped: {e}"));
                continue;
            }
        };
        let Some(create) = create else { continue };
        sql.push_str(&if_not_exists(&create));
        sql.push_str(";\n");
        let grants: Vec<String> = conn.query(format!("SHOW GRANTS FOR {account}")).await.unwrap_or_default();
        for grant in grants {
            sql.push_str(&grant);
            sql.push_str(";\n");
        }
        dumps.mariadb_users += 1;
    }
    let users_path = staging.join("mariadb-users.sql");
    crate::fs_atomic::write(&users_path, sql.as_bytes(), 0o600).await?;
    dumps.files.push(Staged { archive_path: "db/mariadb/users.sql".into(), path: users_path });

    let databases: Vec<String> = conn.query("SHOW DATABASES").await?;
    drop(conn);
    for db in databases {
        if crate::handlers::databases::is_reserved_db(&db) {
            continue;
        }
        let out = staging.join(format!("mariadb-{}.sql.zst", file_safe(&db)));
        task.log(format!("dumping MariaDB database {db}"));
        dump_to_zst(
            &out,
            "mariadb-dump",
            &[
                "--single-transaction",
                "--quick",
                "--lock-tables=0",
                "--routines",
                "--events",
                "--triggers",
                "--hex-blob",
                "--databases",
                &db,
            ],
        )
        .await?;
        dumps.files.push(Staged { archive_path: format!("db/mariadb/{}.sql.zst", file_safe(&db)), path: out });
        dumps.mariadb_databases.push(db);
    }

    // The panel's DNS and mail data, rows only: the importing server has the
    // schemas already (made by its installer, with its own accounts), and a
    // newer schema there still takes these rows (column names spelled out).
    for schema in ["pdns", "mail"] {
        if !schema_exists(state, schema).await {
            continue;
        }
        let out = staging.join(format!("{schema}.data.sql.zst"));
        task.log(format!("dumping the {schema} data"));
        dump_to_zst(
            &out,
            "mariadb-dump",
            &[
                "--single-transaction",
                "--quick",
                "--lock-tables=0",
                "--no-create-info",
                "--complete-insert",
                "--skip-triggers",
                "--hex-blob",
                schema,
            ],
        )
        .await?;
        dumps.files.push(Staged { archive_path: format!("db/mariadb/{schema}.data.sql.zst"), path: out });
    }
    Ok(())
}

/// `CREATE USER` -> `CREATE USER IF NOT EXISTS`, so loading the accounts
/// twice (a resumed import) does not stop at the first one.
pub(crate) fn if_not_exists(create: &str) -> String {
    let trimmed = create.trim_start();
    let upper = trimmed.to_ascii_uppercase();
    if upper.starts_with("CREATE USER") && !upper.starts_with("CREATE USER IF NOT EXISTS") {
        format!("CREATE USER IF NOT EXISTS{}", &trimmed["CREATE USER".len()..])
    } else {
        trimmed.to_string()
    }
}

async fn schema_exists(state: &AppState, schema: &str) -> bool {
    let Ok(mut conn) = state.mariadb.pool().get_conn().await else { return false };
    conn.exec_first::<String, _, _>(
        "SELECT SCHEMA_NAME FROM information_schema.SCHEMATA WHERE SCHEMA_NAME = ?",
        (schema,),
    )
    .await
    .ok()
    .flatten()
    .is_some()
}

/// A database name as a file name (names may hold almost anything).
fn file_safe(name: &str) -> String {
    let mut out = String::new();
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `cmd args | zstd > out`, root-only, failing when either side fails.
async fn dump_to_zst(out: &Path, cmd: &str, args: &[&str]) -> AgentResult<()> {
    let target = out.to_string_lossy().into_owned();
    let mut argv: Vec<&str> = vec![
        "-c",
        r#"set -o pipefail; umask 077; out="$1"; shift; "$@" | zstd -q -3 -T0 -c > "$out""#,
        "snapshot-dump",
        target.as_str(),
        cmd,
    ];
    argv.extend_from_slice(args);
    exec::run("bash", argv).await.map(|_| ())
}

async fn postgres_running() -> bool {
    exec::run("systemctl", ["is-active", "--quiet", "postgresql"]).await.is_ok()
}

async fn dump_postgres(staging: &Path, task: &TaskHandle, dumps: &mut Dumps) -> AgentResult<()> {
    if !postgres_running().await {
        return Ok(());
    }
    task.log("dumping PostgreSQL roles");
    let roles = exec::run("runuser", ["-u", "postgres", "--", "pg_dumpall", "--roles-only"]).await?;
    let roles_path = staging.join("postgres-roles.sql");
    crate::fs_atomic::write(&roles_path, filter_pg_roles(&roles.stdout).as_bytes(), 0o600).await?;
    dumps.files.push(Staged { archive_path: "db/postgres/roles.sql".into(), path: roles_path });

    let list = exec::run(
        "runuser",
        [
            "-u",
            "postgres",
            "--",
            "psql",
            "-XAt",
            "-c",
            "SELECT datname FROM pg_database WHERE datallowconn AND NOT datistemplate ORDER BY datname",
        ],
    )
    .await?;
    for db in list.stdout.lines().map(str::trim).filter(|d| !d.is_empty()) {
        if crate::handlers::postgres::is_reserved_db(db) {
            continue;
        }
        task.log(format!("dumping PostgreSQL database {db}"));
        let out = staging.join(format!("postgres-{}.dump", file_safe(db)));
        let target = out.to_string_lossy().into_owned();
        // -Fc: compressed, restorable table by table, with --create the
        // database itself (owner, encoding, settings).
        exec::run(
            "sh",
            [
                "-c",
                r#"umask 077 && runuser -u postgres -- pg_dump -Fc --create -- "$2" > "$1""#,
                "pg-dump",
                target.as_str(),
                db,
            ],
        )
        .await?;
        dumps.files.push(Staged { archive_path: format!("db/postgres/{}.dump", file_safe(db)), path: out });
        dumps.postgres_databases.push(db.to_string());
    }
    Ok(())
}

/// pg_dumpall --roles-only without the roles PostgreSQL makes itself
/// (`postgres`, `pg_*`): the importing server has them already, and
/// re-creating `postgres` fails.
pub(crate) fn filter_pg_roles(dump: &str) -> String {
    let mut out = String::new();
    for line in dump.lines() {
        if let Some(role) = role_of(line) {
            if crate::handlers::postgres::is_reserved_user(&role) {
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// The role a `CREATE ROLE` / `ALTER ROLE` / `COMMENT ON ROLE` line is about.
fn role_of(line: &str) -> Option<String> {
    let rest = ["CREATE ROLE ", "ALTER ROLE ", "COMMENT ON ROLE "]
        .iter()
        .find_map(|p| line.strip_prefix(p))?;
    if let Some(quoted) = rest.strip_prefix('"') {
        // "name" with "" for a quote inside.
        let mut name = String::new();
        let mut chars = quoted.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    name.push('"');
                } else {
                    break;
                }
            } else {
                name.push(c);
            }
        }
        Some(name)
    } else {
        Some(rest.split([' ', ';']).next().unwrap_or("").to_string())
    }
}

/// A consistent copy of panel.db without what only means something here:
/// signed-in sessions, sign-in attempts (addresses) and task logs.
async fn dump_panel(state: &AppState, staging: &Path, task: &TaskHandle, dumps: &mut Dumps) -> AgentResult<()> {
    task.log("copying panel.db");
    let path = staging.join("panel.db");
    let target = path.to_string_lossy().into_owned();
    state
        .panel_db
        .with(|conn| conn.execute("VACUUM INTO ?1", rusqlite::params![target]).map(|_| ()))?;
    let copy = path.clone();
    tokio::task::spawn_blocking(move || -> AgentResult<()> {
        let conn = rusqlite::Connection::open(&copy)?;
        for table in ["sessions", "login_attempts", "tasks"] {
            // A table an older panel never had is fine.
            let _ = conn.execute(&format!("DELETE FROM {table}"), []);
        }
        conn.execute_batch("VACUUM")?;
        Ok(())
    })
    .await
    .map_err(|e| AgentError::Other(anyhow::anyhow!("panel.db copy: {e}")))??;
    crate::fs_atomic::chmod(&path, 0o600).await?;
    dumps.files.push(Staged { archive_path: "panel/panel.db".into(), path });

    // My Networks: carried so a server that REPLACES this one can keep its
    // place in its networks. The import only takes it when told so.
    let key = state.config.panel_db_path.with_file_name("node-identity.key");
    if tokio::fs::metadata(&key).await.is_ok() {
        let staged = staging.join("node-identity.key");
        tokio::fs::copy(&key, &staged).await?;
        crate::fs_atomic::chmod(&staged, 0o600).await?;
        dumps.files.push(Staged { archive_path: "panel/node-identity.key".into(), path: staged });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_user_becomes_if_not_exists() {
        assert_eq!(
            if_not_exists("CREATE USER `shop`@`localhost` IDENTIFIED BY PASSWORD '*ABC'"),
            "CREATE USER IF NOT EXISTS `shop`@`localhost` IDENTIFIED BY PASSWORD '*ABC'"
        );
        assert_eq!(
            if_not_exists("CREATE USER IF NOT EXISTS `a`@`%`"),
            "CREATE USER IF NOT EXISTS `a`@`%`"
        );
    }

    #[test]
    fn postgres_roles_without_the_builtin_ones() {
        let dump = "SET default_transaction_read_only = off;\n\
                    CREATE ROLE postgres;\n\
                    ALTER ROLE postgres WITH SUPERUSER INHERIT;\n\
                    CREATE ROLE shop;\n\
                    ALTER ROLE shop WITH LOGIN PASSWORD 'SCRAM-SHA-256$x';\n\
                    CREATE ROLE \"Odd \"\"Name\";\n\
                    COMMENT ON ROLE pg_monitor IS 'x';\n\
                    GRANT pg_read_all_data TO shop;\n";
        let out = filter_pg_roles(dump);
        assert!(!out.contains("CREATE ROLE postgres;"));
        assert!(!out.contains("ALTER ROLE postgres"));
        assert!(!out.contains("COMMENT ON ROLE pg_monitor"));
        assert!(out.contains("CREATE ROLE shop;"));
        assert!(out.contains("ALTER ROLE shop WITH LOGIN"));
        assert!(out.contains("CREATE ROLE \"Odd \"\"Name\";"));
        assert!(out.contains("GRANT pg_read_all_data TO shop;"));
        assert!(out.starts_with("SET default_transaction_read_only"));
    }

    #[test]
    fn database_names_as_file_names() {
        assert_eq!(file_safe("shop_db-1"), "shop_db-1");
        assert_eq!(file_safe("a/b.c"), "a%2Fb%2Ec");
        assert_ne!(file_safe("é"), file_safe("ĩ"));
    }
}
