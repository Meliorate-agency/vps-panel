import "server-only";
import { db } from "./db";
import { getPanelTimezone, todayInTz } from "./audit";

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

function offsetFmt(tz: string): Intl.DateTimeFormat {
  return new Intl.DateTimeFormat("en-US", {
    timeZone: tz,
    timeZoneName: "shortOffset",
  });
}

/** Resolve the configured tz's UTC offset (in minutes) for a given instant. */
function tzOffsetMinutes(at: Date, tz: string): number {
  const parts = offsetFmt(tz).formatToParts(at);
  const label = parts.find((p) => p.type === "timeZoneName")?.value ?? "GMT+0";
  if (label === "GMT" || label === "UTC") return 0;
  const m = label.match(/GMT([+-])(\d+)(?::(\d+))?/);
  if (!m) return 0;
  const sign = m[1] === "-" ? -1 : 1;
  const hours = Number(m[2]);
  const mins = Number(m[3] ?? 0);
  return sign * (hours * 60 + mins);
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
  // Noon UTC reference so DST boundaries don't put us on the wrong day.
  const reference = new Date(`${day}T12:00:00Z`);
  const offsetMin = tzOffsetMinutes(reference, tz);
  const baseUtcMidnight = new Date(`${day}T00:00:00Z`);
  const startMs = baseUtcMidnight.getTime() - offsetMin * 60_000;
  const endMs = startMs + 24 * 60 * 60 * 1000;
  return {
    startRfc: new Date(startMs).toISOString(),
    endRfc: new Date(endMs).toISOString(),
  };
}

/**
 * Distinct calendar days (in the configured tz) that have at least one
 * task. Newest first. Powers the prev/next pager on /tasks.
 *
 * Implementation: pull distinct UTC dates first (cheap, capped at one row
 * per day), then for each, fetch min+max started_at and convert both to
 * tz-day. That keeps DST-straddling UTC days from dropping a local-tz
 * bucket.
 */
export function listTaskDays(): string[] {
  const tz = getPanelTimezone();
  const utcDays = db()
    .prepare(
      `SELECT DISTINCT substr(started_at, 1, 10) AS day
         FROM tasks
        ORDER BY day DESC`,
    )
    .all() as { day: string }[];
  if (utcDays.length === 0) return [];

  const tzDays = new Set<string>();
  const stmt = db().prepare(
    `SELECT MIN(started_at) AS first_ts, MAX(started_at) AS last_ts
       FROM tasks
      WHERE substr(started_at, 1, 10) = ?`,
  );
  for (const { day } of utcDays) {
    const row = stmt.get(day) as { first_ts: string; last_ts: string };
    tzDays.add(toTzDay(new Date(row.first_ts), tz));
    tzDays.add(toTzDay(new Date(row.last_ts), tz));
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
