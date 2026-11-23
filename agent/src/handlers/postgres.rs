use crate::error::{AgentError, AgentResult};
use crate::exec;
use axum::{
    extract::Path,
    Json,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};

// ─── Inputs / outputs ───────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateRole {
    pub user: String,
    /// Optional. Omitted → server-generated random password.
    pub password: Option<String>,
    /// When true, the role is created with the SUPERUSER attribute. Default
    /// false — operators creating an app user almost never want this.
    #[serde(default)]
    pub superuser: bool,
}

#[derive(Deserialize)]
pub struct CreateDatabase {
    pub name: String,
    /// Optional role to set as the database owner. Recommended — without
    /// this the database is owned by `postgres` and the application role
    /// can't create tables in it without extra grants. If omitted, the
    /// owner defaults to `postgres`.
    pub owner: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateDatabase {
    /// New database name. When present and different from the current name,
    /// the database is renamed. Omitted/empty → name unchanged.
    pub new_name: Option<String>,
    /// New owner role. When present and non-empty, the database-level owner
    /// is changed (ALTER DATABASE … OWNER TO). DB-level only — existing
    /// tables keep their owner, matching pgAdmin's "Properties → owner".
    pub owner: Option<String>,
}

#[derive(Deserialize)]
pub struct Grant {
    pub user: String,
    pub database: String,
    /// One of the keys in `is_valid_priv` below. Defaults to
    /// "ALL PRIVILEGES" for parity with the MariaDB endpoint.
    #[serde(default = "default_priv")]
    pub privileges: String,
}

#[derive(Serialize)]
pub struct RoleRow {
    pub user: String,
    /// True when the role has the SUPERUSER attribute. Surfaced so the UI
    /// can mark admin roles, similar to the `sudo` flag on system users.
    pub superuser: bool,
}

#[derive(Serialize)]
pub struct DatabaseRow {
    pub name: String,
    pub owner: String,
}

#[derive(Serialize)]
pub struct CreateRoleResp {
    pub user: String,
    pub initial_password: String,
}

#[derive(Deserialize)]
pub struct PgvectorReq {
    /// Database to enable/disable the `vector` extension on. CREATE/DROP
    /// EXTENSION is per-database, so this targets one DB at a time.
    pub database: String,
}

#[derive(Serialize)]
pub struct PgvectorStatus {
    /// True when the pgvector package is installed on the server (the
    /// `vector.control` file exists). When false the UI disables the
    /// per-database control and points the operator at the install card.
    pub available: bool,
    /// Names of databases that currently have the `vector` extension enabled.
    pub enabled: Vec<String>,
}

#[derive(Deserialize)]
pub struct SetRolePassword {
    /// Omitted → server-generated random password.
    pub password: Option<String>,
}

#[derive(Serialize)]
pub struct SetRolePasswordResp {
    pub user: String,
    pub password: String,
}

fn default_priv() -> String {
    "ALL PRIVILEGES".into()
}

// ─── Reserved names ─────────────────────────────────────────────────────
//
// We refuse to create, drop, or grant against PostgreSQL's built-in
// roles + system databases. `pg_*` is the prefix Postgres reserves for
// its own roles (pg_monitor, pg_read_all_settings, etc.) — covered by a
// prefix check below so we don't have to enumerate every version.

const RESERVED_USERS: &[&str] = &[
    "",
    "postgres",
    "rdsadmin", // RDS-style hosted-Postgres superuser; harmless to block
];

const RESERVED_DBS: &[&str] = &["postgres", "template0", "template1"];

fn is_reserved_user(s: &str) -> bool {
    RESERVED_USERS.iter().any(|r| r.eq_ignore_ascii_case(s)) || s.starts_with("pg_")
}

fn is_reserved_db(s: &str) -> bool {
    RESERVED_DBS.iter().any(|r| r.eq_ignore_ascii_case(s))
}

// ─── psql plumbing ──────────────────────────────────────────────────────
//
// All operations run via `sudo -u postgres psql` using Unix peer
// authentication — that's the default `pg_hba.conf` on a fresh Debian
// install, no password needed since the agent runs as root.
//
// SQL is always passed via stdin; we never construct a shell command line
// from operator input. Combined with strict identifier validation
// (`validate_ident`) and double-quote wrapping for identifiers, that
// closes the obvious injection paths.
//
// `-X` skips the user's .psqlrc (forensic safety + reproducibility),
// `-v ON_ERROR_STOP=1` makes the process exit non-zero on the first error
// instead of silently continuing past it.

async fn psql_exec(sql: &str) -> AgentResult<()> {
    exec::run_with_input(
        "sudo",
        ["-u", "postgres", "psql", "-X", "-v", "ON_ERROR_STOP=1"],
        sql,
    )
    .await?;
    Ok(())
}

/// Run a SELECT and return its stdout. `-tA` produces tuple-only output
/// without column headers, `-F<tab>` sets the field separator so we can
/// parse rows with `split('\t')`.
async fn psql_query(sql: &str) -> AgentResult<String> {
    let out = exec::run_with_input(
        "sudo",
        [
            "-u",
            "postgres",
            "psql",
            "-X",
            "-tA",
            "-F",
            "\t",
            "-v",
            "ON_ERROR_STOP=1",
        ],
        sql,
    )
    .await?;
    Ok(out.stdout)
}

/// Like `psql_exec`, but connects to a specific database (`-d <db>`) instead
/// of the default `postgres` DB. Needed for per-database operations such as
/// `CREATE EXTENSION`. `db` is passed as its own argv element (never
/// interpolated into SQL), so it can't smuggle SQL; callers still validate it.
async fn psql_exec_on_db(db: &str, sql: &str) -> AgentResult<()> {
    exec::run_with_input(
        "sudo",
        [
            "-u", "postgres", "psql", "-X", "-v", "ON_ERROR_STOP=1", "-d", db,
        ],
        sql,
    )
    .await?;
    Ok(())
}

/// `psql_query` against a specific database (`-d <db>`). Same parsing
/// conventions (`-tA`, tab field separator).
async fn psql_query_on_db(db: &str, sql: &str) -> AgentResult<String> {
    let out = exec::run_with_input(
        "sudo",
        [
            "-u", "postgres", "psql", "-X", "-tA", "-F", "\t", "-v",
            "ON_ERROR_STOP=1", "-d", db,
        ],
        sql,
    )
    .await?;
    Ok(out.stdout)
}

// ─── Handlers ───────────────────────────────────────────────────────────

pub async fn list_users() -> AgentResult<Json<Vec<RoleRow>>> {
    // pg_roles surfaces every login + non-login role. We filter to login
    // roles (rolcanlogin) because the panel only manages "users" in the
    // app-level sense; pure group roles aren't useful to expose here.
    let sql = "SELECT rolname, rolsuper FROM pg_roles WHERE rolcanlogin ORDER BY rolname";
    let out = psql_query(sql).await?;
    let mut rows = Vec::new();
    for line in out.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut parts = line.split('\t');
        let user = parts.next().unwrap_or("").to_string();
        let super_str = parts.next().unwrap_or("").trim();
        if is_reserved_user(&user) {
            continue;
        }
        rows.push(RoleRow {
            user,
            // psql renders bool as `t` / `f` with `-tA`.
            superuser: super_str.eq_ignore_ascii_case("t"),
        });
    }
    Ok(Json(rows))
}

