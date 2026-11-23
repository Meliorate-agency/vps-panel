import "server-only";
import { agent, type CreateApp } from "./agent";
import { db } from "./db";

/**
 * panel.db bookkeeping for app deploys, shared by the /apps deploy form and
 * the MCP `apps.create` tool. MCP deploys used to write no row at all, so
 * their slot looked free to the pickers and `apps.set_domain` refused them.
 */

/** What panel.db recorded for an app (runtime, source folder), or null. */
export function recordedApp(name: string): { runtime: string; source_dir: string } | null {
  const row = db().prepare(`SELECT runtime, source_dir FROM apps WHERE name = ?`).get(name) as
    | { runtime: string; source_dir: string }
    | undefined;
  return row ?? null;
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
 * Apps that have never come up: name -> the tasks of their deploys so far,
 * oldest first. Only the newest of them may take the row away again. On
 * globalThis: the deploy form and the MCP route can be separate module
 * instances.
 */
function unprovenApps(): Map<string, string[]> {
  const g = globalThis as { __appsUnproven?: Map<string, string[]> };
  return (g.__appsUnproven ??= new Map());
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
  // A row whose first deploy is not settled yet is still a brand-new app.
  const earlier = unprovenApps().get(req.name);
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
  if (!existed || earlier) {
    unprovenApps().set(req.name, [...(earlier ?? []), taskId]);
    void forgetIfDeployFails(req.name, taskId);
  }
}

/** How the task ended, or null when it was still running after an hour. */
async function taskOutcome(taskId: string): Promise<"succeeded" | "failed" | null> {
  const deadline = Date.now() + 60 * 60 * 1000;
  while (Date.now() < deadline) {
    await new Promise((r) => setTimeout(r, 3000));
    let status: string;
    try {
      status = (await agent.tasks.get(taskId)).status;
    } catch {
      continue;
    }
    if (status === "succeeded" || status === "failed") return status;
  }
  return null;
}

/**
 * Removes the row of a brand-new app when the deploy `taskId` fails, unless
 * the row is no longer this deploy's. A second deploy started before the
 * failure was noticed here (the task is looked at every 3 s) reuses the row,
 * and the first deploy's failure used to delete it under the second one.
 */
async function forgetIfDeployFails(name: string, taskId: string): Promise<void> {
  const outcome = await taskOutcome(taskId);
  const unproven = unprovenApps();
  const isNewest = () => {
    const tasks = unproven.get(name);
    return !!tasks && tasks[tasks.length - 1] === taskId;
  };
  if (!unproven.has(name)) return;
  if (outcome !== "failed") {
    // The app came up (or nobody can tell): the row stays from here on.
    unproven.delete(name);
    return;
  }
  // A later deploy has the row now; what happens to it is that one's call.
  if (!isNewest()) return;
  // The agent runs one deploy of an app at a time, so the earlier ones are
  // over. If one of them came up, the app has run and keeps its row.
  for (const earlier of (unproven.get(name) ?? []).slice(0, -1)) {
    const status = await agent.tasks
      .get(earlier)
      .then((t) => t.status)
      .catch(() => null);
    if (status !== "failed") {
      unproven.delete(name);
      return;
    }
  }
  // Looked at again: a deploy accepted during those lookups has the row.
  if (!isNewest()) return;
  db().prepare(`DELETE FROM apps WHERE name = ?`).run(name);
  unproven.delete(name);
}
