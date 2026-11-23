import "server-only";
import { db } from "./db";
import { pruneDissolved } from "./networks/store";
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
 * The audit log and the task history are not touched: how long those are
 * kept is the operator's decision.
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
 * At panel start and once a day after that. Started once per panel process,
 * from src/instrumentation.ts.
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
  };
  run();
  g.__panelHousekeeping = setInterval(run, DAY_MS);
  g.__panelHousekeeping.unref();
}