pub async fn list_databases() -> AgentResult<Json<Vec<DatabaseRow>>> {
    // pg_database is the canonical list. We join pg_authid (visible to
    // postgres superuser) to get the owner role name instead of an OID.
    let sql = "SELECT d.datname, r.rolname \
               FROM pg_database d \
               JOIN pg_authid r ON r.oid = d.datdba \
               WHERE NOT d.datistemplate \
               ORDER BY d.datname";
    let out = psql_query(sql).await?;
    let mut rows = Vec::new();
    for line in out.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut parts = line.split('\t');
        let name = parts.next().unwrap_or("").to_string();
        let owner = parts.next().unwrap_or("").to_string();
        if is_reserved_db(&name) {
            continue;
        }
        rows.push(DatabaseRow { name, owner });
    }
    Ok(Json(rows))
}

pub async fn create_role(Json(req): Json<CreateRole>) -> AgentResult<Json<CreateRoleResp>> {
    validate_ident(&req.user)?;
    if is_reserved_user(&req.user) {
        return Err(AgentError::Invalid(format!(
            "'{}' is a reserved system role — pick a different name",
            req.user
        )));
    }
    let password = password_or_random(req.password);
    let attrs = if req.superuser {
        "LOGIN SUPERUSER"
    } else {
        "LOGIN"
    };
    let sql = format!(
        "CREATE ROLE \"{}\" {} PASSWORD '{}';",
        req.user,
        attrs,
        escape_string(&password)
    );
    psql_exec(&sql).await?;
    Ok(Json(CreateRoleResp {
        user: req.user,
        initial_password: password,
    }))
}

