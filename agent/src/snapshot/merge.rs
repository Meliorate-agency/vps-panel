//! Folding an imported server's records into this one: panel.db, the
//! PowerDNS and mail rows, and the move of its DNS from the old server's
//! addresses to this server's.

use crate::error::{AgentError, AgentResult};
use mysql_async::prelude::*;
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::path::Path;

// ------------------------------------------------------------- panel.db

/// Settings that belong to this machine, not to the server being moved:
/// kept from this panel whatever the snapshot says.
const KEEP_SETTINGS: &[&str] = &[
    "ipv6.address",
    "ipv6.migrated",
    "panel.setup_complete",
    "panel.setup_step",
    // Applied by the import itself, only when the importing browser stays
    // allowed (or the operator would lock themselves out).
    "panel.access_allowlist",
    "panel.ip_allowlist",
];

/// My Networks: this server's place in its networks, unless it REPLACES the
/// old server (then the old server's identity and networks come over).
const NETWORK_TABLES: &[&str] = &["networks", "network_members", "networks_dissolved", "network_invites"];

fn table_exists(conn: &Connection, schema: &str, table: &str) -> rusqlite::Result<bool> {
    let n: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM {schema}.sqlite_master WHERE type = 'table' AND name = ?1"),
        [table],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

fn columns(conn: &Connection, schema: &str, table: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA {schema}.table_info(\"{table}\")"))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
    rows.collect()
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// `main.table` becomes `src.table`, column by column where both have the
/// column (an older or newer panel on either side). A table the snapshot
/// lacks is left as it is.
fn replace_table(conn: &Connection, table: &str) -> rusqlite::Result<bool> {
    if !table_exists(conn, "main", table)? || !table_exists(conn, "src", table)? {
        return Ok(false);
    }
    let here = columns(conn, "main", table)?;
    let there = columns(conn, "src", table)?;
    let shared: Vec<String> = here.iter().filter(|c| there.contains(c)).map(|c| quote(c)).collect();
    if shared.is_empty() {
        return Ok(false);
    }
    let cols = shared.join(", ");
    conn.execute(&format!("DELETE FROM main.{}", quote(table)), [])?;
    conn.execute(
        &format!("INSERT INTO main.{t} ({cols}) SELECT {cols} FROM src.{t}", t = quote(table)),
        [],
    )?;
    Ok(true)
}

/// What the import needs from the panel database before anything that
/// reads it (DNS reads `dns.ns_pair`): apps, sites, domains, redirects, app
/// folders and comments, backup jobs, and settings without this machine's
/// own. Signs nobody out.
/// The panel tables part A replaces with the snapshot's.
const PART_A_TABLES: [&str; 7] = ["apps", "domains", "sites", "redirects", "app_folders", "app_comments", "backup_jobs"];

/// Discard: the tables part A replaced, and the settings, back as they were
/// just before it (`before_db`, a copy of panel.db taken then). Without it a
/// discarded import left its apps, sites and domains in the panel, which then
/// no longer counted as freshly installed: no snapshot could be imported
/// again.
pub fn restore_panel_part_a(conn: &Connection, before_db: &Path) -> rusqlite::Result<()> {
    with_attached(conn, before_db, |conn| {
        for table in PART_A_TABLES {
            replace_table(conn, table)?;
        }
        if table_exists(conn, "src", "settings")? && table_exists(conn, "main", "settings")? {
            conn.execute("DELETE FROM main.settings", [])?;
            conn.execute("INSERT INTO main.settings (key, value) SELECT key, value FROM src.settings", [])?;
        }
        Ok(())
    })
}

pub fn merge_panel_part_a(conn: &Connection, snapshot_db: &Path) -> rusqlite::Result<Vec<String>> {
    with_attached(conn, snapshot_db, |conn| {
        let mut done = Vec::new();
        for table in PART_A_TABLES {
            if replace_table(conn, table)? {
                done.push(table.to_string());
            }
        }
        if table_exists(conn, "src", "settings")? && table_exists(conn, "main", "settings")? {
            let keep: Vec<String> = KEEP_SETTINGS.iter().map(|k| format!("'{k}'")).collect();
            let keep = keep.join(", ");
            conn.execute(&format!("DELETE FROM main.settings WHERE key NOT IN ({keep}) AND key NOT LIKE 'networks.%'"), [])?;
            conn.execute(
                &format!(
                    "INSERT OR REPLACE INTO main.settings (key, value) SELECT key, value FROM src.settings \
                     WHERE key NOT IN ({keep}) AND key NOT LIKE 'networks.%'"
                ),
                [],
            )?;
            done.push("settings".into());
        }
        Ok(done)
    })
}

/// ts, actor_id, action, target, request_hash, result, message.
type AuditRow = (String, Option<i64>, String, Option<String>, Option<String>, String, Option<String>);

/// The rest, at the very end: the panel's users (their passwords and 2FA),
/// terminal history, the audit log with this panel's own entries kept
/// after it, notification channels, and My Networks when this server
/// replaces the old one. Everyone is signed out.
pub fn merge_panel_part_b(conn: &Connection, snapshot_db: &Path, replaces_old: bool) -> rusqlite::Result<()> {
    with_attached(conn, snapshot_db, |conn| {
        // This panel's audit entries so far (the import's own included),
        // put back after the old server's.
        let ours: Vec<AuditRow> =
            if table_exists(conn, "main", "audit_log")? {
                let mut stmt = conn.prepare(
                    "SELECT ts, actor_id, action, target, request_hash, result, message FROM main.audit_log ORDER BY id",
                )?;
                let rows = stmt.query_map([], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))
                })?;
                rows.collect::<rusqlite::Result<_>>().unwrap_or_default()
            } else {
                Vec::new()
            };
        for table in ["users", "terminal_history", "notifications"] {
            replace_table(conn, table)?;
        }
        if replace_table(conn, "audit_log")? {
            for (ts, _actor, action, target, hash, result, message) in &ours {
                // Their actor ids were this panel's users, who are gone.
                conn.execute(
                    "INSERT INTO main.audit_log (ts, actor_id, action, target, request_hash, result, message) \
                     VALUES (?1, NULL, ?2, ?3, ?4, ?5, ?6)",
                    rusqlite::params![ts, action, target, hash, result, message],
                )?;
            }
        }
        if replaces_old {
            for table in NETWORK_TABLES {
                replace_table(conn, table)?;
            }
            conn.execute(
                "INSERT OR REPLACE INTO main.settings (key, value) SELECT key, value FROM src.settings WHERE key LIKE 'networks.%'",
                [],
            )
            .ok();
        }
        for table in ["sessions", "login_attempts"] {
            if table_exists(conn, "main", table)? {
                conn.execute(&format!("DELETE FROM main.{table}"), [])?;
            }
        }
        conn.execute(
            "INSERT OR REPLACE INTO main.settings (key, value) VALUES ('panel.setup_complete', 'true')",
            [],
        )?;
        Ok(())
    })
}

