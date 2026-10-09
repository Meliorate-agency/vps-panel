import "server-only";
import { db } from "./db";
import { getSetting } from "./settings";
import { createHash } from "node:crypto";

export function writeAudit(args: {
  actorId: number | null;
  action: string;
  target?: string | null;
  request?: unknown;
  result: "ok" | "error";
  message?: string;
}) {
  const requestHash =
    args.request === undefined
      ? null
      : createHash("sha256")
          .update(JSON.stringify(args.request))
          .digest("hex")
          .slice(0, 32);
  db()
    .prepare(
      `INSERT INTO audit_log (actor_id, action, target, request_hash, result, message)
       VALUES (?, ?, ?, ?, ?, ?)`,
    )
    .run(
      args.actorId,
      args.action,
      args.target ?? null,
      requestHash,
      args.result,
      args.message ?? null,
    );
}

export type AuditRow = {
  id: number;
  ts: string;
  actor_id: number | null;
  /** Joined from users(email). Null when actor_id is null (system action) or
   *  when the user row was since deleted. */
  actor_email: string | null;
  action: string;
  target: string | null;
  request_hash: string | null;
  result: string;
  message: string | null;
};

// Centralised SELECT so listAudit + listAuditForDay stay in lockstep. The
// LEFT JOIN keeps rows whose actor_id no longer exists (or is null for
// system actions) but pulls in the email when available.
const AUDIT_SELECT = `
  SELECT a.id, a.ts, a.actor_id, u.email AS actor_email,
         a.action, a.target, a.request_hash, a.result, a.message
    FROM audit_log a
    LEFT JOIN users u ON u.id = a.actor_id
`;

export function listAudit(limit = 200): AuditRow[] {
  return db()
    .prepare(`${AUDIT_SELECT} ORDER BY a.id DESC LIMIT ?`)
    .all(limit) as AuditRow[];
}

// ─── Timezone handling ────────────────────────────────────────────────────
//
// `audit_log.ts` is stored in UTC via SQLite's `datetime('now')`. The UI
// renders everything in Europe/Berlin (CEST in summer, CET in winter; DST
// boundaries handled by Intl). Day-grouping has to happen in the operator's
// timezone — a 22:30 UTC event in summer is already 00:30 the next day in
// Berlin and grouping by UTC date would put it in the wrong bucket.

// The display/grouping timezone is operator-configurable via Settings →
// Panel timezone. Default UTC so a fresh install with no setting still
// renders cleanly. Read at call-time (not module-load) so a settings save
// takes effect on the next request without restarting the panel.
export function getPanelTimezone(): string {
  const tz = getSetting("panel.timezone", "UTC");
  try {
    new Intl.DateTimeFormat("en", { timeZone: tz });
    return tz;
  } catch {
    // Setting got corrupted somehow — fall back to UTC and keep rendering.
    return "UTC";
  }
}

function dayFmt(tz: string): Intl.DateTimeFormat {
  return new Intl.DateTimeFormat("en-CA", {
    timeZone: tz,
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
  });
}

function offsetFmt(tz: string): Intl.DateTimeFormat {
  return new Intl.DateTimeFormat("en-US", {
    timeZone: tz,
    timeZoneName: "shortOffset",
  });
}

/** "YYYY-MM-DD HH:MM:SS" UTC string → Date. */
function parseUtcTimestamp(ts: string): Date {
  return new Date(ts.replace(" ", "T") + "Z");
}

/** Date → "YYYY-MM-DD" in the configured tz. */
function toTzDay(d: Date, tz: string): string {
  return dayFmt(tz).format(d);
}

/** Today in the configured tz as "YYYY-MM-DD". */
export function todayInTz(): string {
  return toTzDay(new Date(), getPanelTimezone());
}