pub async fn set_role_password(
    Path(user): Path<String>,
    Json(req): Json<SetRolePassword>,
) -> AgentResult<Json<SetRolePasswordResp>> {
    validate_ident(&user)?;
    if is_reserved_user(&user) {
        return Err(AgentError::Invalid(format!(
            "'{user}' is a reserved system role — refusing to change its password"
        )));
    }
    let password = password_or_random(req.password);
    let sql = format!(
        "ALTER ROLE \"{}\" WITH PASSWORD '{}';",
        user,
        escape_string(&password)
    );
    psql_exec(&sql).await?;
    Ok(Json(SetRolePasswordResp { user, password }))
}

pub async fn create_database(
    Json(req): Json<CreateDatabase>,
) -> AgentResult<Json<DatabaseRow>> {
    validate_ident(&req.name)?;
    if is_reserved_db(&req.name) {
        return Err(AgentError::Invalid(format!(
            "'{}' is a reserved system database — pick a different name",
            req.name
        )));
    }

    // Owner is optional but recommended: a database owned by `postgres`
    // means an application role can't create tables in it without an
    // extra schema-level grant. If the operator passes an owner we set
    // it on creation; otherwise the DB ends up owned by `postgres` and
    // they'll need a follow-up grant.
    let owner_clause = if let Some(owner) = req.owner.as_deref() {
        let owner = owner.trim();
        if owner.is_empty() {
            String::new()
        } else {
            validate_ident(owner)?;
            if is_reserved_user(owner) {
                return Err(AgentError::Invalid(format!(
                    "'{owner}' is a reserved system role — refusing to set as owner"
                )));
            }
            format!(" OWNER \"{owner}\"")
        }
    } else {
        String::new()
    };

    let template1_encoding = psql_query(
        "SELECT pg_encoding_to_char(encoding) FROM pg_database WHERE datname = 'template1'",
    )
    .await?;
    let template1_encoding = template1_encoding.trim();
    if template1_encoding != "UTF8" {
        tracing::info!(
            database = %req.name,
            template1_encoding,
            "template1 is not UTF8: creating the database from template0 with locale C.UTF-8"
        );
    }
    psql_exec(&create_database_sql(&req.name, &owner_clause, template1_encoding)).await?;

    let owner = req.owner.unwrap_or_else(|| "postgres".to_string());
    Ok(Json(DatabaseRow {
        name: req.name,
        owner,
    }))
}

/// CREATE DATABASE for a UTF8 database. A cluster made while the server's
/// locale was C or POSIX has an SQL_ASCII template1, and a copy of it cannot
/// be given another encoding ("new encoding (UTF8) is incompatible with the
/// encoding of the template database"): every create failed there. template0
/// can, but its locale has to fit UTF8 as well, so it is named: C.UTF-8,
/// which glibc always has. Extensions the operator put into template1 are
/// not in a database made this way. LC_COLLATE / LC_CTYPE and not LOCALE,
/// which only PostgreSQL 13 and later know.
fn create_database_sql(name: &str, owner_clause: &str, template1_encoding: &str) -> String {
    let template = if template1_encoding == "UTF8" {
        "TEMPLATE template1"
    } else {
        "LC_COLLATE 'C.UTF-8' LC_CTYPE 'C.UTF-8' TEMPLATE template0"
    };
    format!("CREATE DATABASE \"{name}\"{owner_clause} ENCODING 'UTF8' {template};")
}

