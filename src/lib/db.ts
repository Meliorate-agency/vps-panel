import "server-only";
import Database from "better-sqlite3";
import { config } from "./config";
import path from "node:path";
import fs from "node:fs";

let _db: Database.Database | null = null;

export function db(): Database.Database {
  if (_db) return _db;
  const dbPath = config.panelDbPath;
  fs.mkdirSync(path.dirname(dbPath), { recursive: true });
  const handle = new Database(dbPath);
  handle.pragma("journal_mode = WAL");
  handle.pragma("foreign_keys = ON");
  handle.pragma("busy_timeout = 5000");
  applyMigrations(handle);
  _db = handle;
  return handle;
}

function applyMigrations(handle: Database.Database) {
  handle.exec(`
    CREATE TABLE IF NOT EXISTS users (
      id              INTEGER PRIMARY KEY AUTOINCREMENT,
      email           TEXT NOT NULL UNIQUE,
      password_hash   TEXT NOT NULL,
      totp_secret     TEXT,
      totp_enabled    INTEGER NOT NULL DEFAULT 0,
      role            TEXT NOT NULL DEFAULT 'admin',
      created_at      TEXT NOT NULL DEFAULT (datetime('now'))
    );

    CREATE TABLE IF NOT EXISTS sessions (
      jti          TEXT PRIMARY KEY,
      user_id      INTEGER NOT NULL,
      expires_at   TEXT NOT NULL,
      revoked      INTEGER NOT NULL DEFAULT 0,
      FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
    );

    CREATE TABLE IF NOT EXISTS login_attempts (
      id        INTEGER PRIMARY KEY AUTOINCREMENT,
      ip        TEXT NOT NULL,
      email     TEXT,
      ts        TEXT NOT NULL DEFAULT (datetime('now')),
      success   INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX IF NOT EXISTS idx_login_attempts_ip ON login_attempts (ip, ts);
    CREATE INDEX IF NOT EXISTS idx_login_attempts_email ON login_attempts (email, ts);

    CREATE TABLE IF NOT EXISTS tasks (
      id           TEXT PRIMARY KEY,
      kind         TEXT NOT NULL,
      status       TEXT NOT NULL,
      started_at   TEXT NOT NULL,
      finished_at  TEXT,
      log_blob     TEXT NOT NULL DEFAULT ''
    );

    -- Per-folder command history for /terminal. The UNIQUE index is the whole
    -- design: recording a command is an upsert that bumps last_used_at, so a
    -- repeated command reorders to the top instead of appearing twice.
    -- Scoped by actor_id as well as path — this is "my recent commands here",
    -- not a shared log (audit_log is the shared log).
    CREATE TABLE IF NOT EXISTS terminal_history (
      id            INTEGER PRIMARY KEY AUTOINCREMENT,
      actor_id      INTEGER NOT NULL,
      path          TEXT NOT NULL,
      command       TEXT NOT NULL,
      uses          INTEGER NOT NULL DEFAULT 1,
      last_used_at  TEXT NOT NULL DEFAULT (datetime('now')),
      FOREIGN KEY (actor_id) REFERENCES users(id) ON DELETE CASCADE
    );
    CREATE UNIQUE INDEX IF NOT EXISTS idx_terminal_history_unique
      ON terminal_history (actor_id, path, command);
    CREATE INDEX IF NOT EXISTS idx_terminal_history_recent
      ON terminal_history (actor_id, path, last_used_at DESC);

    CREATE TABLE IF NOT EXISTS audit_log (
      id            INTEGER PRIMARY KEY AUTOINCREMENT,
      ts            TEXT NOT NULL DEFAULT (datetime('now')),
      actor_id      INTEGER,
      action        TEXT NOT NULL,
      target        TEXT,
      request_hash  TEXT,
      result        TEXT NOT NULL,
      message       TEXT
    );

    CREATE TABLE IF NOT EXISTS apps (
      name          TEXT PRIMARY KEY,
      source_dir    TEXT NOT NULL,
      port          INTEGER NOT NULL,
      subdomain     TEXT NOT NULL,
      runtime       TEXT NOT NULL DEFAULT 'node',
      entrypoint    TEXT NOT NULL DEFAULT 'npm start',
      ssl_enabled   INTEGER NOT NULL DEFAULT 0,
      repo_url      TEXT,
      created_at    TEXT NOT NULL DEFAULT (datetime('now'))
    );

    CREATE TABLE IF NOT EXISTS domains (
      name        TEXT PRIMARY KEY,
      created_at  TEXT NOT NULL DEFAULT (datetime('now')),
      dnssec      INTEGER NOT NULL DEFAULT 0
    );

    CREATE TABLE IF NOT EXISTS backup_jobs (
      id            INTEGER PRIMARY KEY AUTOINCREMENT,
      schedule      TEXT NOT NULL,
      includes      TEXT NOT NULL,
      last_run_at   TEXT,
      last_status   TEXT
    );

    CREATE TABLE IF NOT EXISTS notifications (
      id        INTEGER PRIMARY KEY AUTOINCREMENT,
      channel   TEXT NOT NULL,
      target    TEXT NOT NULL,
      events    TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS settings (
      key      TEXT PRIMARY KEY,
      value    TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS sites (
      domain               TEXT PRIMARY KEY,
      mode                 TEXT NOT NULL DEFAULT 'port' CHECK(mode IN ('port','folder')),
      upstream_port        INTEGER,
      folder_path          TEXT,
      force_www_redirect   INTEGER NOT NULL DEFAULT 0,
      ssl_requested        INTEGER NOT NULL DEFAULT 1,
      ssl_issued_at        TEXT,
      last_ssl_error       TEXT,
      created_at           TEXT NOT NULL DEFAULT (datetime('now'))
    );

    -- Operator-defined folders for visually organising apps on /apps.
    -- Purely cosmetic — does not affect how apps run or where their code
    -- lives. Apps reference a folder via apps.folder_id (nullable);
    -- deleting a folder sets the referencing apps back to NULL (ungrouped).
    CREATE TABLE IF NOT EXISTS app_folders (
      id            INTEGER PRIMARY KEY AUTOINCREMENT,
      name          TEXT NOT NULL,
      sort_order    INTEGER NOT NULL DEFAULT 0,
      collapsed     INTEGER NOT NULL DEFAULT 0,
      created_at    TEXT NOT NULL DEFAULT (datetime('now'))
    );

    -- 301 / 302 redirect rules, one row per (domain, source path, match mode).
    -- Source of truth for /redirects; the agent only renders the per-domain
    -- nginx include from the rows the panel sends it (lib/redirects.ts).
    CREATE TABLE IF NOT EXISTS redirects (
      id            INTEGER PRIMARY KEY AUTOINCREMENT,
      domain        TEXT NOT NULL,
      source_path   TEXT NOT NULL,
      target_url    TEXT NOT NULL,
      code          INTEGER NOT NULL DEFAULT 301 CHECK(code IN (301, 302)),
      match_mode    TEXT NOT NULL DEFAULT 'exact' CHECK(match_mode IN ('exact', 'prefix')),
      created_at    TEXT NOT NULL DEFAULT (datetime('now'))
    );
    CREATE UNIQUE INDEX IF NOT EXISTS idx_redirects_unique
      ON redirects (domain, source_path, match_mode);

    -- My Networks (lib/networks): servers that ping each other every 10 s.
    -- The owner signs the member list (roster_json + roster_sig); every
    -- member keeps the latest signed copy and hands it on to members that
    -- are behind. network_members holds that list plus this server's view
    -- of each member. Times in the member and invite tables are ms since
    -- the epoch.
    CREATE TABLE IF NOT EXISTS networks (
      id              TEXT PRIMARY KEY,
      name            TEXT NOT NULL,
      owner_key       TEXT NOT NULL,
      roster_version  INTEGER NOT NULL DEFAULT 0,
      roster_json     TEXT NOT NULL DEFAULT '',
      roster_sig      TEXT NOT NULL DEFAULT '',
      created_at      TEXT NOT NULL DEFAULT (datetime('now'))
    );
    CREATE TABLE IF NOT EXISTS network_members (
      network_id      TEXT NOT NULL REFERENCES networks(id) ON DELETE CASCADE,
      public_key      TEXT NOT NULL,
      name            TEXT NOT NULL,
      url             TEXT NOT NULL,
      joined_at       INTEGER NOT NULL,
      -- The name it reports in its pings (it may be renamed after joining).
      live_name       TEXT,
      -- Its last ping that reached this server / this server's last ping
      -- that it answered.
      last_in_at      INTEGER,
      last_out_ok_at  INTEGER,
      last_rtt_ms     INTEGER,
      out_error       TEXT,
      stats_json      TEXT,
      -- When the offline alert went out; NULL while it is up.
      alerted_at      INTEGER,
      PRIMARY KEY (network_id, public_key)
    );
    -- One-time join codes. Only a hash of the secret is kept.
    CREATE TABLE IF NOT EXISTS network_invites (
      id              TEXT PRIMARY KEY,
      network_id      TEXT NOT NULL REFERENCES networks(id) ON DELETE CASCADE,
      secret_hash     TEXT NOT NULL,
      expires_at      INTEGER NOT NULL,
      used_at         INTEGER,
      used_by         TEXT,
      created_at      TEXT NOT NULL DEFAULT (datetime('now'))
    );
  `);

  migrateSitesSchema(handle);
  migrateAppsSchema(handle);
  migrateUsersSchema(handle);
}