fn with_attached<T>(
    conn: &Connection,
    snapshot_db: &Path,
    f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
) -> rusqlite::Result<T> {
    conn.execute("ATTACH DATABASE ?1 AS src", [snapshot_db.to_string_lossy()])?;
    // Rows go in table by table; the keys are checked once at the end.
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let out = f(conn);
    let out = match out {
        Ok(v) => conn.execute_batch("COMMIT").map(|_| v),
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    };
    let _ = conn.execute_batch("PRAGMA foreign_keys = ON");
    let _ = conn.execute("DETACH DATABASE src", []);
    out
}

// ---------------------------------------------------------- pdns / mail

/// What the merge of one schema did.
#[derive(Debug, Default)]
pub struct SchemaMerge {
    /// old id -> new id of each parent row taken.
    pub ids: BTreeMap<i64, i64>,
    /// Names taken (zones / mail domains).
    pub taken: Vec<String>,
    /// Names this server has already: the snapshot's rows for them are left.
    pub skipped: Vec<String>,
}

/// Columns of `schema.table`, in order.
async fn mysql_columns(conn: &mut mysql_async::Conn, schema: &str, table: &str) -> AgentResult<Vec<String>> {
    Ok(conn
        .exec(
            "SELECT COLUMN_NAME FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION",
            (schema, table),
        )
        .await?)
}

fn ident(s: &str) -> String {
    format!("`{}`", s.replace('`', "``"))
}

