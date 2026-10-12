import "server-only";
import { agent, type CreateApp } from "./agent";
import { db } from "./db";

/**
 * panel.db bookkeeping for app deploys, shared by the /apps deploy form and
 * the MCP `apps.create` tool. MCP deploys used to write no row at all, so
 * their slot looked free to the pickers and `apps.set_domain` refused them.
 */

/** The runtime panel.db recorded for an app, or null for an unknown app. */
export function appRuntime(name: string): string | null {
  const row = db().prepare(`SELECT runtime FROM apps WHERE name = ?`).get(name) as
    | { runtime: string }
    | undefined;
  return row?.runtime ?? null;
}

/** Why `port` / `subdomain` cannot go to `name`: another app already has it. */
export function slotConflict(name: string, port: number, subdomain: string): string | null {
  const row = db()
    .prepare(
      `SELECT name, port, subdomain FROM apps
        WHERE name != ?
          AND ((port = ? AND port != 0) OR (subdomain = ? AND subdomain != ''))
        LIMIT 1`,
    )
    .get(name, port, subdomain) as { name: string; port: number; subdomain: string } | undefined;
  if (!row) return null;
  return row.port === port
    ? `port ${port} is already used by app ${row.name}`
    : `${subdomain} is already used by app ${row.name}`;
}

/**
 * Record a deploy the agent accepted. A brand-new app whose deploy then fails
 * is removed again, so /apps never lists an app that never ran. Watching the
 * task happens in the background; if the panel restarts meanwhile the row
 * simply stays, as before.
 */
export function recordAppDeploy(req: CreateApp, taskId: string): void {
  const conn = db();
  const existed = !!conn.prepare(`SELECT 1 FROM apps WHERE name = ?`).get(req.name);
  conn
    .prepare(
      `INSERT INTO apps (name, source_dir, port, subdomain, runtime, entrypoint, ssl_enabled, repo_url)
       VALUES (?, ?, ?, ?, ?, COALESCE(?, '/usr/bin/npm start'), ?, ?)
       ON CONFLICT(name) DO UPDATE SET
         source_dir=excluded.source_dir, port=excluded.port,
         subdomain=excluded.subdomain, runtime=excluded.runtime,
         entrypoint=COALESCE(?, apps.entrypoint), ssl_enabled=excluded.ssl_enabled,
         repo_url=COALESCE(excluded.repo_url, apps.repo_url)`,
    )
    .run(
      req.name,
      req.source_dir,
      req.port,
      req.subdomain,
      req.runtime,
      req.exec_start ?? null,
      req.issue_ssl ? 1 : 0,
      req.repo_url ?? null,
      req.exec_start ?? null,
    );
  if (!existed) void forgetIfDeployFails(req.name, taskId);
}

async function forgetIfDeployFails(name: string, taskId: string): Promise<void> {
  const deadline = Date.now() + 60 * 60 * 1000;
  while (Date.now() < deadline) {
    await new Promise((r) => setTimeout(r, 3000));
    let status: string;
    try {
      status = (await agent.tasks.get(taskId)).status;
    } catch {
      continue;
    }
    if (status === "succeeded") return;
    if (status === "failed") {
      db().prepare(`DELETE FROM apps WHERE name = ?`).run(name);
      return;
    }
  }
}