pub async fn update_database(
    Path(name): Path<String>,
    Json(req): Json<UpdateDatabase>,
) -> AgentResult<Json<crate::ipc::OkResponse>> {
    validate_ident(&name)?;
    if is_reserved_db(&name) {
        return Err(AgentError::Invalid(format!(
            "'{name}' is a reserved system database — refusing to modify"
        )));
    }

    // Build the statement list. Rename FIRST, then set the owner on the
    // resulting name: a rename blocked by active connections aborts (under
    // ON_ERROR_STOP) before any owner change lands, so the operation never
    // leaves a half-applied owner on the old name. Both run in one
    // transaction: an owner that no longer exists used to leave the database
    // renamed under an error, and the next save failed on the old name.
    let mut sql = String::new();
    let mut target = name.clone();

    if let Some(new_name) = req.new_name.as_deref() {
        let new_name = new_name.trim();
        if !new_name.is_empty() && new_name != name {
            validate_ident(new_name)?;
            if is_reserved_db(new_name) {
                return Err(AgentError::Invalid(format!(
                    "'{new_name}' is a reserved system database — pick a different name"
                )));
            }
            sql.push_str(&format!(
                "ALTER DATABASE \"{name}\" RENAME TO \"{new_name}\";\n"
            ));
            target = new_name.to_string();
        }
    }

    if let Some(owner) = req.owner.as_deref() {
        let owner = owner.trim();
        if !owner.is_empty() {
            validate_ident(owner)?;
            if is_reserved_user(owner) {
                return Err(AgentError::Invalid(format!(
                    "'{owner}' is a reserved system role — refusing to set as owner"
                )));
            }
            sql.push_str(&format!(
                "ALTER DATABASE \"{target}\" OWNER TO \"{owner}\";\n"
            ));
        }
    }

    // Nothing actually changed (empty request, or new_name == current) — no
    // SQL to run. Treat as a successful no-op so the UI can save without a
    // diff without surfacing a spurious error.
    if !sql.is_empty() {
        psql_exec(&format!("BEGIN;\n{sql}COMMIT;\n")).await?;
    }
    Ok(Json(crate::ipc::OkResponse::default()))
}