/// Copies the parent rows of `scratch` (loaded from the snapshot) into
/// `live` with new ids, skipping names `live` has, then the child rows of
/// each, pointed at the new ids: `domains` + `records`, `domainmetadata`,
/// `cryptokeys`, `comments` for PowerDNS; `virtual_domains` + users and
/// aliases for mail. One transaction.
pub async fn merge_schema(
    pool: &mysql_async::Pool,
    scratch: &str,
    live: &str,
    parent: &str,
    name_col: &str,
    children: &[&str],
) -> AgentResult<SchemaMerge> {
    let mut conn = pool.get_conn().await?;
    let mut out = SchemaMerge::default();
    let parent_cols: Vec<String> = {
        let here = mysql_columns(&mut conn, live, parent).await?;
        let there = mysql_columns(&mut conn, scratch, parent).await?;
        here.into_iter().filter(|c| c != "id" && there.contains(c)).collect()
    };
    let mut child_cols: Vec<(String, Vec<String>)> = Vec::new();
    for child in children {
        let here = mysql_columns(&mut conn, live, child).await?;
        let there = mysql_columns(&mut conn, scratch, child).await?;
        if here.is_empty() || there.is_empty() {
            continue;
        }
        let cols: Vec<String> = here.into_iter().filter(|c| c != "id" && there.contains(c)).collect();
        child_cols.push((child.to_string(), cols));
    }
    let rows: Vec<(i64, String)> = conn
        .query(format!(
            "SELECT id, {n} FROM {s}.{p} ORDER BY id",
            n = ident(name_col),
            s = ident(scratch),
            p = ident(parent)
        ))
        .await?;
    let existing: Vec<String> = conn
        .query(format!("SELECT {n} FROM {l}.{p}", n = ident(name_col), l = ident(live), p = ident(parent)))
        .await?;
    let mut tx = conn.start_transaction(mysql_async::TxOpts::default()).await?;
    let pcols = parent_cols.iter().map(|c| ident(c)).collect::<Vec<_>>().join(", ");
    for (old_id, name) in rows {
        if existing.iter().any(|e| e.eq_ignore_ascii_case(&name)) {
            out.skipped.push(name);
            continue;
        }
        tx.exec_drop(
            format!(
                "INSERT INTO {l}.{p} ({pcols}) SELECT {pcols} FROM {s}.{p} WHERE id = ?",
                l = ident(live),
                p = ident(parent),
                s = ident(scratch)
            ),
            (old_id,),
        )
        .await?;
        let new_id = tx.last_insert_id().unwrap_or(0) as i64;
        for (child, cols) in &child_cols {
            let list = cols.iter().map(|c| ident(c)).collect::<Vec<_>>().join(", ");
            let select = cols
                .iter()
                .map(|c| if c == "domain_id" { "?".to_string() } else { ident(c) })
                .collect::<Vec<_>>()
                .join(", ");
            tx.exec_drop(
                format!(
                    "INSERT INTO {l}.{c} ({list}) SELECT {select} FROM {s}.{c} WHERE domain_id = ?",
                    l = ident(live),
                    c = ident(child),
                    s = ident(scratch)
                ),
                (new_id, old_id),
            )
            .await?;
        }
        out.ids.insert(old_id, new_id);
        out.taken.push(name);
    }
    tx.commit().await?;
    Ok(out)
}

// ----------------------------------------------------------- DNS moves

/// An SPF record with `ip4:<old>` (or `ip4:<old>/32`) terms naming one of
/// `old`, given `ip4:<new>` instead (once). None when nothing changes.
/// `content` is PowerDNS's TXT content: the text in double quotes.
pub fn spf_with_ip4(content: &str, old: &[String], new: &str) -> Option<String> {
    let inner = content.trim().trim_matches('"');
    if !inner.starts_with("v=spf1") {
        return None;
    }
    let mut changed = false;
    let mut have_new = inner.split_whitespace().any(|t| t == format!("ip4:{new}") || t == format!("ip4:{new}/32"));
    let mut out: Vec<String> = Vec::new();
    for term in inner.split_whitespace() {
        let ip = term.strip_prefix("ip4:").map(|v| v.strip_suffix("/32").unwrap_or(v));
        match ip {
            Some(ip) if old.iter().any(|o| o == ip) => {
                changed = true;
                if !have_new {
                    out.push(format!("ip4:{new}"));
                    have_new = true;
                }
            }
            _ => out.push(term.to_string()),
        }
    }
    changed.then(|| format!("\"{}\"", out.join(" ")))
}

/// An SOA record with its serial moved past both the old serial and now
/// (YYYYMMDDHH), so secondaries and caches take the changed zone.
pub fn bump_soa(content: &str, now: chrono::DateTime<chrono::Utc>) -> Option<String> {
    let mut parts: Vec<String> = content.split_whitespace().map(str::to_string).collect();
    if parts.len() < 3 {
        return None;
    }
    let old: u64 = parts[2].parse().ok()?;
    let stamp: u64 = now.format("%Y%m%d00").to_string().parse().ok()?;
    let stamp = stamp + now.format("%H").to_string().parse::<u64>().ok()?;
    let new = (old + 1).max(stamp);
    if new > u32::MAX as u64 {
        return None;
    }
    parts[2] = new.to_string();
    Some(parts.join(" "))
}