function migrateUsersSchema(handle: Database.Database) {
  const cols = handle.prepare("PRAGMA table_info(users)").all() as ColumnInfo[];
  if (cols.length === 0) return;
  if (!cols.some((c) => c.name === "totp_last_step")) {
    // Last accepted TOTP time step, so a code is accepted only once.
    handle.exec("ALTER TABLE users ADD COLUMN totp_last_step INTEGER NOT NULL DEFAULT 0");
  }
}

function migrateAppsSchema(handle: Database.Database) {
  const cols = handle.prepare("PRAGMA table_info(apps)").all() as ColumnInfo[];
  if (cols.length === 0) return;
  const byName = new Map(cols.map((c) => [c.name, c]));
  if (!byName.has("repo_url")) {
    // Existing installs predate the GitHub-pull flow; add the column so
    // the /apps page can fetch upstream-status per app.
    handle.exec("ALTER TABLE apps ADD COLUMN repo_url TEXT");
  }
  if (!byName.has("folder_id")) {
    // /apps folder-grouping feature. Nullable — apps without a folder
    // render under "Ungrouped". No FK CASCADE because SQLite ALTER TABLE
    // can't add a foreign-key constraint retroactively; the cleanup is
    // handled in deleteAppFolderAction (sets folder_id back to NULL
    // before dropping the folder row).
    handle.exec("ALTER TABLE apps ADD COLUMN folder_id INTEGER");
  }
}