pub async fn grant(Json(req): Json<Grant>) -> AgentResult<Json<crate::ipc::OkResponse>> {
    validate_ident(&req.user)?;
    validate_ident(&req.database)?;
    if is_reserved_user(&req.user) {
        return Err(AgentError::Invalid(format!(
            "'{}' is a reserved system role — refusing to grant",
            req.user
        )));
    }
    if is_reserved_db(&req.database) {
        return Err(AgentError::Invalid(format!(
            "'{}' is a reserved system database — refusing to grant",
            req.database
        )));
    }
    if !is_valid_priv(&req.privileges) {
        return Err(AgentError::Invalid("invalid privileges spec".into()));
    }

    // Postgres permission model is more layered than MariaDB:
    //   1. GRANT on the database (CREATE/CONNECT/TEMP)
    //   2. GRANT on the schema (USAGE/CREATE — needed to create tables)
    //   3. GRANT on objects (tables/sequences/functions — for existing)
    //   4. ALTER DEFAULT PRIVILEGES — for future objects
    //
    // Doing only (1) is the trap operators hit: GRANT ALL on the database
    // doesn't actually let the user create tables, because tables live in
    // schemas (default `public`) which the database grant doesn't cover.
    // We do (1) + (2) here so the panel grant matches operator
    // expectation. For ALL PRIVILEGES we also do (3) + (4) so existing
    // and future tables are reachable. Operators can use psql for finer
    // grants.
    //
    // The schema/object grants run inside the target database via
    // `\connect`, since they're per-database.
    // Database-level privileges are only CONNECT / CREATE / TEMPORARY;
    // SELECT / INSERT / UPDATE / DELETE apply to tables. Those options were
    // sent as `GRANT SELECT ON DATABASE`, which Postgres rejects, so they
    // always failed. They now become CONNECT on the database plus table (and,
    // for INSERT, sequence) grants in its public schema, existing and future.
    let all = req.privileges == "ALL PRIVILEGES";
    let requested: Vec<&str> = req.privileges.split(',').map(str::trim).collect();
    let table_privs: Vec<&str> = requested
        .iter()
        .copied()
        .filter(|p| matches!(*p, "SELECT" | "INSERT" | "UPDATE" | "DELETE"))
        .collect();
    let mut db_privs: Vec<&str> = requested
        .iter()
        .copied()
        .filter(|p| matches!(*p, "CONNECT" | "TEMPORARY" | "CREATE"))
        .collect();
    if !table_privs.is_empty() && !db_privs.contains(&"CONNECT") {
        db_privs.push("CONNECT");
    }
    let db_grant = if all { "ALL PRIVILEGES".to_string() } else { db_privs.join(", ") };

    let mut sql = String::new();
    if !db_grant.is_empty() {
        sql.push_str(&format!(
            "GRANT {db_grant} ON DATABASE \"{db}\" TO \"{user}\";\n",
            db = req.database,
            user = req.user,
        ));
    }
    sql.push_str(&format!("\\connect \"{}\"\n", req.database));
    sql.push_str(&format!(
        "GRANT {priv} ON SCHEMA public TO \"{user}\";\n",
        priv = if all { "ALL PRIVILEGES" } else { "USAGE" },
        user = req.user,
    ));
    if all {
        sql.push_str(&format!(
            "GRANT ALL PRIVILEGES ON ALL TABLES IN SCHEMA public TO \"{}\";\n",
            req.user
        ));
        sql.push_str(&format!(
            "GRANT ALL PRIVILEGES ON ALL SEQUENCES IN SCHEMA public TO \"{}\";\n",
            req.user
        ));
        sql.push_str(&default_privileges("ALL", "TABLES", &req.user));
        sql.push_str(&default_privileges("ALL", "SEQUENCES", &req.user));
    } else if !table_privs.is_empty() {
        let t = table_privs.join(", ");
        sql.push_str(&format!(
            "GRANT {t} ON ALL TABLES IN SCHEMA public TO \"{}\";\n",
            req.user
        ));
        sql.push_str(&default_privileges(&t, "TABLES", &req.user));
        if table_privs.contains(&"INSERT") {
            // serial / identity columns draw from sequences.
            sql.push_str(&format!(
                "GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO \"{}\";\n",
                req.user
            ));
            sql.push_str(&default_privileges("USAGE, SELECT", "SEQUENCES", &req.user));
        }
    }
    psql_exec(&sql).await?;
    Ok(Json(crate::ipc::OkResponse::default()))
}

/// `ALTER DEFAULT PRIVILEGES` for what is made in schema public later, run
/// inside the target database. Without FOR ROLE it covers only objects the
/// role running it (postgres) creates. An app's tables are made by the
/// database's owner, so a table added after the grant stayed out of reach of
/// the granted role ("permission denied for table"); the second statement
/// covers the owner.
fn default_privileges(privs: &str, objects: &str, user: &str) -> String {
    format!(
        "ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT {privs} ON {objects} TO \"{user}\";\n\
         SELECT format('ALTER DEFAULT PRIVILEGES FOR ROLE %I IN SCHEMA public GRANT {privs} ON {objects} TO \"{user}\"', r.rolname) \
         FROM pg_database d JOIN pg_roles r ON r.oid = d.datdba \
         WHERE d.datname = current_database() AND r.rolname NOT IN (current_user, '{user}') \\gexec\n"
    )
}

pub async fn delete_role(Path(user): Path<String>) -> AgentResult<Json<crate::ipc::OkResponse>> {
    validate_ident(&user)?;
    if is_reserved_user(&user) {
        return Err(AgentError::Invalid(format!(
            "'{user}' is a reserved system role — refusing to drop"
        )));
    }
    // REASSIGN OWNED first: Postgres refuses DROP ROLE while the role
    // still owns objects (tables, sequences, schemas) anywhere. We hand
    // ownership to `postgres` so the drop succeeds; the operator can
    // re-own afterwards if needed. DROP OWNED clears default-privilege
    // grants the role held.
    //
    // REASSIGN OWNED / DROP OWNED only act on the database they run in. Run
    // only in `postgres`, they left the role owning objects in its app
    // database, so DROP ROLE failed for exactly the roles apps use. Run them
    // in every database that accepts connections.
    let exists = psql_query(&format!(
        "SELECT 1 FROM pg_roles WHERE rolname = '{}'",
        escape_string(&user)
    ))
    .await?;
    if exists.trim().is_empty() {
        return Ok(Json(crate::ipc::OkResponse::default()));
    }
    let dbs = psql_query("SELECT datname FROM pg_database WHERE datallowconn AND NOT datistemplate")
        .await?;
    for db in dbs.lines().map(str::trim).filter(|d| !d.is_empty()) {
        psql_exec_on_db(
            db,
            &format!("REASSIGN OWNED BY \"{user}\" TO postgres;\nDROP OWNED BY \"{user}\";\n"),
        )
        .await?;
    }
    psql_exec(&format!("DROP ROLE IF EXISTS \"{user}\";\n")).await?;
    Ok(Json(crate::ipc::OkResponse::default()))
}

