import "server-only";
import { db } from "./db";
import { getPanelTimezone, todayInTz, tzMidnightUtcMs } from "./audit";

// ─── Why this file lives on the panel side ──────────────────────────────
//
// The agent writes rows to the `tasks` table in /var/lib/vps-panel/panel.db.
// The panel mounts the same SQLite file, so we can read it directly via
// `db()` instead of going through agent.tasks.list() over the unix socket.
// That mirrors how /audit is implemented (audit.ts also reads its own
// table direct from panel.db). Day-grouping in particular wants a few SQL
// queries the agent's list-200-newest endpoint doesn't support, and
// duplicating those over IPC is more code than just hitting the DB.
//
// Timestamps: the agent writes `started_at` / `finished_at` as RFC3339
// (e.g. "2026-05-28T10:02:31.118809825Z"). Audit's "YYYY-MM-DD HH:MM:SS"
// format and RFC3339 both sort lexicographically, so the same range-
// comparison strategy works — we just emit the boundaries as RFC3339
// here instead of " "-separated.

export type TaskRow = {
  id: string;
  kind: string;
  status: string;
  started_at: string;
  finished_at: string | null;
};

const DAY_FMT_CACHE = new Map<string, Intl.DateTimeFormat>();
function dayFmt(tz: string): Intl.DateTimeFormat {
  let f = DAY_FMT_CACHE.get(tz);
  if (!f) {
    f = new Intl.DateTimeFormat("en-CA", {
      timeZone: tz,
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
    });
    DAY_FMT_CACHE.set(tz, f);
  }
  return f;
}

/** Date → "YYYY-MM-DD" in the configured tz. */
function toTzDay(d: Date, tz: string): string {
  return dayFmt(tz).format(d);
}

/**
 * Compute the RFC3339 range [start, end) corresponding to one calendar day
 * in the configured tz. Used to pull task rows for a specific local day.
 */
function tzDayToRfc3339Range(
  day: string,
  tz: string,
): { startRfc: string; endRfc: string } {
  // From this local midnight to the next: the day the clocks change is 23
  // or 25 hours long.
  const nextDay = new Date(Date.parse(`${day}T00:00:00Z`) + 86_400_000).toISOString().slice(0, 10);
  return {
    startRfc: new Date(tzMidnightUtcMs(day, tz)).toISOString(),
    endRfc: new Date(tzMidnightUtcMs(nextDay, tz)).toISOString(),
  };
}

/**
 * Distinct calendar days (in the configured tz) that have at least one
 * task. Newest first. Powers the prev/next pager on /tasks.
 *
 * Implementation: for every UTC date with tasks, its first and last
 * started_at, both converted to tz-day. That keeps DST-straddling UTC days
 * from dropping a local-tz bucket.
 *
 * The UTC dates are walked on the index on `started_at`
 * (idx_tasks_started_at), newest first, one index lookup per step, the same
 * way listAuditDays does it. Grouping by substr(started_at, 1, 10) read the
 * whole table, build logs included, once per date on each visit to /tasks.
 */
export function listTaskDays(): string[] {
  const tz = getPanelTimezone();
  const firstFrom = db().prepare(`SELECT MIN(started_at) AS ts FROM tasks WHERE started_at >= ?`);
  const lastBefore = db().prepare(`SELECT MAX(started_at) AS ts FROM tasks WHERE started_at < ?`);
  const tzDays = new Set<string>();
  let last = (db().prepare(`SELECT MAX(started_at) AS ts FROM tasks`).get() as { ts: string | null }).ts;
  while (last) {
    const utcDay = last.slice(0, 10);
    const first = (firstFrom.get(utcDay) as { ts: string }).ts;
    tzDays.add(toTzDay(new Date(first), tz));
    tzDays.add(toTzDay(new Date(last), tz));
    last = (lastBefore.get(utcDay) as { ts: string | null }).ts;
  }
  return [...tzDays].sort().reverse();
}

/** Tasks started within the given local-tz calendar day. Newest first. */
export function listTasksForDay(day: string, limit = 2000): TaskRow[] {
  const { startRfc, endRfc } = tzDayToRfc3339Range(day, getPanelTimezone());
  return db()
    .prepare(
      `SELECT id, kind, status, started_at, finished_at
         FROM tasks
        WHERE started_at >= ? AND started_at < ?
        ORDER BY started_at DESC
        LIMIT ?`,
    )
    .all(startRfc, endRfc, limit) as TaskRow[];
}

/** Today's tz-day as "YYYY-MM-DD". Re-exported convenience. */
export function todayForTasks(): string {
  return todayInTz();
}

/** RFC3339 UTC → "DD/MM/YYYY HH:MM:SS" in the configured tz. */
export function formatTaskTs(rfcTs: string | null): string {
  if (!rfcTs) return "-";
  const d = new Date(rfcTs);
  if (Number.isNaN(d.getTime())) return rfcTs;
  const fmt = new Intl.DateTimeFormat("en-GB", {
    timeZone: getPanelTimezone(),
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  });
  return fmt.format(d);
}