/// Points the imported zones (`domain_ids`) at `new_v4`: A records holding
/// one of `old_v4`, SPF `ip4:` terms, and a newer SOA serial. Returns how
/// many records changed.
pub async fn move_zones_ipv4(
    pool: &mysql_async::Pool,
    domain_ids: &[i64],
    old_v4: &[String],
    new_v4: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> AgentResult<u64> {
    if domain_ids.is_empty() {
        return Ok(0);
    }
    let mut conn = pool.get_conn().await?;
    let mut tx = conn.start_transaction(mysql_async::TxOpts::default()).await?;
    let mut changed = 0u64;
    for id in domain_ids {
        for old in old_v4 {
            if old == new_v4 {
                continue;
            }
            tx.exec_drop(
                "UPDATE pdns.records SET content = ? WHERE domain_id = ? AND type = 'A' AND content = ?",
                (new_v4, id, old),
            )
            .await?;
            changed += tx.affected_rows();
        }
        let txts: Vec<(i64, String)> = tx
            .exec("SELECT id, content FROM pdns.records WHERE domain_id = ? AND type = 'TXT'", (id,))
            .await?;
        for (rid, content) in txts {
            if let Some(new) = spf_with_ip4(&content, old_v4, new_v4) {
                tx.exec_drop("UPDATE pdns.records SET content = ? WHERE id = ?", (new, rid)).await?;
                changed += 1;
            }
        }
        let soas: Vec<(i64, String)> = tx
            .exec("SELECT id, content FROM pdns.records WHERE domain_id = ? AND type = 'SOA'", (id,))
            .await?;
        for (rid, content) in soas {
            if let Some(new) = bump_soa(&content, now) {
                tx.exec_drop("UPDATE pdns.records SET content = ? WHERE id = ?", (new, rid)).await?;
            }
        }
    }
    tx.commit().await?;
    Ok(changed)
}

/// Mail domains pinned to IPv6 on a server that has none here: back to the
/// default, so their mail goes out at all.
pub async fn unpin_ipv6_mail(pool: &mysql_async::Pool) -> AgentResult<u64> {
    let mut conn = pool.get_conn().await?;
    conn.query_drop("UPDATE mail.virtual_domains SET ip_preference = 'default', ip_forced = 0 WHERE ip_preference = 'ipv6'")
        .await
        .map_err(AgentError::MariaDb)?;
    Ok(conn.affected_rows())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spf_terms_move_to_the_new_address() {
        let old = vec!["203.0.113.10".to_string()];
        assert_eq!(
            spf_with_ip4("\"v=spf1 mx ip4:203.0.113.10 ip6:2001:db8::1 -all\"", &old, "203.0.113.20").as_deref(),
            Some("\"v=spf1 mx ip4:203.0.113.20 ip6:2001:db8::1 -all\"")
        );
        assert_eq!(
            spf_with_ip4("\"v=spf1 ip4:203.0.113.10/32 ip4:203.0.113.20 -all\"", &old, "203.0.113.20").as_deref(),
            Some("\"v=spf1 ip4:203.0.113.20 -all\"")
        );
        assert_eq!(spf_with_ip4("\"v=spf1 mx -all\"", &old, "203.0.113.20"), None);
        assert_eq!(spf_with_ip4("\"google-site-verification=x\"", &old, "203.0.113.20"), None);
        assert_eq!(spf_with_ip4("\"v=spf1 ip4:203.0.113.100 -all\"", &old, "203.0.113.20"), None);
    }

    #[test]
    fn soa_serial_moves_forward() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-27T13:05:09Z").unwrap().with_timezone(&chrono::Utc);
        assert_eq!(
            bump_soa("ns1.example.com. hostmaster.example.com. 2026010101 10800 3600 604800 3600", now).as_deref(),
            Some("ns1.example.com. hostmaster.example.com. 2026112713 10800 3600 604800 3600")
        );
        assert_eq!(
            bump_soa("ns1.example.com. h.example.com. 2026112799 10800 3600 604800 3600", now).as_deref(),
            Some("ns1.example.com. h.example.com. 2026112800 10800 3600 604800 3600")
        );
        assert_eq!(bump_soa("garbage", now), None);
    }

    fn panel_db(path: &Path) -> Connection {
        let c = Connection::open(path).unwrap();
        c.execute_batch(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT, password_hash TEXT, role TEXT);
             CREATE TABLE sessions (jti TEXT PRIMARY KEY, user_id INTEGER, expires_at TEXT, revoked INTEGER);
             CREATE TABLE login_attempts (id INTEGER PRIMARY KEY, ip TEXT);
             CREATE TABLE apps (name TEXT PRIMARY KEY, port INTEGER);
             CREATE TABLE sites (domain TEXT PRIMARY KEY, upstream_port INTEGER);
             CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE audit_log (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL, actor_id INTEGER,
               action TEXT NOT NULL, target TEXT, request_hash TEXT, result TEXT NOT NULL, message TEXT);
             CREATE TABLE tasks (id TEXT PRIMARY KEY, kind TEXT);
             CREATE TABLE networks (id TEXT PRIMARY KEY, name TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn panel_db_merge_takes_the_server_and_keeps_this_machine() {
        let dir = tempfile::tempdir().unwrap();
        let src_path = dir.path().join("src.db");
        let src = panel_db(&src_path);
        src.execute_batch(
            "INSERT INTO users VALUES (7, 'old@example.com', 'hash-a', 'admin');
             INSERT INTO apps VALUES ('shop', 3000);
             INSERT INTO sites VALUES ('shop.example.com', 3000);
             INSERT INTO settings VALUES ('dns.ns_pair', 'ns1.example.com,ns2.example.com'),
                                         ('ipv6.address', '2001:db8::a'), ('panel.theme', 'dragon'),
                                         ('networks.node_name', 'old');
             INSERT INTO audit_log (ts, actor_id, action, result) VALUES ('2026-01-01', 7, 'apps.create', 'ok');
             INSERT INTO networks VALUES ('n1', 'net of old');",
        )
        .unwrap();
        // The old server has a column this panel does not.
        src.execute_batch("ALTER TABLE apps ADD COLUMN future TEXT").unwrap();
        drop(src);

        let main = panel_db(&dir.path().join("main.db"));
        main.execute_batch(
            "INSERT INTO users VALUES (1, 'new@example.com', 'hash-b', 'admin');
             INSERT INTO sessions VALUES ('j', 1, 'x', 0);
             INSERT INTO settings VALUES ('ipv6.address', '2001:db8::b'), ('panel.setup_complete', 'true'),
                                         ('networks.node_name', 'new');
             INSERT INTO audit_log (ts, actor_id, action, result) VALUES ('2026-11-27', 1, 'snapshots.import', 'ok');
             INSERT INTO tasks VALUES ('t1', 'snapshots.import');",
        )
        .unwrap();

        merge_panel_part_a(&main, &src_path).unwrap();
        let apps: i64 = main.query_row("SELECT COUNT(*) FROM apps WHERE name = 'shop'", [], |r| r.get(0)).unwrap();
        assert_eq!(apps, 1);
        let get = |k: &str| -> Option<String> {
            main.query_row("SELECT value FROM settings WHERE key = ?1", [k], |r| r.get(0)).ok()
        };
        assert_eq!(get("dns.ns_pair").as_deref(), Some("ns1.example.com,ns2.example.com"));
        assert_eq!(get("panel.theme").as_deref(), Some("dragon"));
        assert_eq!(get("ipv6.address").as_deref(), Some("2001:db8::b"), "this machine's address stays");
        assert_eq!(get("networks.node_name").as_deref(), Some("new"));
        // Part A signs nobody out.
        let sessions: i64 = main.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0)).unwrap();
        assert_eq!(sessions, 1);

        merge_panel_part_b(&main, &src_path, false).unwrap();
        let email: String = main.query_row("SELECT email FROM users WHERE id = 7", [], |r| r.get(0)).unwrap();
        assert_eq!(email, "old@example.com");
        let users: i64 = main.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0)).unwrap();
        assert_eq!(users, 1);
        let sessions: i64 = main.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0)).unwrap();
        assert_eq!(sessions, 0, "everyone signs in again");
        let actions: Vec<(String, Option<i64>)> = main
            .prepare("SELECT action, actor_id FROM audit_log ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            actions,
            vec![("apps.create".to_string(), Some(7)), ("snapshots.import".to_string(), None)]
        );
        let tasks: i64 = main.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap();
        assert_eq!(tasks, 1, "this panel's tasks stay");
        let nets: i64 = main.query_row("SELECT COUNT(*) FROM networks", [], |r| r.get(0)).unwrap();
        assert_eq!(nets, 0, "not replacing the old server: its networks stay with it");

        merge_panel_part_b(&main, &src_path, true).unwrap();
        let nets: i64 = main.query_row("SELECT COUNT(*) FROM networks", [], |r| r.get(0)).unwrap();
        assert_eq!(nets, 1);
        assert_eq!(get("networks.node_name").as_deref(), Some("old"));
    }
}