pub async fn delete_database(
    Path(name): Path<String>,
) -> AgentResult<Json<crate::ipc::OkResponse>> {
    validate_ident(&name)?;
    if is_reserved_db(&name) {
        return Err(AgentError::Invalid(format!(
            "'{name}' is a reserved system database — refusing to drop"
        )));
    }
    // WITH (FORCE) terminates active connections so the drop doesn't
    // fail with "database is being accessed by other users" when a stray
    // psql shell or app is still attached. Available since Postgres 13;
    // Debian stable ships ≥ 13 so this is safe to assume.
    let sql = format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE);");
    psql_exec(&sql).await?;
    Ok(Json(crate::ipc::OkResponse::default()))
}

// ─── pgvector extension ─────────────────────────────────────────────────
//
// pgvector is two layers: the system package (installed once per server from
// the Quick Install strip) and a per-database `CREATE EXTENSION vector`. These
// handlers cover the per-database layer plus a status probe the databases page
// uses to render each row's Enable/Enabled control.

pub async fn enable_pgvector(
    Json(req): Json<PgvectorReq>,
) -> AgentResult<Json<crate::ipc::OkResponse>> {
    validate_ident(&req.database)?;
    if is_reserved_db(&req.database) {
        return Err(AgentError::Invalid(format!(
            "'{}' is a reserved system database — refusing to modify",
            req.database
        )));
    }
    // IF NOT EXISTS makes re-enabling a no-op success. Fails cleanly with
    // "could not open extension control file" when the package isn't installed
    // yet — the panel maps that to a friendly "install pgvector first" message.
    psql_exec_on_db(&req.database, "CREATE EXTENSION IF NOT EXISTS vector;").await?;
    Ok(Json(crate::ipc::OkResponse::default()))
}

pub async fn disable_pgvector(
    Json(req): Json<PgvectorReq>,
) -> AgentResult<Json<crate::ipc::OkResponse>> {
    validate_ident(&req.database)?;
    if is_reserved_db(&req.database) {
        return Err(AgentError::Invalid(format!(
            "'{}' is a reserved system database — refusing to modify",
            req.database
        )));
    }
    // No CASCADE on purpose: if tables still use vector columns the DROP fails
    // with a dependency error the UI surfaces, instead of silently destroying
    // those columns and their data.
    psql_exec_on_db(&req.database, "DROP EXTENSION IF EXISTS vector;").await?;
    Ok(Json(crate::ipc::OkResponse::default()))
}

pub async fn pgvector_status() -> AgentResult<Json<PgvectorStatus>> {
    // Reuse the same control-file scan the Quick Install card uses, so
    // "available" here and the card's "installed" badge can never disagree.
    let available = crate::handlers::quick_install::pg_extension_available("vector").await;
    if !available {
        return Ok(Json(PgvectorStatus {
            available: false,
            enabled: Vec::new(),
        }));
    }

    // Extensions are per-database catalog entries, so probe each managed DB.
    // Same DB set the page lists (non-template, non-reserved).
    let list_sql = "SELECT d.datname \
                    FROM pg_database d \
                    JOIN pg_authid r ON r.oid = d.datdba \
                    WHERE NOT d.datistemplate \
                    ORDER BY d.datname";
    let out = psql_query(list_sql).await?;
    let mut enabled = Vec::new();
    for line in out.lines() {
        let name = line.trim();
        if name.is_empty() || is_reserved_db(name) {
            continue;
        }
        // Per-DB probe; on any error (e.g. a DB that refuses the connection)
        // skip it rather than failing the whole status call.
        if let Ok(res) =
            psql_query_on_db(name, "SELECT 1 FROM pg_extension WHERE extname='vector'").await
        {
            if res.lines().any(|l| l.trim() == "1") {
                enabled.push(name.to_string());
            }
        }
    }
    Ok(Json(PgvectorStatus {
        available: true,
        enabled,
    }))
}

