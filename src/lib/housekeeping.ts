import "server-only";
import { db } from "./db";

const DAY_MS = 24 * 60 * 60 * 1000;

/**
 * Deletes rows nothing reads any more. Both tables only ever grew:
 *
 * - `sessions`: a sign-in and every keep-alive refresh add two rows. Once
 *   `expires_at` has passed, the token itself is refused before its row is
 *   looked up. A day's margin on top.
 * - `login_attempts`: the limiter counts the last 15 minutes; a week is kept.
 *
 * The audit log and the task history are not touched: how long those are
 * kept is the operator's decision.
 */
export function pruneAuthTables(): void {
  const conn = db();
  conn.prepare(`DELETE FROM sessions WHERE expires_at < datetime('now', '-1 day')`).run();
  conn.prepare(`DELETE FROM login_attempts WHERE ts < datetime('now', '-7 days')`).run();
}

/**
 * At panel start and once a day after that. Started once per panel process,
 * from src/instrumentation.ts.
 */
export function startHousekeeping(): void {
  const g = globalThis as { __panelHousekeeping?: ReturnType<typeof setInterval> };
  if (g.__panelHousekeeping) return;
  const run = () => {
    try {
      pruneAuthTables();
    } catch (e) {
      console.error("[housekeeping] could not prune sessions / login attempts:", e);
    }
  };
  run();
  g.__panelHousekeeping = setInterval(run, DAY_MS);
  g.__panelHousekeeping.unref();
}
