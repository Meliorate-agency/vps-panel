import "server-only";
import { db } from "./db";
import { pruneDissolved } from "./networks/store";
import { retentionDays } from "./privacy";
import { KNOWN_ADDRESS_DAYS } from "./rateLimit";

const DAY_MS = 24 * 60 * 60 * 1000;

/**
 * Deletes rows nothing reads any more. Both tables only ever grew:
 *
 * - `sessions`: a sign-in and every keep-alive refresh add two rows. Once
 *   `expires_at` has passed, the token itself is refused before its row is
 *   looked up. A day's margin on top.
 * - `login_attempts`: the limiter counts the failures of the last 15
 *   minutes; a week of them is kept. Successful sign-ins are kept for
 *   KNOWN_ADDRESS_DAYS: they tell the limiter which addresses are the
 *   account's own (lib/rateLimit.ts).
 *
 * The audit log is not touched here: how long it is kept is the operator's
 * decision (pruneByRetention). The task history is the agent's, which
 * deletes finished tasks after 30 days.
 */
export function pruneAuthTables(): void {
  const conn = db();
  conn.prepare(`DELETE FROM sessions WHERE expires_at < datetime('now', '-1 day')`).run();
  conn.prepare(`DELETE FROM login_attempts WHERE success = 0 AND ts < datetime('now', '-7 days')`).run();
  conn
    .prepare(`DELETE FROM login_attempts WHERE ts < datetime('now', '-' || ? || ' days')`)
    .run(KNOWN_ADDRESS_DAYS);
}

/**
 * "Keep logs for N days" (Settings > Privacy) for what panel.db holds: with
 * a limit set, audit log entries, sign-in records and finished tasks older
 * than it are deleted. Without a limit nothing is. The shorter limits above
 * stay; a limit under KNOWN_ADDRESS_DAYS also shortens how long an address
 * counts as the account's own for the sign-in limiter.
 *
 * The `tasks` rows are the agent's, which deletes finished ones itself after
 * 30 days (agent/src/ipc/tasks.rs, prune_old). This is the same delete with
 * an earlier date, for a limit under 30 days: finished rows only, which the
 * agent no longer writes to and only reads by id; a task still running is in
 * its memory and keeps its row. Both sides open panel.db in WAL mode with a
 * busy timeout. `started_at` is RFC 3339 and sorts as text (lib/tasksDb.ts).
 *
 * Returns how many rows went, or null without a limit.
 */
export function pruneByRetention(): { audit: number; logins: number; tasks: number } | null {
  const days = retentionDays();
  if (days === null) return null;
  const conn = db();
  const older = `datetime('now', '-' || ? || ' days')`;
  return {
    audit: conn.prepare(`DELETE FROM audit_log WHERE ts < ${older}`).run(days).changes,
    logins: conn.prepare(`DELETE FROM login_attempts WHERE ts < ${older}`).run(days).changes,
    tasks: conn
      .prepare(`DELETE FROM tasks WHERE status NOT IN ('pending', 'running') AND started_at < ?`)
      .run(new Date(Date.now() - days * DAY_MS).toISOString()).changes,
  };
}

/**
 * At panel start and once a day after that. Started once per panel process,
 * from src/instrumentation.ts. Saving the retention setting runs
 * pruneByRetention right away as well (server-actions/privacy.ts).
 */
export function startHousekeeping(): void {
  const g = globalThis as { __panelHousekeeping?: ReturnType<typeof setInterval> };
  if (g.__panelHousekeeping) return;
  const run = () => {
    try {
      pruneAuthTables();
      // The final member lists of networks deleted here, after 90 days.
      pruneDissolved();
    } catch (e) {
      console.error("[housekeeping] could not prune sessions / login attempts:", e);
    }
    try {
      pruneByRetention();
    } catch (e) {
      console.error("[housekeeping] could not apply the log retention:", e);
    }
  };
  run();
  g.__panelHousekeeping = setInterval(run, DAY_MS);
  g.__panelHousekeeping.unref();
}