/** Resolve the configured tz's UTC offset (in minutes) for a given instant. */
function tzOffsetMinutes(at: Date, tz: string): number {
  const parts = offsetFmt(tz).formatToParts(at);
  const label = parts.find((p) => p.type === "timeZoneName")?.value ?? "GMT+0";
  // Formats observed: "GMT+2", "GMT+02:00", "GMT-5", "GMT" (for UTC).
  if (label === "GMT" || label === "UTC") return 0;
  const m = label.match(/GMT([+-])(\d+)(?::(\d+))?/);
  if (!m) return 0;
  const sign = m[1] === "-" ? -1 : 1;
  const hours = Number(m[2]);
  const mins = Number(m[3] ?? 0);
  return sign * (hours * 60 + mins);
}

/** Format a JS Date as "YYYY-MM-DD HH:MM:SS" in UTC for SQLite comparison. */
function toSqlUtc(d: Date): string {
  const p = (n: number) => String(n).padStart(2, "0");
  return (
    `${d.getUTCFullYear()}-${p(d.getUTCMonth() + 1)}-${p(d.getUTCDate())} ` +
    `${p(d.getUTCHours())}:${p(d.getUTCMinutes())}:${p(d.getUTCSeconds())}`
  );
}

/**
 * Compute the UTC range [start, end) that corresponds to a single calendar
 * day in the configured tz. Used to query audit rows for "today in the
 * operator's tz" rather than "today in UTC", which can differ by 24h around
 * local midnight.
 */
function tzDayToUtcRange(day: string, tz: string): { startUtc: string; endUtc: string } {
  // Take noon UTC on `day` purely to detect the offset — using midnight UTC
  // can land us on the wrong side of a DST boundary in spring.
  const reference = new Date(`${day}T12:00:00Z`);
  const offsetMin = tzOffsetMinutes(reference, tz);
  // Local midnight → UTC instant = midnight-UTC minus the offset.
  const baseUtcMidnight = new Date(`${day}T00:00:00Z`);
  const startMs = baseUtcMidnight.getTime() - offsetMin * 60_000;
  const endMs = startMs + 24 * 60 * 60 * 1000;
  return {
    startUtc: toSqlUtc(new Date(startMs)),
    endUtc: toSqlUtc(new Date(endMs)),
  };
}

/**
 * Distinct calendar days in the configured tz that have at least one audit
 * entry, newest first. Powers the audit page's "prev/next day with entries"
 * pager.
 *
 * Implementation: SQL DISTINCTs by UTC date first to cap the rows we walk,
 * then JS re-groups by local-tz date so a single UTC date that straddles
 * two local dates produces both buckets correctly.
 */
export function listAuditDays(): string[] {
  const tz = getPanelTimezone();
  const utcDays = db()
    .prepare(
      `SELECT DISTINCT substr(ts, 1, 10) AS day
         FROM audit_log
        ORDER BY day DESC`,
    )
    .all() as { day: string }[];
  if (utcDays.length === 0) return [];

  // For each UTC day, pull min+max ts so DST straddles don't lose a local-tz
  // day. Cheap: 2 rows per UTC day.
  const tzDays = new Set<string>();
  const stmt = db().prepare(
    `SELECT MIN(ts) AS first_ts, MAX(ts) AS last_ts
       FROM audit_log
      WHERE substr(ts, 1, 10) = ?`,
  );
  for (const { day } of utcDays) {
    const row = stmt.get(day) as { first_ts: string; last_ts: string };
    tzDays.add(toTzDay(parseUtcTimestamp(row.first_ts), tz));
    tzDays.add(toTzDay(parseUtcTimestamp(row.last_ts), tz));
  }
  return [...tzDays].sort().reverse();
}

export function listAuditForDay(day: string, limit = 2000): AuditRow[] {
  const { startUtc, endUtc } = tzDayToUtcRange(day, getPanelTimezone());
  return db()
    .prepare(
      `${AUDIT_SELECT}
        WHERE a.ts >= ? AND a.ts < ?
        ORDER BY a.id DESC
        LIMIT ?`,
    )
    .all(startUtc, endUtc, limit) as AuditRow[];
}

/** "YYYY-MM-DD HH:MM:SS" UTC → "DD/MM/YYYY HH:MM:SS" in the configured tz. */
export function formatAuditTs(utcTs: string): string {
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
  return fmt.format(parseUtcTimestamp(utcTs));
}
