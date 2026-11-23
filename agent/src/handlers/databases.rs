use crate::error::{AgentError, AgentResult};
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    Json,
};
use mysql_async::prelude::*;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

#[derive(Deserialize)]
pub struct CreateUser {
    pub user: String,
    pub password: Option<String>,
    #[serde(default = "default_host")]
    pub host: String,
}

#[derive(Deserialize)]
pub struct CreateDatabase {
    pub name: String,
}

#[derive(Deserialize)]
pub struct Grant {
    pub user: String,
    pub host: Option<String>,
    pub database: String,
    #[serde(default = "default_priv")]
    pub privileges: String,
}

#[derive(Serialize)]
pub struct UserRow {
    pub user: String,
    pub host: String,
}

#[derive(Serialize)]
pub struct GrantedUser {
    pub user: String,
    pub host: String,
}

#[derive(Serialize)]
pub struct DatabaseRow {
    pub name: String,
    /// user@host pairs holding db-level grants on this schema (from
    /// `mysql.db`). Exact-name grants only: global `ON *.*` grants never
    /// appear in mysql.db, and wildcard-pattern grants (`ON \`%\`.*`) are
    /// deliberately not attached to every matching row — both would list
    /// the same user on every database, which reads as noise. The UI
    /// documents this next to the column.
    pub users: Vec<GrantedUser>,
}

#[derive(Deserialize)]
pub struct Revoke {
    pub user: String,
    pub host: Option<String>,
    pub database: String,
}

#[derive(Serialize)]
pub struct CreateUserResp {
    pub user: String,
    pub host: String,
    pub initial_password: String,
}

#[derive(Deserialize)]
pub struct SetUserPassword {
    #[serde(default = "default_host")]
    pub host: String,
    /// Omitted → server-generated random password.
    pub password: Option<String>,
}

#[derive(Serialize)]
pub struct SetUserPasswordResp {
    pub user: String,
    pub host: String,
    pub password: String,
}

fn default_host() -> String {
    "localhost".into()
}

fn default_priv() -> String {
    "ALL PRIVILEGES".into()
}

// Users we will not let an operator create, drop, or see in the list.
// Includes the MariaDB system accounts and the panel's own service accounts
// (pdns/mail/panel) that other parts of the stack rely on.
const RESERVED_USERS: &[&str] = &[
    "",
    "root",
    "mariadb.sys",
    "mysql",
    "sys",
    "debian-sys-maint",
    "pdns",
    "mail",
    "panel",
];

// Databases we hide and refuse to touch — system schemas plus the panel's own data.
const RESERVED_DBS: &[&str] = &[
    "information_schema",
    "performance_schema",
    "mysql",
    "sys",
    "pdns",
    "mail",
    "panel",
];

fn is_reserved_user(s: &str) -> bool {
    RESERVED_USERS.iter().any(|r| r.eq_ignore_ascii_case(s))
}

fn is_reserved_db(s: &str) -> bool {
    RESERVED_DBS.iter().any(|r| r.eq_ignore_ascii_case(s))
}

pub async fn list_users(State(state): State<AppState>) -> AgentResult<Json<Vec<UserRow>>> {
    let mut conn = state.mariadb.pool().get_conn().await?;
    let rows: Vec<(String, String)> = conn
        .query("SELECT User, Host FROM mysql.user")
        .await?;
    Ok(Json(
        rows.into_iter()
            .filter(|(user, _)| !is_reserved_user(user))
            .map(|(user, host)| UserRow { user, host })
            .collect(),
    ))
}

pub async fn list_databases(State(state): State<AppState>) -> AgentResult<Json<Vec<DatabaseRow>>> {
    let mut conn = state.mariadb.pool().get_conn().await?;
    let schemas: Vec<String> = conn
        .query("SELECT SCHEMA_NAME FROM information_schema.SCHEMATA")
        .await?;
    // Db-level grants. mysql.db stores the Db column as a LIKE pattern —
    // `GRANT ... ON \`my_app\`.*` lands as `my\_app` — so unescape before
    // matching against real schema names.
    let grants: Vec<(String, String, String)> = conn
        .query("SELECT Db, User, Host FROM mysql.db")
        .await?;
    let mut by_db: std::collections::HashMap<String, Vec<GrantedUser>> =
        std::collections::HashMap::new();
    for (db, user, host) in grants {
        if is_reserved_user(&user) {
            continue;
        }
        by_db
            .entry(unescape_grant_db(&db))
            .or_default()
            .push(GrantedUser { user, host });
    }
    Ok(Json(
        schemas
            .into_iter()
            .filter(|name| !is_reserved_db(name))
            .map(|name| {
                let mut users = by_db.remove(&name).unwrap_or_default();
                users.sort_by(|a, b| a.user.cmp(&b.user).then_with(|| a.host.cmp(&b.host)));
                // One chip per user@host, also when it holds the grant under
                // both spellings of the name (`my_app` and `my\_app`).
                users.dedup_by(|a, b| a.user == b.user && a.host == b.host);
                DatabaseRow { name, users }
            })
            .collect(),
    ))
}

/// Undo the LIKE-escaping in `mysql.db.Db` (`my\_app` → `my_app`). A raw
/// wildcard pattern like `%` survives unescaping but then never equals a
/// real schema name, so pattern grants simply don't attach — intended.
fn unescape_grant_db(db: &str) -> String {
    let mut out = String::with_capacity(db.len());
    let mut chars = db.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            out.push(chars.next().unwrap_or('\\'));
        } else {
            out.push(c);
        }
    }
    out
}

