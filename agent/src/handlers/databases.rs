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
    let password = req.password.unwrap_or_else(random_password);
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
    let password = req.password.unwrap_or_else(random_password);
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
    conn.query_drop(format!(
        "GRANT {} ON `{}`.* TO '{}'@'{}'",
        req.privileges,
        escape(&req.database),
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
    conn.query_drop(format!(
        "REVOKE ALL PRIVILEGES ON `{}`.* FROM '{}'@'{}'",
        escape(&req.database),
        escape(&req.user),
        escape(&host)
    ))
    .await?;
    // REVOKE ALL PRIVILEGES leaves the mysql.db row behind when the user
    // also held GRANT OPTION; clearing that needs its own REVOKE, which
    // errors with 1141 when the user never had it — swallow exactly that.
    if let Err(e) = conn
        .query_drop(format!(
            "REVOKE GRANT OPTION ON `{}`.* FROM '{}'@'{}'",
            escape(&req.database),
            escape(&req.user),
            escape(&host)
        ))
        .await
    {
        let no_such_grant =
            matches!(&e, mysql_async::Error::Server(s) if s.code == 1141);
        if !no_such_grant {
            return Err(e.into());
        }
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
/// see it.
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

/// Ubuntu's mariadb-client ships the binary as `mariadb` with a `mysql`
/// compat symlink; other setups only have `mysql`. Try both.
fn spawn_mariadb_client(args: &[String]) -> std::io::Result<tokio::process::Child> {
    for bin in ["mariadb", "mysql"] {
        match Command::new(bin)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => return Ok(c),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "neither `mariadb` nor `mysql` client binary found — install mariadb-client",
    ))
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
fn import_error_hints(output: &[String]) -> Vec<String> {
    let joined = output.join("\n");
    let mut hints = Vec::new();
    if joined.contains("max_allowed_packet") || joined.contains("server has gone away") {
        hints.push(
            "Hint: a statement in this dump exceeds the server's max_allowed_packet. Raise it (e.g. `max_allowed_packet = 256M` under [mysqld] in /etc/mysql/mariadb.conf.d/) and re-run the import."
                .to_string(),
        );
    }
    if joined.contains("Unknown command '\\-'") {
        hints.push(
            "Hint: the dump starts with the MariaDB 10.11.8+ sandbox-mode line and this server's client is too old to understand it. Update mariadb-client and retry."
                .to_string(),
        );
    }
    hints
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
        // create a brand-new target either) — ensure it exists.
        if !dump_defines_db {
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
    ];
    if !dump_defines_db {
        // Only headerless dumps get a database argument. A dump with its own
        // CREATE DATABASE may target a database that doesn't exist yet, and
        // the client refuses to start in a missing one.
        args.push(db.clone());
    }

    let mut child = match spawn_mariadb_client(&args) {
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
            for hint in import_error_hints(&client_output) {
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

fn random_password() -> String {
    use base64::Engine;
    let mut bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}