// ─── Validation helpers ─────────────────────────────────────────────────

fn validate_ident(s: &str) -> AgentResult<()> {
    // Conservative: unquoted Postgres identifiers, ASCII-only, starting
    // with a letter or underscore. Anything outside this would require
    // proper double-quote escaping of the input which we don't bother
    // with since the panel UI only exposes a normal text input.
    let re = regex::Regex::new(r"^[A-Za-z_][A-Za-z0-9_]{0,62}$").unwrap();
    if !re.is_match(s) {
        return Err(AgentError::Invalid(format!("invalid identifier: {s}")));
    }
    Ok(())
}

fn is_valid_priv(s: &str) -> bool {
    // Mirrors the MariaDB endpoint's allowlist (with the synonyms that
    // make sense in Postgres). Anything outside this is rejected so a
    // typoed privilege can't smuggle additional SQL through the format!.
    let allowed = [
        "ALL PRIVILEGES",
        "SELECT",
        "INSERT",
        "UPDATE",
        "DELETE",
        "CONNECT",
        "TEMPORARY",
        "CREATE",
        "SELECT, INSERT, UPDATE, DELETE",
    ];
    allowed.contains(&s)
}

fn escape_string(s: &str) -> String {
    // SQL string literal escape — double up single quotes. Backslashes
    // don't need escaping in standard_conforming_strings mode (default
    // since Postgres 9.1) which is what every supported Debian release
    // ships with.
    s.replace('\'', "''")
}

/// The caller's password, or a generated one. An empty string counts as none:
/// `PASSWORD ''` leaves the role without a password (it cannot log in with
/// one) while the response said the password was "".
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
    fn default_privileges_cover_postgres_and_the_database_owner() {
        let sql = default_privileges("USAGE, SELECT", "SEQUENCES", "reporter");
        let mut lines = sql.lines();
        assert_eq!(
            lines.next(),
            Some("ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT USAGE, SELECT ON SEQUENCES TO \"reporter\";")
        );
        // psql runs each row of this query as a statement (\gexec): one for
        // the owner, none when postgres or the granted role owns the database.
        let owner = lines.next().unwrap();
        assert!(owner.starts_with(
            "SELECT format('ALTER DEFAULT PRIVILEGES FOR ROLE %I IN SCHEMA public GRANT USAGE, SELECT ON SEQUENCES TO \"reporter\"', r.rolname) FROM pg_database d"
        ));
        assert!(owner.ends_with("r.rolname NOT IN (current_user, 'reporter') \\gexec"));
        assert_eq!(lines.next(), None);
        assert!(sql.ends_with('\n'));
    }

    #[test]
    fn a_database_on_a_non_utf8_cluster_is_made_from_template0() {
        assert_eq!(
            create_database_sql("shop", " OWNER \"app\"", "UTF8"),
            "CREATE DATABASE \"shop\" OWNER \"app\" ENCODING 'UTF8' TEMPLATE template1;"
        );
        let from_template0 =
            "CREATE DATABASE \"shop\" ENCODING 'UTF8' LC_COLLATE 'C.UTF-8' LC_CTYPE 'C.UTF-8' TEMPLATE template0;";
        assert_eq!(create_database_sql("shop", "", "SQL_ASCII"), from_template0);
        assert_eq!(create_database_sql("shop", "", "LATIN1"), from_template0);
        // No template1 at all (the query came back empty).
        assert_eq!(create_database_sql("shop", "", ""), from_template0);
    }

    #[test]
    fn an_empty_password_is_replaced_by_a_generated_one() {
        assert_eq!(password_or_random(Some("s3cret".into())), "s3cret");
        assert!(password_or_random(Some(String::new())).len() >= 24);
        assert!(password_or_random(None).len() >= 24);
    }
}