/// A database name as `GRANT ... ON <name>.*` has to spell it. MariaDB reads
/// `_` and `%` in that position as LIKE wildcards: a grant on `shop_1` also
/// opened `shopa1` and `shop11` to the user. Escaped, it covers that one
/// database, and lands in `mysql.db` as `shop\_1` (what phpMyAdmin writes).
fn grant_db_pattern(db: &str) -> String {
    let mut out = String::with_capacity(db.len() + 2);
    for c in db.chars() {
        if matches!(c, '_' | '%' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The names a grant on `db` can be stored under in `mysql.db`: escaped (what
/// a new grant uses), then plain. The column holds 64 characters and MariaDB
/// refuses a longer name, so a long name with underscores only has the plain
/// spelling.
fn grant_db_spellings(db: &str) -> Vec<String> {
    let escaped = grant_db_pattern(db);
    if escaped == db {
        vec![escaped]
    } else if escaped.chars().count() > 64 {
        vec![db.to_string()]
    } else {
        vec![escaped, db.to_string()]
    }
}

/// "There is no such grant": 1141 for a database, 1147 for a table, 1403 for
/// a routine.
fn is_no_such_grant(e: &mysql_async::Error) -> bool {
    matches!(e, mysql_async::Error::Server(s) if matches!(s.code, 1141 | 1147 | 1403))
}

/// A name read back from the grant tables, as an identifier. Unlike `escape`
/// it leaves a single quote alone, and inside backticks a backslash is
/// literal under every sql_mode.
fn quote_ident(s: &str) -> String {
    format!("`{}`", s.replace('`', "``"))
}

/// `user`@`host` for a GRANT / REVOKE. The anonymous user has no identifier
/// spelling.
fn quote_account(user: &str, host: &str) -> String {
    let user = if user.is_empty() { "''".to_string() } else { quote_ident(user) };
    format!("{user}@{}", quote_ident(host))
}

/// The statements that take everything `account` holds on `on` (`db`.*, a
/// table, `PROCEDURE db.name`). GRANT OPTION first: once REVOKE ALL
/// PRIVILEGES has removed the row, revoking it fails with "no such grant",
/// and left for last it is the row that stays behind.
fn revoke_statements(on: &str, account: &str) -> [String; 2] {
    [
        format!("REVOKE GRANT OPTION ON {on} FROM {account}"),
        format!("REVOKE ALL PRIVILEGES ON {on} FROM {account}"),
    ]
}

/// MariaDB keeps every grant on a database when the database is dropped: the
/// rows stay in mysql.db (and, for grants on one table, column or routine,
/// in tables_priv and procs_priv). A database created later under the same
/// name was open to the old users from its first second. Returns how many
/// grants were removed.
async fn revoke_grants_of_dropped_db(
    conn: &mut mysql_async::Conn,
    db: &str,
) -> Result<usize, mysql_async::Error> {
    // (what the grant is on, user, host)
    let mut grants: Vec<(String, String, String)> = Vec::new();
    // Both spellings a grant on this one database can have. A pattern that
    // covers several databases (`shop%`) is somebody's choice and stays.
    for spelling in grant_db_spellings(db) {
        let rows: Vec<(String, String)> = conn
            .exec("SELECT User, Host FROM mysql.db WHERE Db = ?", (spelling.as_str(),))
            .await?;
        let on = format!("{}.*", quote_ident(&spelling));
        grants.extend(rows.into_iter().map(|(user, host)| (on.clone(), user, host)));
    }
    // Column grants have their row in tables_priv too, and go with it.
    let tables: Vec<(String, String, String)> = conn
        .exec("SELECT User, Host, Table_name FROM mysql.tables_priv WHERE Db = ?", (db,))
        .await?;
    for (user, host, table) in tables {
        grants.push((format!("{}.{}", quote_ident(db), quote_ident(&table)), user, host));
    }
    let routines: Vec<(String, String, String, String)> = conn
        .exec(
            "SELECT User, Host, Routine_name, Routine_type FROM mysql.procs_priv WHERE Db = ?",
            (db,),
        )
        .await?;
    for (user, host, routine, kind) in routines {
        let kind = kind.to_ascii_uppercase();
        if !matches!(kind.as_str(), "FUNCTION" | "PROCEDURE" | "PACKAGE" | "PACKAGE BODY") {
            continue;
        }
        grants.push((
            format!("{kind} {}.{}", quote_ident(db), quote_ident(&routine)),
            user,
            host,
        ));
    }
    for (on, user, host) in &grants {
        for stmt in revoke_statements(on, &quote_account(user, host)) {
            if let Err(e) = conn.query_drop(stmt).await {
                if !is_no_such_grant(&e) {
                    return Err(e);
                }
            }
        }
    }
    if !grants.is_empty() {
        conn.query_drop("FLUSH PRIVILEGES").await?;
    }
    Ok(grants.len())
}

pub async fn create_user(
    State(state): State<AppState>,
    Json(req): Json<CreateUser>,
) -> AgentResult<Json<CreateUserResp>> {
    validate_ident(&req.user)?;
    validate_host(&req.host)?;
    if is_reserved_user(&req.user) {
        return Err(AgentError::Invalid(format!(
            "'{}' is a reserved system user — pick a different name",
            req.user
        )));
    }
    let password = password_or_random(req.password);
    let mut conn = state.mariadb.pool().get_conn().await?;
    // MariaDB does not accept '?' placeholders in CREATE USER ... IDENTIFIED BY,
    // so the password has to be inlined as an escaped string literal.
    let stmt = format!(
        "CREATE USER '{}'@'{}' IDENTIFIED BY '{}'",
        escape(&req.user),
        escape(&req.host),
        escape_string(&password)
    );
    conn.query_drop(stmt).await?;
    Ok(Json(CreateUserResp {
        user: req.user,
        host: req.host,
        initial_password: password,
    }))
}

pub async fn set_user_password(
    State(state): State<AppState>,
    Path(user): Path<String>,
    Json(req): Json<SetUserPassword>,
) -> AgentResult<Json<SetUserPasswordResp>> {
    validate_ident(&user)?;
    validate_host(&req.host)?;
    if is_reserved_user(&user) {
        return Err(AgentError::Invalid(format!(
            "'{user}' is a reserved system user — refusing to change its password"
        )));
    }
    let password = password_or_random(req.password);
    let mut conn = state.mariadb.pool().get_conn().await?;
    // Same constraint as CREATE USER: no '?' placeholders in IDENTIFIED BY,
    // so the password is inlined as an escaped string literal.
    let stmt = format!(
        "ALTER USER '{}'@'{}' IDENTIFIED BY '{}'",
        escape(&user),
        escape(&req.host),
        escape_string(&password)
    );
    conn.query_drop(stmt).await?;
    Ok(Json(SetUserPasswordResp {
        user,
        host: req.host,
        password,
    }))
}

pub async fn create_database(
    State(state): State<AppState>,
    Json(req): Json<CreateDatabase>,
) -> AgentResult<Json<DatabaseRow>> {
    validate_ident(&req.name)?;
    if is_reserved_db(&req.name) {
        return Err(AgentError::Invalid(format!(
            "'{}' is a reserved system database — pick a different name",
            req.name
        )));
    }
    let mut conn = state.mariadb.pool().get_conn().await?;
    conn.query_drop(format!(
        "CREATE DATABASE `{}` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci",
        escape(&req.name)
    ))
    .await?;
    Ok(Json(DatabaseRow {
        name: req.name,
        users: Vec::new(),
    }))
}

pub async fn grant(
    State(state): State<AppState>,
    Json(req): Json<Grant>,
) -> AgentResult<Json<crate::ipc::OkResponse>> {
    validate_ident(&req.user)?;
    validate_ident(&req.database)?;
    if is_reserved_user(&req.user) {
        return Err(AgentError::Invalid(format!(
            "'{}' is a reserved system user — refusing to grant",
            req.user
        )));
    }
    if is_reserved_db(&req.database) {
        return Err(AgentError::Invalid(format!(
            "'{}' is a reserved system database — refusing to grant",
            req.database
        )));
    }
    let host = req.host.unwrap_or_else(default_host);
    validate_host(&host)?;
    if !is_valid_priv(&req.privileges) {
        return Err(AgentError::Invalid("invalid privileges spec".into()));
    }
    let mut conn = state.mariadb.pool().get_conn().await?;
    let spellings = grant_db_spellings(&req.database);
    let mut on = &spellings[0];
    if let [escaped, plain] = spellings.as_slice() {
        // A grant made before the name was escaped sits in mysql.db under
        // the plain name. It is extended in place: MariaDB applies only the
        // most specific row, so a second, escaped row with fewer privileges
        // would take away what the old one gives.
        let held: Vec<String> = conn
            .exec(
                "SELECT Db FROM mysql.db WHERE Db IN (?, ?) AND User = ? AND Host = ?",
                (escaped.as_str(), plain.as_str(), req.user.as_str(), host.as_str()),
            )
            .await?;
        if !held.contains(escaped) && held.contains(plain) {
            on = plain;
        }
    }
    conn.query_drop(format!(
        "GRANT {} ON `{}`.* TO '{}'@'{}'",
        req.privileges,
        escape(on),
        escape(&req.user),
        escape(&host)
    ))
    .await?;
    conn.query_drop("FLUSH PRIVILEGES").await?;
    Ok(Json(crate::ipc::OkResponse::default()))
}

/// Drop a user's db-level grants — the inverse of `grant`, wired to the ×
/// on the Granted-users chips. Validation is the lenient kind rather than
/// `validate_ident`: the chip values come from `mysql.db`, where
/// hand-created users/databases may legally carry characters the panel's own
/// create forms refuse (dots, dashes, leading digits), and a chip the column
/// can show must also be revocable.
pub async fn revoke(
    State(state): State<AppState>,
    Json(req): Json<Revoke>,
) -> AgentResult<Json<crate::ipc::OkResponse>> {
    validate_lenient("user", &req.user)?;
    validate_lenient("database", &req.database)?;
    if is_reserved_user(&req.user) {
        return Err(AgentError::Invalid(format!(
            "'{}' is a reserved system user — refusing to revoke",
            req.user
        )));
    }
    if is_reserved_db(&req.database) {
        return Err(AgentError::Invalid(format!(
            "'{}' is a reserved system database — refusing to revoke",
            req.database
        )));
    }
    let host = req.host.unwrap_or_else(default_host);
    validate_host(&host)?;
    let mut conn = state.mariadb.pool().get_conn().await?;
    // The chip shows the plain name; the grant behind it is stored escaped
    // (`my\_app`: this panel, phpMyAdmin) or plain (`my_app`: older panel
    // grants, the mysql CLI). REVOKE only finds the spelling it is given, so
    // an escaped grant could be shown but never removed. Try both; 1141 (no
    // such grant) is the answer only when neither is there.
    let spellings = grant_db_spellings(&req.database);
    let mut revoked = false;
    let mut missing: Option<mysql_async::Error> = None;
    for db in &spellings {
        if let Err(e) = conn
            .query_drop(format!(
                "REVOKE ALL PRIVILEGES ON `{}`.* FROM '{}'@'{}'",
                escape(db),
                escape(&req.user),
                escape(&host)
            ))
            .await
        {
            if !is_no_such_grant(&e) {
                return Err(e.into());
            }
            missing.get_or_insert(e);
            continue;
        }
        revoked = true;
        // REVOKE ALL PRIVILEGES leaves the mysql.db row behind when the user
        // also held GRANT OPTION; clearing that needs its own REVOKE, which
        // errors with 1141 when the user never had it — swallow exactly that.
        if let Err(e) = conn
            .query_drop(format!(
                "REVOKE GRANT OPTION ON `{}`.* FROM '{}'@'{}'",
                escape(db),
                escape(&req.user),
                escape(&host)
            ))
            .await
        {
            if !is_no_such_grant(&e) {
                return Err(e.into());
            }
        }
    }
    if let (false, Some(e)) = (revoked, missing) {
        return Err(e.into());
    }
    conn.query_drop("FLUSH PRIVILEGES").await?;
    Ok(Json(crate::ipc::OkResponse::default()))
}

#[derive(Deserialize)]
pub struct DeleteUserQuery {
    /// Drop only user@host. Omitted: every host of the user (the old
    /// behaviour, kept for API callers that mean it). The Databases page
    /// lists one row per user@host and always sends the row's host: dropping
    /// `app@%` used to remove `app@localhost` too.
    #[serde(default)]
    pub host: Option<String>,
}

fn is_safe_db_host(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 255
        && h.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '%' | '_' | ':' | '-'))
}

pub async fn delete_user(
    State(state): State<AppState>,
    Path(user): Path<String>,
    axum::extract::Query(q): axum::extract::Query<DeleteUserQuery>,
) -> AgentResult<Json<crate::ipc::OkResponse>> {
    validate_ident(&user)?;
    if let Some(host) = &q.host {
        if !is_safe_db_host(host) {
            return Err(AgentError::Invalid("invalid host".into()));
        }
    }
    if is_reserved_user(&user) {
        return Err(AgentError::Invalid(format!(
            "'{}' is a reserved system user — refusing to drop",
            user
        )));
    }
    let mut conn = state.mariadb.pool().get_conn().await?;
    let hosts: Vec<String> = match &q.host {
        Some(host) => vec![host.clone()],
        None => conn
            .exec("SELECT Host FROM mysql.user WHERE User = ?", (&user,))
            .await?,
    };
    for host in hosts {
        conn.query_drop(format!(
            "DROP USER IF EXISTS '{}'@'{}'",
            escape(&user),
            escape(&host)
        ))
        .await?;
    }
    conn.query_drop("FLUSH PRIVILEGES").await?;
    Ok(Json(crate::ipc::OkResponse::default()))
}

pub async fn delete_database(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> AgentResult<Json<crate::ipc::OkResponse>> {
    validate_ident(&name)?;
    if is_reserved_db(&name) {
        return Err(AgentError::Invalid(format!(
            "'{}' is a reserved system database — refusing to drop",
            name
        )));
    }
    let mut conn = state.mariadb.pool().get_conn().await?;
    conn.query_drop(format!("DROP DATABASE IF EXISTS `{}`", escape(&name)))
        .await?;
    // After the drop, never before it: a drop that fails must not leave the
    // database there with its users locked out.
    match revoke_grants_of_dropped_db(&mut conn, &name).await {
        Ok(0) => {}
        Ok(n) => tracing::info!(database = %name, grants = n, "revoked the grants on a dropped database"),
        Err(e) => {
            return Err(AgentError::Other(anyhow::anyhow!(
                "database `{name}` was dropped, but not all grants on it could be removed (a new database of this name would be open to the users that had them): {e}"
            )))
        }
    }
    Ok(Json(crate::ipc::OkResponse::default()))
}

// ---------------------------------------------------------------------------
// SQL dump import
// ---------------------------------------------------------------------------

/// How much of the dump's head is scanned for CREATE DATABASE / USE. Both
/// statements sit within the first couple of KB of every mariadb-dump /
/// mysqldump file that carries them; 256 KB leaves room for oversized
/// comment banners. A `USE` buried gigabytes into an --all-databases dump is
/// out of scope by design (documented limitation).
const IMPORT_HEADER_SCAN_BYTES: usize = 256 * 1024;

#[derive(Deserialize)]
pub struct ImportSql {
    /// /files root id the staged dump lives in (normally `drive`).
    pub root: String,
    /// Root-relative path of the `.sql` file (e.g. `sql-dumps/backup.sql`).
    pub path: String,
    /// Target database for dumps WITHOUT their own CREATE DATABASE / USE
    /// header. Ignored when the dump names its own database.
    pub target_db: Option<String>,
    /// Create `target_db` (utf8mb4) if missing. Headerless dumps only.
    #[serde(default)]
    pub create_target: bool,
    /// DROP DATABASE IF EXISTS before importing — clean re-import.
    #[serde(default)]
    pub drop_first: bool,
}

#[derive(Serialize)]
pub struct ImportSqlAccepted {
    pub task_id: String,
    /// Database the import targets (detected from the dump header, or the
    /// caller's choice for headerless dumps).
    pub database: String,
    /// True when the dump carries its own CREATE DATABASE / USE statements.
    pub dump_defines_db: bool,
}

/// Import a staged `.sql` dump into MariaDB by piping it through the
/// `mariadb` CLI as a background task.
///
/// Why the CLI and not the `mysql_async` pool: dumps taken with
/// `--routines` / `--triggers` carry client-side `DELIMITER` blocks, and
/// dumps from MariaDB 10.11.8+ open with the sandbox-mode line
/// (`/*M!999999\- ... */`) — both are mariadb-client features no
/// server-protocol driver can execute. Piping through the client is also
/// exactly the documented manual flow (`mariadb < dump.sql`), which this
/// handler automates. Running as
/// root over the unix socket is what lets DEFINER clauses import unchanged.
///
/// The import runs detached from the HTTP request: a multi-hundred-MB dump
/// takes minutes, and a connection-bound import would be killed half-done by
/// any proxy timeout. Progress (bytes fed of total) streams into the task
/// log.
///
/// SECURITY: the dump executes arbitrary SQL as MariaDB root. That is the
/// same trust level as /terminal — admin-only and audited on the panel side,
/// not filtered here. The reserved-DB check below catches the honest mistake
/// (a dump whose header targets `mysql`/`pdns`/...), it is not a boundary: a
/// hostile dump could switch databases mid-file where no cheap scan would
/// see it. What a dump cannot do is make the client, which is root, run
/// programs or read files of this machine: its own commands are switched off
/// (see `client_guard`) and it sends no local file.
pub async fn import_sql(
    State(state): State<AppState>,
    Json(req): Json<ImportSql>,
) -> AgentResult<Json<ImportSqlAccepted>> {
    // Resolve through the /files jail with download's read-primitive rules:
    // leaf canonicalised, protected list applied to the resolved target.
    let (dump_path, meta) = super::files::resolve_read_file(&req.root, &req.path).await?;
    let size = meta.len();
    if size == 0 {
        return Err(AgentError::Invalid("the dump file is empty".into()));
    }
    if !dump_path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("sql"))
    {
        return Err(AgentError::Invalid(
            "only .sql files can be imported (gzip the download, not the import)".into(),
        ));
    }

    let header = read_dump_header(&dump_path, size).await?;
    let detected = detect_dump_database(&header);

    let (db, dump_defines_db) = match detected {
        Some(name) => (name, true),
        None => {
            let t = req.target_db.as_deref().unwrap_or("").trim().to_string();
            if t.is_empty() {
                return Err(AgentError::Invalid(
                    "this dump has no CREATE DATABASE / USE header — choose a target database"
                        .into(),
                ));
            }
            (t, false)
        }
    };
    if dump_defines_db {
        validate_detected_db(&db)?;
    } else {
        validate_ident(&db)?;
    }
    if is_reserved_db(&db) {
        return Err(AgentError::Invalid(format!(
            "this import targets `{db}`, a reserved system database — refusing"
        )));
    }

    // A headerless dump that is neither creating nor dropping its target
    // must land in a database that actually exists.
    if !dump_defines_db && !req.create_target && !req.drop_first {
        let mut conn = state.mariadb.pool().get_conn().await?;
        let exists: Option<String> = conn
            .exec_first(
                "SELECT SCHEMA_NAME FROM information_schema.SCHEMATA WHERE SCHEMA_NAME = ?",
                (db.as_str(),),
            )
            .await?;
        if exists.is_none() {
            return Err(AgentError::Invalid(format!(
                "database `{db}` does not exist — pick an existing database or choose to create it"
            )));
        }
    }

    // A dump expands into InnoDB files at roughly its own size or more
    // (indexes), and everything on this box shares one partition — filling
    // it takes down mail/Postgres/nginx/the panel, not just this import.
    if let Some(free) = super::files::available_bytes(
        dump_path.parent().unwrap_or_else(|| std::path::Path::new("/")),
    ) {
        let needed = (size + size / 2).max(1024 * 1024 * 1024);
        if free < needed {
            return Err(AgentError::Conflict(format!(
                "not enough free disk space: importing this {} MB dump needs ~{} MB headroom but only {} MB is free",
                size / (1024 * 1024),
                needed / (1024 * 1024),
                free / (1024 * 1024)
            )));
        }
    }

    // Double-click / double-tab protection, not a hard lock (a race between
    // this check and `create` below is possible but harmless beyond two
    // imports contending — the second one's SQL errors surface in its log).
    if state.tasks.any_live_with_prefix("db:import:") {
        return Err(AgentError::Conflict(
            "another SQL import is already running — wait for it to finish first".into(),
        ));
    }

    let task = state.tasks.create(&format!("db:import:{db}"));
    let task_id = task.id.clone();
    tokio::spawn(run_import_task(
        task,
        state.mariadb.clone(),
        dump_path,
        size,
        mariadb_socket_path(&state.config.mariadb_url),
        db.clone(),
        dump_defines_db,
        dump_defines_db && dump_creates_database(&header),
        req.drop_first,
        req.create_target,
    ));

    Ok(Json(ImportSqlAccepted {
        task_id,
        database: db,
        dump_defines_db,
    }))
}

async fn read_dump_header(path: &std::path::Path, size: u64) -> AgentResult<String> {
    let mut f = tokio::fs::File::open(path).await.map_err(AgentError::Io)?;
    let mut buf = vec![0u8; IMPORT_HEADER_SCAN_BYTES.min(size as usize)];
    let mut read = 0usize;
    while read < buf.len() {
        let n = f.read(&mut buf[read..]).await.map_err(AgentError::Io)?;
        if n == 0 {
            break;
        }
        read += n;
    }
    buf.truncate(read);
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Pull the database a dump targets out of its header. mariadb-dump (and
/// mysqldump) write one of these near the top when the dump carries its own
/// database:
///   CREATE DATABASE /*!32312 IF NOT EXISTS*/ `Name` /*!40100 ... */;
///   USE `Name`;
fn detect_dump_database(header: &str) -> Option<String> {
    let patterns = [
        // CREATE DATABASE, backtick-quoted name (with optional versioned or
        // plain IF NOT EXISTS between keyword and name).
        r#"(?im)^\s*CREATE\s+DATABASE\s+(?:/\*!\d+\s+IF\s+NOT\s+EXISTS\s*\*/\s*|IF\s+NOT\s+EXISTS\s+)?`([^`\r\n]+)`"#,
        // CREATE DATABASE, bare name.
        r#"(?im)^\s*CREATE\s+DATABASE\s+(?:/\*!\d+\s+IF\s+NOT\s+EXISTS\s*\*/\s*|IF\s+NOT\s+EXISTS\s+)?([A-Za-z0-9_$]+)"#,
        // USE, quoted then bare.
        r#"(?im)^\s*USE\s+`([^`\r\n]+)`\s*;"#,
        r#"(?im)^\s*USE\s+([A-Za-z0-9_$]+)\s*;"#,
    ];
    for p in patterns {
        if let Some(c) = regex::Regex::new(p).unwrap().captures(header) {
            return Some(c[1].to_string());
        }
    }
    None
}

/// True when the header has a CREATE DATABASE of its own. A dump that only
/// says `USE` (Adminer, `mysqldump --no-create-db`) needs its database to
/// exist before the import starts.
fn dump_creates_database(header: &str) -> bool {
    regex::Regex::new(r"(?im)^\s*CREATE\s+DATABASE\b")
        .unwrap()
        .is_match(header)
}

/// The dump named its own database, so the name never came from a form
/// field — see `validate_lenient` for why the check is looser than
/// `validate_ident`.
fn validate_detected_db(s: &str) -> AgentResult<()> {
    validate_lenient("the dump's database name", s)
}

/// Loose validation for names that already exist inside MariaDB (a dump's
/// own database, a grant row from mysql.db) rather than arriving from a
/// panel form. `validate_ident` would refuse legal names like `2024_data`
/// or `app.prod`, but a name the engine holds must remain operable on.
/// Backticks/quotes/backslashes are still refused: they'd survive into DDL
/// mangled by `escape` (which doubles quotes meant for string contexts), so
/// a statement against such a name would silently target the wrong thing.
fn validate_lenient(what: &str, s: &str) -> AgentResult<()> {
    if s.is_empty() || s.len() > 64 {
        return Err(AgentError::Invalid(format!("{what} has an invalid length")));
    }
    if s.chars().any(|c| c.is_control()) || s.contains(['`', '\'', '"', '\\']) {
        return Err(AgentError::Invalid(format!(
            "{what} contains invalid characters"
        )));
    }
    Ok(())
}

/// The CLI needs the raw socket path, so pull `socket=` out of MARIADB_URL
/// (`mysql://root@localhost:3306/mysql?socket=/var/run/mysqld/mysqld.sock`).
fn mariadb_socket_path(url: &str) -> String {
    url.split_once("socket=")
        .map(|(_, rest)| rest.split('&').next().unwrap_or("").trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/var/run/mysqld/mysqld.sock".to_string())
}

/// The client with what every run of it gets. `\e` / `edit` starts $EDITOR
/// (vi when unset) on the statement so far, with the rest of the dump still
/// on its stdin: the dump then types into vi, `:!command` included, and the
/// sandbox mode allows `\e`. `true` as the editor makes it do nothing. The
/// agent's own secrets stay out of the client's environment, as in exec.rs.
fn client_command(bin: &str) -> Command {
    let mut cmd = Command::new(bin);
    cmd.env("EDITOR", "true").env("VISUAL", "true");
    for name in crate::exec::AGENT_SECRET_ENV {
        cmd.env_remove(name);
    }
    cmd
}

/// Ubuntu's mariadb-client ships the binary as `mariadb` with a `mysql`
/// compat symlink; other setups only have `mysql`. Returns the one that is
/// there and whether it has a sandbox mode (`--sandbox`, MariaDB 10.11.8+):
/// asked of the binary itself, an older one answers "unknown option". Only a
/// yes is remembered, because the client can be updated under a running
/// agent.
async fn find_mariadb_client() -> std::io::Result<(&'static str, bool)> {
    static SANDBOX_CLIENT: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    if let Some(bin) = SANDBOX_CLIENT.get() {
        return Ok((bin, true));
    }
    for bin in ["mariadb", "mysql"] {
        match client_command(bin)
            .args(["--sandbox", "--version"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
        {
            Ok(status) => {
                if status.success() {
                    let _ = SANDBOX_CLIENT.set(bin);
                }
                return Ok((bin, status.success()));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "neither `mariadb` nor `mysql` client binary found — install mariadb-client",
    ))
}

/// The option that keeps a dump from running the client's own commands,
/// which act on this machine as root: `\!` / `system` (a shell command) and
/// `\.` / `source` (another file as SQL). They work anywhere in the input,
/// also in the middle of a line and inside `/*! */`, so no filter over the
/// dump's text can be trusted to find them (where a string ends depends on
/// the sql_mode the dump itself sets). The client refuses them itself:
///
/// - `--sandbox` where the client has it: those commands stop the import
///   with "Not allowed in the sandbox mode", everything else is as before.
///   Dumps from mariadb-dump 10.11.8+ ask for the same in their first line.
/// - `--binary-mode` on an older client (and Oracle's `mysql`), which rejects
///   `--sandbox`: every client command except DELIMITER and `\C` is off and
///   stops the import as an unknown command or a syntax error. It also stops
///   translating `\r\n` and accepts NUL bytes, which only makes the import
///   closer to the file.
///
/// A dump without such commands imports as it did before, with either.
fn client_guard(sandbox: bool) -> &'static str {
    if sandbox {
        "--sandbox"
    } else {
        "--binary-mode"
    }
}

/// The first line of every dump written by mariadb-dump 10.11.8+. A client
/// that knows it enters the sandbox mode; an older one stops right there with
/// "Unknown command '\-'".
const SANDBOX_COMMENT: &[u8] = br"/*M!999999\- enable the sandbox mode */";

/// The start of a dump as a client without a sandbox mode has to get it:
/// without that comment. Only the comment goes, not its line, so the line
/// numbers in the client's errors stay those of the file.
fn without_sandbox_comment(head: &[u8]) -> &[u8] {
    head.strip_prefix(SANDBOX_COMMENT).unwrap_or(head)
}

/// Reads until `buf` is full or the file ends.
async fn read_up_to(file: &mut tokio::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut read = 0usize;
    while read < buf.len() {
        let n = file.read(&mut buf[read..]).await?;
        if n == 0 {
            break;
        }
        read += n;
    }
    Ok(read)
}

fn spawn_mariadb_client(bin: &str, args: &[String]) -> std::io::Result<tokio::process::Child> {
    client_command(bin)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

/// Events flowing from the pump / output-drain tasks back to the task log.
/// The channel indirection exists because `TaskHandle` is consumed by
/// `finish()` and so can't be shared across spawned readers.
enum ImportEv {
    /// A line of client stdout/stderr (drains cap these at 200 per stream so
    /// a chatty client can't balloon the task log).
    Line(String),
    /// Bytes fed to the client's stdin so far.
    Progress(u64),
    /// The stdin pump finished: total bytes, or why it stopped early.
    PumpDone(Result<u64, String>),
}

fn spawn_line_drain(
    stream: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    tx: tokio::sync::mpsc::UnboundedSender<ImportEv>,
) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stream).lines();
        let mut sent = 0usize;
        while let Ok(Some(l)) = lines.next_line().await {
            sent += 1;
            if sent <= 200 {
                let _ = tx.send(ImportEv::Line(l));
            } else if sent == 201 {
                let _ = tx.send(ImportEv::Line("… further client output truncated".into()));
            }
            // Keep draining past the cap — a full pipe buffer would block
            // the child.
        }
    });
}

/// Map the most common import failures to a one-line fix. The raw client
/// error (with its line number) is already in the log; these just save the
/// operator a search.
fn import_error_hints(output: &[String], sandbox: bool) -> Vec<String> {
    let joined = output.join("\n");
    let mut hints = Vec::new();
    if joined.contains("max_allowed_packet") || joined.contains("server has gone away") {
        hints.push(
            "Hint: a statement in this dump exceeds the server's max_allowed_packet. Raise it (e.g. `max_allowed_packet = 256M` under [mysqld] in /etc/mysql/mariadb.conf.d/) and re-run the import."
                .to_string(),
        );
    }
    if joined.contains("Not allowed in the sandbox mode") {
        hints.push(
            "Hint: the dump has a client command that runs a program or reads or writes a file on this server (`system` or `\\!`, `source` or `\\.`, `tee`). An import does not run those. Remove that line (the error above has its number) and import again."
                .to_string(),
        );
    }
    if joined.contains("disabled the local infile capability") {
        hints.push(
            "Hint: the dump has a LOAD DATA LOCAL INFILE statement, which makes the client send a file from this server to the database. An import does not do that; a dump carries its rows as INSERT statements."
                .to_string(),
        );
    }
    // The comment at the very start is taken out for a client that does not
    // know it, so this is one further down.
    if joined.contains("Unknown command '\\-'") {
        hints.push(
            "Hint: the dump has the MariaDB 10.11.8+ sandbox-mode line after its first line (two dumps joined into one file?) and this server's client is too old to understand it. Update mariadb-client and retry."
                .to_string(),
        );
    } else if !sandbox && names_client_command(&joined) {
        hints.push(
            "Hint: this server's MariaDB client is older than 10.11.8, so the import runs with the client's own commands switched off, and the dump uses one. Update mariadb-client and retry, or remove that line (the error above has its number)."
                .to_string(),
        );
    }
    hints
}

/// True when the client reported one of its own one-letter commands as
/// unknown (`Unknown command '\g'.`), which is what `--binary-mode` makes of
/// them. A stray backslash (`'\\'`, `'\0'`) is reported the same way and is
/// not one.
fn names_client_command(output: &str) -> bool {
    const MARK: &str = "Unknown command '\\";
    output.match_indices(MARK).any(|(at, _)| {
        output[at + MARK.len()..]
            .chars()
            .next()
            .is_some_and(|c| "?!.#cdeGghnPpqRrsTtuWw".contains(c))
    })
}

#[allow(clippy::too_many_arguments)]
async fn run_import_task(
    task: crate::ipc::tasks::TaskHandle,
    mariadb: crate::db::MariaDb,
    dump: PathBuf,
    size: u64,
    socket: String,
    db: String,
    dump_defines_db: bool,
    dump_creates_db: bool,
    drop_first: bool,
    create_target: bool,
) {
    task.start();
    task.log(format!(
        "Importing {} ({} MB) into `{}`",
        dump.display(),
        size / (1024 * 1024),
        db
    ));
    task.log(if dump_defines_db {
        "The dump carries its own CREATE DATABASE / USE statements.".to_string()
    } else {
        format!("Headerless dump — statements run against `{db}`.")
    });

    // Before the DROP below: without a client there is nothing to import.
    let (client, sandbox) = match find_mariadb_client().await {
        Ok(found) => found,
        Err(e) => {
            task.log(format!("could not start the mariadb client: {e}"));
            task.finish(false);
            return;
        }
    };
    if !sandbox {
        task.log(
            "This server's MariaDB client is older than 10.11.8 and has no sandbox mode: importing with the client's own commands switched off (--binary-mode).",
        );
    }

    // Pre-steps through the pool (same connection identity as the CLI).
    if drop_first || (!dump_defines_db && create_target) {
        let mut conn = match mariadb.pool().get_conn().await {
            Ok(c) => c,
            Err(e) => {
                task.log(format!("mariadb connection failed: {e}"));
                task.finish(false);
                return;
            }
        };
        if drop_first {
            if let Err(e) = conn
                .query_drop(format!("DROP DATABASE IF EXISTS `{}`", escape(&db)))
                .await
            {
                task.log(format!("DROP DATABASE failed: {e}"));
                task.finish(false);
                return;
            }
            task.log(format!("Dropped database `{db}` (clean re-import requested)."));
        }
        // A headerless dump won't recreate what we just dropped (and can't
        // create a brand-new target either) — ensure it exists. The same for
        // a dump that only says `USE`: after the DROP its first statement
        // failed with "Unknown database" and the old data was gone.
        if !dump_creates_db {
            if let Err(e) = conn
                .query_drop(format!(
                    "CREATE DATABASE IF NOT EXISTS `{}` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci",
                    escape(&db)
                ))
                .await
            {
                task.log(format!("CREATE DATABASE failed: {e}"));
                task.finish(false);
                return;
            }
            task.log(format!("Ensured database `{db}` exists (utf8mb4)."));
        }
    }

    let mut args: Vec<String> = vec![
        format!("--socket={socket}"),
        "-u".into(),
        "root".into(),
        "--default-character-set=utf8mb4".into(),
        "--max-allowed-packet=1G".into(),
        client_guard(sandbox).into(),
        // LOAD DATA LOCAL INFILE has the client send any file root can read
        // (/etc/shadow, the panel's .env) into a table the dump names; both
        // sides allow it by default, the sandbox mode too.
        "--local-infile=0".into(),
    ];
    if !dump_defines_db {
        // Only headerless dumps get a database argument. A dump with its own
        // CREATE DATABASE may target a database that doesn't exist yet, and
        // the client refuses to start in a missing one.
        args.push(db.clone());
    }

    let mut child = match spawn_mariadb_client(client, &args) {
        Ok(c) => c,
        Err(e) => {
            task.log(format!("could not start the mariadb client: {e}"));
            task.finish(false);
            return;
        }
    };

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ImportEv>();
    spawn_line_drain(child.stdout.take().expect("stdout piped"), tx.clone());
    spawn_line_drain(child.stderr.take().expect("stderr piped"), tx.clone());

    // Feed the file ourselves instead of handing the client an fd — counting
    // bytes here is what turns a silent multi-minute import into a progress
    // log.
    let mut stdin = child.stdin.take().expect("stdin piped");
    let pump_dump = dump.clone();
    tokio::spawn(async move {
        let res = async {
            let mut file = tokio::fs::File::open(&pump_dump)
                .await
                .map_err(|e| e.to_string())?;
            let mut buf = vec![0u8; 256 * 1024];
            let mut written: u64 = 0;
            let mut last_progress: u64 = 0;
            if !sandbox {
                // The sandbox comment, when the dump starts with one, is not
                // passed on: this client would stop at it.
                let n = read_up_to(&mut file, &mut buf[..SANDBOX_COMMENT.len()])
                    .await
                    .map_err(|e| e.to_string())?;
                if let Err(e) = stdin.write_all(without_sandbox_comment(&buf[..n])).await {
                    return Err(format!("client stopped reading input: {e}"));
                }
                written += n as u64;
            }
            loop {
                let n = file.read(&mut buf).await.map_err(|e| e.to_string())?;
                if n == 0 {
                    break;
                }
                if let Err(e) = stdin.write_all(&buf[..n]).await {
                    // Broken pipe = the client exited early (SQL error). The
                    // exit code + stderr carry the real story; stop feeding.
                    return Err(format!("client stopped reading input: {e}"));
                }
                written += n as u64;
                if written - last_progress >= 32 * 1024 * 1024 {
                    last_progress = written;
                    let _ = tx.send(ImportEv::Progress(written));
                }
            }
            stdin.shutdown().await.ok();
            Ok(written)
        }
        .await;
        let _ = tx.send(ImportEv::PumpDone(res));
        // `stdin` drops here → EOF for the child.
    });

    // All three senders are owned by spawned tasks; rx closes when the pump
    // and both drains are done.
    let mut client_output: Vec<String> = Vec::new();
    let mut pump_result: Option<Result<u64, String>> = None;
    while let Some(ev) = rx.recv().await {
        match ev {
            ImportEv::Line(l) => {
                client_output.push(l.clone());
                task.log(l);
            }
            ImportEv::Progress(done) => {
                let pct = done.saturating_mul(100) / size.max(1);
                task.log(format!(
                    "… fed {} MB of {} MB ({pct}%)",
                    done / (1024 * 1024),
                    size / (1024 * 1024)
                ));
            }
            ImportEv::PumpDone(r) => pump_result = Some(r),
        }
    }

    let status = child.wait().await;
    match status {
        Ok(s) if s.success() => {
            if let Some(Err(e)) = &pump_result {
                // Exit 0 with a broken pump would mean the client quit while
                // we still had bytes — worth surfacing even on "success".
                task.log(format!("warning: {e}"));
            }
            match mariadb.pool().get_conn().await {
                Ok(mut conn) => {
                    let counts: Result<Option<(u64, u64)>, _> = conn
                        .exec_first(
                            "SELECT COUNT(*), CAST(COALESCE(SUM(TABLE_ROWS), 0) AS UNSIGNED)
                             FROM information_schema.TABLES WHERE TABLE_SCHEMA = ?",
                            (db.as_str(),),
                        )
                        .await;
                    match counts {
                        Ok(Some((tables, rows))) => task.log(format!(
                            "Import complete: {tables} tables in `{db}` (~{rows} rows by engine estimate)."
                        )),
                        Ok(None) => task.log("Import complete.".to_string()),
                        Err(e) => task.log(format!(
                            "Import finished but the verification query failed: {e}"
                        )),
                    }
                }
                Err(e) => task.log(format!(
                    "Import finished but the verification connection failed: {e}"
                )),
            }
            task.finish(true);
        }
        Ok(s) => {
            task.log(format!(
                "mariadb client exited with code {}",
                s.code().unwrap_or(-1)
            ));
            if let Some(Err(e)) = &pump_result {
                task.log(format!("(input feed stopped early: {e})"));
            }
            for hint in import_error_hints(&client_output, sandbox) {
                task.log(hint);
            }
            task.finish(false);
        }
        Err(e) => {
            task.log(format!("failed to wait on the mariadb client: {e}"));
            task.finish(false);
        }
    }
}

fn validate_ident(s: &str) -> AgentResult<()> {
    let re = regex::Regex::new(r"^[A-Za-z_][A-Za-z0-9_]{0,63}$").unwrap();
    if !re.is_match(s) {
        return Err(AgentError::Invalid(format!("invalid identifier: {s}")));
    }
    Ok(())
}

fn validate_host(s: &str) -> AgentResult<()> {
    let re = regex::Regex::new(r"^[A-Za-z0-9_.\-%:]{1,255}$").unwrap();
    if !re.is_match(s) {
        return Err(AgentError::Invalid(format!("invalid host: {s}")));
    }
    Ok(())
}

fn is_valid_priv(s: &str) -> bool {
    let allowed = [
        "ALL PRIVILEGES",
        "SELECT",
        "INSERT",
        "UPDATE",
        "DELETE",
        "CREATE",
        "DROP",
        "ALTER",
        "INDEX",
        "REFERENCES",
        "USAGE",
        "SELECT, INSERT, UPDATE, DELETE",
    ];
    allowed.contains(&s)
}

fn escape(s: &str) -> String {
    s.replace('`', "``").replace('\'', "''")
}

fn escape_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// The caller's password, or a generated one. An empty string counts as none:
/// `IDENTIFIED BY ''` made an account that logs in without a password (the
/// MCP `db.create_user` tool passes "" through).
fn password_or_random(given: Option<String>) -> String {
    given.filter(|p| !p.is_empty()).unwrap_or_else(random_password)
}

fn random_password() -> String {
    use base64::Engine;
    let mut bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_names_are_escaped_and_read_back() {
        assert_eq!(grant_db_pattern("shop_1"), r"shop\_1");
        assert_eq!(grant_db_pattern("a%b"), r"a\%b");
        assert_eq!(grant_db_pattern("plain"), "plain");
        // What the Granted-users column reads back from mysql.db.
        for name in ["shop_1", "a_b_c", "plain", "app-prod", "100%"] {
            assert_eq!(unescape_grant_db(&grant_db_pattern(name)), name);
        }
        // A grant stored unescaped still shows under its database.
        assert_eq!(unescape_grant_db("shop_1"), "shop_1");

        assert_eq!(grant_db_spellings("plain"), ["plain"]);
        assert_eq!(grant_db_spellings("shop_1"), [r"shop\_1", "shop_1"]);
        // 60 characters, 5 underscores: escaped it would not fit mysql.db.
        let long = format!("a_b_c_d_e_{}", "x".repeat(50));
        assert_eq!(grant_db_spellings(&long), [long.clone()]);
        let fits = format!("a_b_{}", "x".repeat(58));
        assert_eq!(grant_db_spellings(&fits).len(), 2);
    }

    #[test]
    fn a_use_only_dump_does_not_create_its_database() {
        let dumped = "-- MariaDB dump\n\nCREATE DATABASE /*!32312 IF NOT EXISTS*/ `shop` /*!40100 DEFAULT CHARACTER SET utf8mb4 */;\n\nUSE `shop`;\n";
        assert_eq!(detect_dump_database(dumped).as_deref(), Some("shop"));
        assert!(dump_creates_database(dumped));
        let use_only = "-- Adminer 4.8.1 MySQL dump\n\nSET NAMES utf8;\n\nUSE `shop`;\n\nDROP TABLE IF EXISTS `orders`;\n";
        assert_eq!(detect_dump_database(use_only).as_deref(), Some("shop"));
        assert!(!dump_creates_database(use_only));
        // A comment that mentions it is not a statement.
        assert!(!dump_creates_database("-- run CREATE DATABASE first\nUSE shop;\n"));
    }

    #[test]
    fn an_empty_password_is_replaced_by_a_generated_one() {
        assert_eq!(password_or_random(Some("s3cret".into())), "s3cret");
        assert!(password_or_random(Some(String::new())).len() >= 24);
        assert!(password_or_random(None).len() >= 24);
    }

    #[test]
    fn grants_of_a_dropped_database_are_revoked_under_any_name() {
        assert_eq!(
            revoke_statements(&format!("{}.*", quote_ident(r"shop\_1")), &quote_account("app", "localhost")),
            [
                r"REVOKE GRANT OPTION ON `shop\_1`.* FROM `app`@`localhost`",
                r"REVOKE ALL PRIVILEGES ON `shop\_1`.* FROM `app`@`localhost`",
            ]
        );
        // Names as they come out of the grant tables: quotes and backslashes
        // stay as they are, only a backtick is doubled.
        assert_eq!(quote_ident("we`ird 't"), "`we``ird 't`");
        assert_eq!(quote_account(r"it's\me", "%.example"), r"`it's\me`@`%.example`");
        assert_eq!(quote_account("", "localhost"), "''@`localhost`");
    }

    #[test]
    fn the_client_is_told_not_to_run_its_own_commands() {
        assert_eq!(client_guard(true), "--sandbox");
        assert_eq!(client_guard(false), "--binary-mode");
        // The editor a dump could start with `\e`, and the agent's secrets.
        let cmd = client_command("mariadb");
        let env: Vec<(String, Option<String>)> = cmd
            .as_std()
            .get_envs()
            .map(|(k, v)| {
                (k.to_string_lossy().into_owned(), v.map(|v| v.to_string_lossy().into_owned()))
            })
            .collect();
        assert!(env.contains(&("EDITOR".to_string(), Some("true".to_string()))));
        assert!(env.contains(&("VISUAL".to_string(), Some("true".to_string()))));
        assert!(env.contains(&("AGENT_TOKEN".to_string(), None)));
    }

    #[test]
    fn the_sandbox_comment_is_taken_out_for_a_client_that_does_not_know_it() {
        let dump = b"/*M!999999\\- enable the sandbox mode */ \n-- MariaDB dump 10.19\nUSE `shop`;\n";
        // The rest of line 1 stays, so line numbers do not move.
        assert_eq!(
            without_sandbox_comment(dump),
            b" \n-- MariaDB dump 10.19\nUSE `shop`;\n"
        );
        let plain = b"-- MySQL dump 10.13\nUSE `shop`;\n";
        assert_eq!(without_sandbox_comment(plain), plain);
        // Only at the very start.
        let later = b"\n/*M!999999\\- enable the sandbox mode */ \n";
        assert_eq!(without_sandbox_comment(later), later);
        assert_eq!(without_sandbox_comment(b""), b"");
    }

    #[test]
    fn a_refused_client_command_gets_a_hint() {
        let hints = |line: &str, sandbox: bool| import_error_hints(&[line.to_string()], sandbox);
        let refused = hints("ERROR at line 7: Not allowed in the sandbox mode", true);
        assert_eq!(refused.len(), 1);
        assert!(refused[0].contains("does not run those"));
        // --binary-mode reports the command as unknown.
        let old = hints(r"ERROR at line 7: Unknown command '\!'.", false);
        assert_eq!(old.len(), 1);
        assert!(old[0].contains("older than 10.11.8"));
        // A stray backslash is not a client command, with either client.
        assert!(hints(r"ERROR at line 7: Unknown command '\\'.", false).is_empty());
        assert!(hints(r"ERROR at line 7: Unknown command '\!'.", true).is_empty());
        let joined = hints(r"ERROR at line 90: Unknown command '\-'.", false);
        assert_eq!(joined.len(), 1);
        assert!(joined[0].contains("after its first line"));
        assert!(hints("ERROR 1064 (42000) at line 3: You have an error in your SQL syntax", true).is_empty());
        let local = hints(
            "ERROR 4166 (HY000) at line 2: The used command is not allowed because the MariaDB server or client has disabled the local infile capability",
            true,
        );
        assert_eq!(local.len(), 1);
        assert!(local[0].contains("LOAD DATA LOCAL INFILE"));
    }

    /// Dumps with `\!`, `system`, `source` and `\e` through the real client:
    /// none may run, and a dump from mariadb-dump still imports.
    #[tokio::test]
    #[ignore = "needs the mariadb client and a server it may create and drop `import_guard_test` on (IMPORT_TEST_SOCKET=/run/mysqld/mysqld.sock cargo test -- --ignored)"]
    async fn client_commands_in_a_dump_do_not_run() {
        use crate::ipc::tasks::{TaskStatus, TaskTracker};
        let socket = std::env::var("IMPORT_TEST_SOCKET").expect("IMPORT_TEST_SOCKET");
        let dir = tempfile::tempdir().unwrap();
        let panel_db = crate::db::PanelDb::open(&dir.path().join("panel.db")).unwrap();
        panel_db
            .with(|c| {
                c.execute_batch(
                    "CREATE TABLE tasks (id TEXT PRIMARY KEY, kind TEXT NOT NULL, status TEXT NOT NULL,
                     started_at TEXT NOT NULL, finished_at TEXT, log_blob TEXT NOT NULL DEFAULT '');",
                )
            })
            .unwrap();
        let tasks = TaskTracker::new(panel_db);
        let mariadb =
            crate::db::MariaDb::connect(&format!("mysql://root@localhost:3306/mysql?socket={socket}")).unwrap();
        let (client, sandbox) = find_mariadb_client().await.unwrap();
        eprintln!("client: {client}, sandbox mode: {sandbox}");

        let ran = dir.path().join("ran");
        let marker = ran.display();
        let other = dir.path().join("other.sql");
        std::fs::write(&other, format!("\\! touch {marker}\n")).unwrap();
        let use_db = "CREATE DATABASE IF NOT EXISTS import_guard_test;\nUSE import_guard_test;\n";
        // After `\e`, vi would read what follows as keystrokes; the newlines
        // push the `:!` past what the client has already read.
        let padding = "\n".repeat(70_000);
        let hostile: Vec<(&str, String)> = vec![
            ("line start", format!("{use_db}\\! touch {marker}\n")),
            ("mid line", format!("{use_db}SELECT 1 \\! touch {marker}\n")),
            ("after a statement", format!("{use_db}SELECT 1; system touch {marker}\n")),
            ("versioned comment", format!("{use_db}/*!50000 \\! touch {marker} */;\n")),
            ("named", format!("{use_db}  SYSTEM touch {marker}\n")),
            (
                "string end moved by sql_mode",
                format!("{use_db}SET sql_mode='NO_BACKSLASH_ESCAPES';\nSELECT 'a\\'; \\! touch {marker} #';\n"),
            ),
            ("another charset", format!("{use_db}\\C gbk\nSELECT 1 \u{81}\\\\! touch {marker}\n")),
            ("source", format!("{use_db}source {}\n", other.display())),
            (
                "a local file",
                format!(
                    "{use_db}CREATE TABLE IF NOT EXISTS leak (l TEXT);\nLOAD DATA LOCAL INFILE '{}' INTO TABLE leak;\n",
                    other.display()
                ),
            ),
            ("editor", format!("{use_db}SELECT 1\n\\e\n{padding}:!touch {marker}\n:q!\n")),
        ];
        for (what, sql) in hostile {
            let dump = dir.path().join("hostile.sql");
            // The charset case needs the raw byte 0x81, not U+0081 as UTF-8.
            let bytes: Vec<u8> = sql.chars().flat_map(|c| {
                if c == '\u{81}' { vec![0x81] } else { c.to_string().into_bytes() }
            }).collect();
            std::fs::write(&dump, &bytes).unwrap();
            let task = tasks.create("db:import:import_guard_test");
            let id = task.id.clone();
            run_import_task(
                task, mariadb.clone(), dump, bytes.len() as u64, socket.clone(),
                "import_guard_test".into(), true, true, false, false,
            )
            .await;
            let state = tasks.get(&id).unwrap();
            assert!(!ran.exists(), "{what}: the command ran\n{}", state.log.join("\n"));
            assert!(matches!(state.status, TaskStatus::Failed), "{what}: {}", state.log.join("\n"));
        }

        let dumped = "/*M!999999\\- enable the sandbox mode */ \n-- MariaDB dump 10.19\n\
            DROP DATABASE IF EXISTS import_guard_test;\nCREATE DATABASE import_guard_test;\nUSE `import_guard_test`;\n\
            CREATE TABLE notes (id INT PRIMARY KEY, body TEXT);\n\
            INSERT INTO notes VALUES (1,'it\\'s; system is fine \\\\! \\\\. C:\\\\Code'),(2,'a\\\\');\n\
            DELIMITER ;;\nCREATE PROCEDURE p() BEGIN SELECT 1; END ;;\nDELIMITER ;\n";
        for sql in [dumped.to_string(), dumped.replace('\n', "\r\n")] {
            let dump = dir.path().join("dumped.sql");
            std::fs::write(&dump, &sql).unwrap();
            let task = tasks.create("db:import:import_guard_test");
            let id = task.id.clone();
            run_import_task(
                task, mariadb.clone(), dump, sql.len() as u64, socket.clone(),
                "import_guard_test".into(), true, true, false, false,
            )
            .await;
            let state = tasks.get(&id).unwrap();
            assert!(matches!(state.status, TaskStatus::Succeeded), "{}", state.log.join("\n"));
            let mut conn = mariadb.pool().get_conn().await.unwrap();
            let body: Option<String> = conn
                .query_first("SELECT body FROM import_guard_test.notes WHERE id = 1")
                .await
                .unwrap();
            assert_eq!(body.as_deref(), Some(r"it's; system is fine \! \. C:\Code"));
            let routines: Option<u64> = conn
                .query_first("SELECT COUNT(*) FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA = 'import_guard_test'")
                .await
                .unwrap();
            assert_eq!(routines, Some(1));
        }
        let mut conn = mariadb.pool().get_conn().await.unwrap();
        conn.query_drop("DROP DATABASE IF EXISTS import_guard_test").await.unwrap();
    }
}