type ColumnInfo = {
  cid: number;
  name: string;
  type: string;
  notnull: number;
  dflt_value: string | null;
  pk: number;
};

function migrateSitesSchema(handle: Database.Database) {
  const cols = handle.prepare("PRAGMA table_info(sites)").all() as ColumnInfo[];
  if (cols.length === 0) return;
  const byName = new Map(cols.map((c) => [c.name, c]));

  if (!byName.has("mode")) {
    handle.exec(
      "ALTER TABLE sites ADD COLUMN mode TEXT NOT NULL DEFAULT 'port'",
    );
  }
  if (!byName.has("folder_path")) {
    handle.exec("ALTER TABLE sites ADD COLUMN folder_path TEXT");
  }
  if (!byName.has("force_www_redirect")) {
    handle.exec(
      "ALTER TABLE sites ADD COLUMN force_www_redirect INTEGER NOT NULL DEFAULT 0",
    );
  }

  const portCol = byName.get("upstream_port");
  if (portCol && portCol.notnull === 1) {
    handle.exec("BEGIN TRANSACTION");
    try {
      handle.exec(`
        CREATE TABLE sites_new (
          domain               TEXT PRIMARY KEY,
          mode                 TEXT NOT NULL DEFAULT 'port' CHECK(mode IN ('port','folder')),
          upstream_port        INTEGER,
          folder_path          TEXT,
          force_www_redirect   INTEGER NOT NULL DEFAULT 0,
          ssl_requested        INTEGER NOT NULL DEFAULT 1,
          ssl_issued_at        TEXT,
          last_ssl_error       TEXT,
          created_at           TEXT NOT NULL DEFAULT (datetime('now'))
        );
        INSERT INTO sites_new (domain, mode, upstream_port, folder_path, force_www_redirect,
                               ssl_requested, ssl_issued_at, last_ssl_error, created_at)
          SELECT domain,
                 COALESCE(mode, 'port'),
                 upstream_port,
                 folder_path,
                 COALESCE(force_www_redirect, 0),
                 ssl_requested,
                 ssl_issued_at,
                 last_ssl_error,
                 created_at
            FROM sites;
        DROP TABLE sites;
        ALTER TABLE sites_new RENAME TO sites;
      `);
      handle.exec("COMMIT");
    } catch (e) {
      handle.exec("ROLLBACK");
      throw e;
    }
  }
}
