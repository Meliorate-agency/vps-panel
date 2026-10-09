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
    let password = req.password.unwrap_or_else(random_password);
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
    let password = req.password.unwrap_or_else(random_password);
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

    let sql = format!(
        "CREATE DATABASE \"{}\"{} ENCODING 'UTF8' TEMPLATE template1;",
        req.name, owner_clause
    );
    psql_exec(&sql).await?;

    let owner = req.owner.unwrap_or_else(|| "postgres".to_string());
    Ok(Json(DatabaseRow {
        name: req.name,
        owner,
    }))
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
    // leaves a half-applied owner on the old name.
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
        psql_exec(&sql).await?;
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
        sql.push_str(&format!(
            "ALTER DEFAULT PRIVILEGES IN SCHEMA public \
             GRANT ALL ON TABLES TO \"{}\";\n",
            req.user
        ));
        sql.push_str(&format!(
            "ALTER DEFAULT PRIVILEGES IN SCHEMA public \
             GRANT ALL ON SEQUENCES TO \"{}\";\n",
            req.user
        ));
    } else if !table_privs.is_empty() {
        let t = table_privs.join(", ");
        sql.push_str(&format!(
            "GRANT {t} ON ALL TABLES IN SCHEMA public TO \"{}\";\n",
            req.user
        ));
        sql.push_str(&format!(
            "ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT {t} ON TABLES TO \"{}\";\n",
            req.user
        ));
        if table_privs.contains(&"INSERT") {
            // serial / identity columns draw from sequences.
            sql.push_str(&format!(
                "GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO \"{}\";\n",
                req.user
            ));
            sql.push_str(&format!(
                "ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT USAGE, SELECT ON SEQUENCES TO \"{}\";\n",
                req.user
            ));
        }
    }
    psql_exec(&sql).await?;
    Ok(Json(crate::ipc::OkResponse::default()))
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

fn random_password() -> String {
    use base64::Engine;
    let mut bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}
