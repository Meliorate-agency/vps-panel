import "server-only";
import { createHmac } from "node:crypto";
import { isIP } from "node:net";
import {
  agent,
  AgentError,
  type PrivacyStatus,
  type SubjectEraseItem,
  type SubjectKind,
  type SubjectSource,
} from "./agent";
import { writeAudit } from "./audit";
import { config } from "./config";
import { db } from "./db";
import { agentErrorMessage } from "./redirects";
import { getSetting } from "./settings";

/**
 * Settings > Privacy, the part both the server actions and the MCP tools
 * use: the four settings, what the agent reports about them, and the
 * personal data request (what the server holds about one email address or IP
 * address, and erasing it).
 *
 * The settings are rows in panel.db. The agent reads the first three itself
 * (also once when it starts); `privacy.server_geo` is the panel's alone.
 * Every default is the behaviour from before these settings existed.
 */

// ─── The settings ────────────────────────────────────────────────────────

export const PRIVACY_LOG_IP_KEY = "privacy.log_ip";
export const PRIVACY_LOG_QUERY_KEY = "privacy.log_query";
export const PRIVACY_RETENTION_KEY = "privacy.retention_days";
export const PRIVACY_SERVER_GEO_KEY = "privacy.server_geo";

export const RETENTION_MIN_DAYS = 7;
export const RETENTION_MAX_DAYS = 3650;

export type PrivacySettings = {
  log_ip: "full" | "masked";
  log_query: "keep" | "strip";
  /** null = no limit. */
  retention_days: number | null;
  server_geo: "on" | "off";
};

/**
 * "Keep logs for N days", or null for no limit. Anything that is not a whole
 * number from 7 to 3650 counts as no limit: a value nobody could have saved
 * here must never delete data.
 */
export function retentionDays(): number | null {
  const raw = getSetting(PRIVACY_RETENTION_KEY, "").trim();
  if (!/^\d{1,4}$/.test(raw)) return null;
  const n = Number(raw);
  return n >= RETENTION_MIN_DAYS && n <= RETENTION_MAX_DAYS ? n : null;
}

/** False only when the operator switched the server-location lookups off. */
export function serverGeoEnabled(): boolean {
  return getSetting(PRIVACY_SERVER_GEO_KEY, "on") !== "off";
}

export function privacySettings(): PrivacySettings {
  return {
    log_ip: getSetting(PRIVACY_LOG_IP_KEY, "full") === "masked" ? "masked" : "full",
    log_query: getSetting(PRIVACY_LOG_QUERY_KEY, "keep") === "strip" ? "strip" : "keep",
    retention_days: retentionDays(),
    server_geo: serverGeoEnabled() ? "on" : "off",
  };
}

/** A retention as it comes from a form or a tool: "", null = no limit. */
export function parseRetention(v: unknown): { days: number | null } | { error: string } {
  if (v === null || v === undefined || v === "") return { days: null };
  const n = typeof v === "number" ? v : typeof v === "string" && /^\d{1,4}$/.test(v.trim()) ? Number(v.trim()) : NaN;
  if (!Number.isInteger(n) || n < RETENTION_MIN_DAYS || n > RETENTION_MAX_DAYS) {
    return {
      error: `Choose a whole number of days from ${RETENTION_MIN_DAYS} to ${RETENTION_MAX_DAYS}, or no limit.`,
    };
  }
  return { days: n };
}

/** Saving `next` deletes data that `current` still keeps. */
export function retentionIsLower(current: number | null, next: number | null): boolean {
  return next !== null && (current === null || next < current);
}

/** Only the keys that are present are checked and written. */
export type PrivacyChange = {
  log_ip?: unknown;
  log_query?: unknown;
  retention_days?: unknown;
  server_geo?: unknown;
};

/**
 * Checks every given value against what the setting allows, then writes them
 * together: nothing is stored when one of them is refused. Each written key
 * gets its line in the audit log.
 */
export function savePrivacySettings(
  change: PrivacyChange,
  actorId: number | null,
): { ok: true; settings: PrivacySettings } | { ok: false; error: string } {
  const rows: [string, string][] = [];
  if (change.log_ip !== undefined) {
    if (change.log_ip !== "full" && change.log_ip !== "masked") {
      return { ok: false, error: "Visitor addresses: choose full or masked." };
    }
    rows.push([PRIVACY_LOG_IP_KEY, change.log_ip]);
  }
  if (change.log_query !== undefined) {
    if (change.log_query !== "keep" && change.log_query !== "strip") {
      return { ok: false, error: "Query strings: choose kept or left out." };
    }
    rows.push([PRIVACY_LOG_QUERY_KEY, change.log_query]);
  }
  if (change.retention_days !== undefined) {
    const r = parseRetention(change.retention_days);
    if ("error" in r) return { ok: false, error: r.error };
    rows.push([PRIVACY_RETENTION_KEY, r.days === null ? "" : String(r.days)]);
  }
  if (change.server_geo !== undefined) {
    if (change.server_geo !== "on" && change.server_geo !== "off") {
      return { ok: false, error: "Server locations: choose on or off." };
    }
    rows.push([PRIVACY_SERVER_GEO_KEY, change.server_geo]);
  }
  if (rows.length === 0) return { ok: false, error: "Nothing to save." };

  const conn = db();
  const write = conn.prepare(
    `INSERT INTO settings (key, value) VALUES (?, ?)
     ON CONFLICT(key) DO UPDATE SET value = excluded.value`,
  );
  conn.transaction(() => {
    for (const [key, value] of rows) write.run(key, value);
  })();
  for (const [key, value] of rows) {
    writeAudit({
      actorId,
      action: "settings.set",
      target: key,
      result: "ok",
      message: value || "(no limit)",
    });
  }
  return { ok: true, settings: privacySettings() };
}

// ─── What the agent reports ──────────────────────────────────────────────

/**
 * The agent's message without its "invalid input: " style prefix. An agent
 * older than the privacy routes answers them with a bare 404.
 */
export function privacyAgentError(e: unknown): string {
  if (e instanceof AgentError) {
    if (e.status === 404 && !e.body) {
      return "The agent on this server does not have the privacy features yet. Update the panel, then try again.";
    }
    return agentErrorMessage(e).replace(/^(conflict|invalid input): /, "");
  }
  const code = (e as { code?: unknown } | null)?.code;
  if (code === "ENOENT" || code === "ECONNREFUSED") {
    return "The agent on this server is not running or cannot be reached.";
  }
  return e instanceof Error ? e.message : String(e);
}

function text(v: unknown): string {
  return typeof v === "string" ? v : "";
}

/** The agent may add fields; the panel takes the ones it knows. */
function cleanStatus(raw: unknown): PrivacyStatus {
  if (typeof raw !== "object" || raw === null) throw new Error("The agent's answer was empty.");
  const r = raw as Partial<PrivacyStatus>;
  const parts = Array.isArray(r.parts) ? r.parts : [];
  const notes = Array.isArray(r.notes) ? r.notes : [];
  return {
    log_ip: r.log_ip === "masked" ? "masked" : "full",
    log_query: r.log_query === "strip" ? "strip" : "keep",
    retention_days: typeof r.retention_days === "number" ? r.retention_days : null,
    parts: parts
      .filter((p) => typeof p === "object" && p !== null && typeof p.id === "string")
      .map((p) => ({
        id: p.id,
        label: text(p.label) || p.id,
        wanted: text(p.wanted),
        actual: text(p.actual),
        ok: p.ok === true,
        detail: text(p.detail),
      })),
    notes: notes.filter((n): n is string => typeof n === "string" && n !== ""),
  };
}

/** What holds on the server now, or why it could not be read. */
export async function loadPrivacyStatus(): Promise<PrivacyStatus | { error: string }> {
  try {
    return cleanStatus(await agent.privacy.status());
  } catch (e: unknown) {
    return { error: privacyAgentError(e) };
  }
}

/**
 * Tells the agent to bring the system to the stored settings. The settings
 * stay saved when this fails; the failure goes to the audit log.
 */
export async function applyPrivacy(
  actorId: number | null,
): Promise<{ ok: true; status: PrivacyStatus } | { ok: false; error: string }> {
  try {
    return { ok: true, status: cleanStatus(await agent.privacy.apply()) };
  } catch (e: unknown) {
    const error = privacyAgentError(e);
    writeAudit({ actorId, action: "privacy.apply", result: "error", message: error });
    return { ok: false, error };
  }
}

// ─── The person asked about ──────────────────────────────────────────────

export type Subject = {
  kind: SubjectKind;
  /** Normalised: an email in lower case, an IP in its shortest form. */
  value: string;
  /** Lower-case spellings to look for: `value`, and an IPv6 address as it was typed. */
  forms: string[];
};

// One address, nothing around it: no display name, no list, no spaces.
const EMAIL_RE =
  /^[a-z0-9.!#$%&'*+/=?^_`{|}~-]{1,64}@[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)*$/;

/** An IPv6 address the way the URL parser writes it: lower case, zeros collapsed. */
function shortV6(ip: string): string | null {
  try {
    return new URL(`http://[${ip}]/`).hostname.replace(/^\[|\]$/g, "");
  } catch {
    return null;
  }
}

/** The error messages never repeat the value: they can end up in the audit log. */
export function parseSubject(kind: unknown, value: unknown): Subject | { error: string } {
  if (kind !== "email" && kind !== "ip") return { error: "Choose email address or IP address." };
  const v = typeof value === "string" ? value.trim().toLowerCase() : "";
  if (kind === "email") {
    if (v.length > 254 || !EMAIL_RE.test(v)) {
      return { error: "Enter one email address, like name@example.com." };
    }
    return { kind, value: v, forms: [v] };
  }
  const family = v.length <= 45 && !/[%/]/.test(v) ? isIP(v) : 0;
  if (family === 4) return { kind, value: v, forms: [v] };
  const short = family === 6 ? shortV6(v) : null;
  if (!short) return { error: "Enter one IPv4 or IPv6 address, like 203.0.113.87 or 2001:db8::7." };
  return { kind, value: short, forms: [...new Set([short, v])] };
}

/**
 * The masked form the logs use: an IPv4 address with its last number zeroed
 * (203.0.113.87 -> 203.0.113.0), an IPv6 address cut to its first three
 * groups (2001:db8:85a3:8d3::7 -> 2001:db8:85a3::).
 */
export function maskIp(ip: string): string {
  if (isIP(ip) === 4) return ip.replace(/\.\d+$/, ".0");
  const short = shortV6(ip);
  if (!short) return ip;
  const [head, tail] = short.split("::");
  const h = head ? head.split(":") : [];
  const t = tail ? tail.split(":") : [];
  const groups = tail === undefined ? h : [...h, ...Array<string>(8 - h.length - t.length).fill("0"), ...t];
  return shortV6(`${groups.slice(0, 3).join(":")}::`) ?? ip;
}

/** For the audit log, which must not hold the value itself. */
function subjectTag(s: Subject): string {
  // Keyed with a secret of this server. A plain hash of an IPv4 address can be
  // found again by hashing all four billion of them, so the audit log would
  // still have said which address was asked about.
  const key = `privacy-audit:${config.jwtSecret}`;
  return `${s.kind} hmac:${createHmac("sha256", key).update(s.value).digest("hex").slice(0, 12)}`;
}

function escapeRe(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

const HEX = /[0-9a-f]/i;

/**
 * Whether the match at [start, end) is the subject itself and not a piece
 * of a longer address: 203.0.113.8 inside 203.0.113.87, ann@example.com
 * inside joann@example.com or ann@example.com.au, 2001:db8::1 inside
 * 2001:db8::1:2. An erase replaces what this accepts, so it leans towards no.
 */
function standsAlone(s: Subject, hay: string, start: number, end: number): boolean {
  const before = hay[start - 1] ?? "";
  const before2 = hay[start - 2] ?? "";
  const after = hay[end] ?? "";
  const after2 = hay[end + 1] ?? "";
  if (s.kind === "email") {
    if (/[a-z0-9._+-]/i.test(before)) return false;
    if (/[a-z0-9-]/i.test(after)) return false;
    return !(after === "." && /[a-z0-9]/i.test(after2));
  }
  if (isIP(s.value) === 4) {
    if (/[0-9.]/.test(before) || /[0-9]/.test(after)) return false;
    return !(after === "." && /[0-9]/.test(after2));
  }
  if (HEX.test(before) || HEX.test(after)) return false;
  if (before === ":" && (before2 === ":" || HEX.test(before2))) return false;
  if (after === ":" && (after2 === ":" || HEX.test(after2))) return false;
  return !(after === "." && /[0-9]/.test(after2));
}

/** Finds the subject in free text (any letter case), and replaces it. */
function subjectMatcher(s: Subject) {
  const pattern = [...s.forms].sort((a, b) => b.length - a.length).map(escapeRe).join("|");
  const replace = (hay: string, by: string): { text: string; n: number } => {
    const re = new RegExp(pattern, "gi");
    let out = "";
    let last = 0;
    let n = 0;
    for (let m = re.exec(hay); m !== null; m = re.exec(hay)) {
      const end = m.index + m[0].length;
      if (!standsAlone(s, hay, m.index, end)) continue;
      out += hay.slice(last, m.index) + by;
      last = end;
      n++;
    }
    return { text: out + hay.slice(last), n };
  };
  return {
    replace,
    has: (hay: string | null): boolean => hay !== null && replace(hay, "").n > 0,
  };
}

// ─── The panel's own records about the subject ───────────────────────────

const SAMPLE_COUNT = 20;
const SAMPLE_CHARS = 500;

export const PANEL_USERS = "panel_users";
export const PANEL_LOGINS = "panel_logins";
export const PANEL_AUDIT = "panel_audit";

function clip(line: string): string {
  return line.length > SAMPLE_CHARS ? `${line.slice(0, SAMPLE_CHARS - 1)}…` : line;
}

function marks(n: number): string {
  return Array(n).fill("?").join(", ");
}

function usersSource(s: Subject): SubjectSource {
  const rows = db()
    .prepare(`SELECT email, role, created_at FROM users WHERE lower(email) = ?`)
    .all(s.value) as { email: string; role: string; created_at: string }[];
  return {
    id: PANEL_USERS,
    label: "Panel account",
    count: rows.length,
    samples: rows.map((r) => clip(`${r.email}, role ${r.role}, created ${r.created_at} UTC`)),
    truncated: false,
    erasable: false,
    note:
      rows.length > 0
        ? "An account signs in to this panel with this address. It is not deleted here: remove it or change its email on the Users page."
        : "No account signs in to this panel with this address.",
  };
}

function loginsWhere(s: Subject): { where: string; params: string[] } {
  return s.kind === "email"
    ? // Two-factor code attempts are stored as `totp:<email>` (server-actions/auth.ts).
      { where: "lower(email) IN (?, ?)", params: [s.value, `totp:${s.value}`] }
    : { where: `lower(ip) IN (${marks(s.forms.length)})`, params: s.forms };
}

function loginsSource(s: Subject): SubjectSource {
  const { where, params } = loginsWhere(s);
  const count = (
    db().prepare(`SELECT COUNT(*) AS n FROM login_attempts WHERE ${where}`).get(...params) as { n: number }
  ).n;
  const rows = db()
    .prepare(
      `SELECT ip, email, ts, success FROM login_attempts WHERE ${where} ORDER BY id DESC LIMIT ${SAMPLE_COUNT}`,
    )
    .all(...params) as { ip: string; email: string | null; ts: string; success: number }[];
  return {
    id: PANEL_LOGINS,
    label: "Panel sign-in records",
    count,
    samples: rows.map((r) =>
      clip(`${r.ts} UTC  ${r.success ? "signed in" : "failed sign-in"}  from ${r.ip}  as ${r.email ?? "-"}`),
    ),
    truncated: count > rows.length,
    erasable: true,
    note: "Sign-in attempts at this panel: the time, the address they came from, the email typed and whether it worked. Erasing deletes these records.",
  };
}

type AuditHit = {
  id: number;
  ts: string;
  action: string;
  target: string | null;
  result: string;
  message: string | null;
};

/**
 * Audit entries that name the subject in their target or message, newest
 * first. SQL narrows the table down to rows that contain one of the
 * spellings; `standsAlone` then drops the ones where it is part of a longer
 * address.
 */
function* auditHits(s: Subject): Generator<AuditHit> {
  const where = s.forms
    .map(() => "instr(lower(coalesce(target, '')), ?) > 0 OR instr(lower(coalesce(message, '')), ?) > 0")
    .join(" OR ");
  const rows = db()
    .prepare(`SELECT id, ts, action, target, result, message FROM audit_log WHERE ${where} ORDER BY id DESC`)
    .iterate(...s.forms.flatMap((f) => [f, f])) as IterableIterator<AuditHit>;
  const match = subjectMatcher(s);
  for (const row of rows) {
    if (match.has(row.target) || match.has(row.message)) yield row;
  }
}

/** What an erase writes in place of the subject. */
function erasedAs(s: Subject): string {
  return s.kind === "email" ? "[erased]" : maskIp(s.value);
}

function auditSource(s: Subject): SubjectSource {
  let count = 0;
  const samples: string[] = [];
  for (const r of auditHits(s)) {
    count++;
    if (samples.length < SAMPLE_COUNT) {
      samples.push(
        clip([`${r.ts} UTC`, r.action, r.target ?? "", r.result, r.message ?? ""].filter(Boolean).join("  ")),
      );
    }
  }
  return {
    id: PANEL_AUDIT,
    label: "Panel audit log",
    count,
    samples,
    truncated: count > samples.length,
    erasable: true,
    note: `Audit log entries that name this address in their target or message. Erasing keeps the entries and writes ${erasedAs(s)} in its place.`,
  };
}

function panelSources(s: Subject): SubjectSource[] {
  return [...(s.kind === "email" ? [usersSource(s)] : []), loginsSource(s), auditSource(s)];
}

function eraseLogins(s: Subject): SubjectEraseItem {
  const { where, params } = loginsWhere(s);
  const n = db().prepare(`DELETE FROM login_attempts WHERE ${where}`).run(...params).changes;
  return { id: PANEL_LOGINS, ok: true, changed: n, detail: n === 1 ? "1 record deleted." : `${n} records deleted.` };
}

function eraseAudit(s: Subject): SubjectEraseItem {
  const by = erasedAs(s);
  if (s.forms.includes(by)) {
    return {
      id: PANEL_AUDIT,
      ok: true,
      changed: 0,
      detail: "This address is already in its masked form, so the entries stay as they are.",
    };
  }
  const conn = db();
  const match = subjectMatcher(s);
  const update = conn.prepare(`UPDATE audit_log SET target = ?, message = ? WHERE id = ?`);
  let n = 0;
  conn.transaction(() => {
    // Collected first: the rows are changed while the same table is read.
    for (const r of [...auditHits(s)]) {
      const target = r.target === null ? null : match.replace(r.target, by).text;
      const message = r.message === null ? null : match.replace(r.message, by).text;
      n += update.run(target, message, r.id).changes;
    }
  })();
  return { id: PANEL_AUDIT, ok: true, changed: n, detail: n === 1 ? "1 entry changed." : `${n} entries changed.` };
}

// ─── Search and erase, panel and agent together ──────────────────────────

/** The agent may add fields, and a source may come without some: take what the panel knows. */
function cleanSources(raw: unknown): SubjectSource[] {
  if (!Array.isArray(raw)) return [];
  return raw
    .filter((x): x is Partial<SubjectSource> & { id: string } => {
      return typeof x === "object" && x !== null && typeof (x as { id?: unknown }).id === "string";
    })
    .map((x) => ({
      id: x.id,
      label: text(x.label) || x.id,
      count: typeof x.count === "number" && x.count > 0 ? Math.floor(x.count) : 0,
      samples: (Array.isArray(x.samples) ? x.samples : [])
        .filter((l): l is string => typeof l === "string")
        .slice(0, SAMPLE_COUNT)
        .map(clip),
      truncated: x.truncated === true,
      erasable: x.erasable === true,
      note: text(x.note),
    }));
}

export type SubjectSearch = {
  kind: SubjectKind;
  value: string;
  /** The server's sources (from the agent), then the panel's own. */
  sources: SubjectSource[];
  /** Why the server's sources are missing; null when the agent answered. */
  agent_error: string | null;
  /** ISO time. */
  searched_at: string;
};

/**
 * What the server and the panel hold about one email address or IP address.
 * The panel's own records are always searched; when the agent cannot be
 * asked, the result says so instead of failing as a whole.
 */
export async function searchSubject(
  kind: unknown,
  value: unknown,
  actorId: number | null,
): Promise<{ ok: true; report: SubjectSearch } | { ok: false; error: string }> {
  const s = parseSubject(kind, value);
  if ("error" in s) return { ok: false, error: s.error };

  let fromAgent: SubjectSource[] = [];
  let agentError: string | null = null;
  try {
    const report = await agent.privacy.subjectSearch({ kind: s.kind, value: s.value });
    fromAgent = cleanSources(report?.sources);
  } catch (e: unknown) {
    agentError = privacyAgentError(e);
  }
  const own = panelSources(s);
  const ownIds = new Set(own.map((o) => o.id));
  const sources = [...fromAgent.filter((a) => !ownIds.has(a.id)), ...own];

  const total = sources.reduce((sum, src) => sum + src.count, 0);
  writeAudit({
    actorId,
    action: "privacy.subject.search",
    target: subjectTag(s),
    result: "ok",
    message:
      `${total} records in ${sources.filter((src) => src.count > 0).length} sources` +
      (agentError ? "; the server's sources could not be searched" : ""),
  });
  return {
    ok: true,
    report: {
      kind: s.kind,
      value: s.value,
      sources,
      agent_error: agentError,
      searched_at: new Date().toISOString(),
    },
  };
}

const SOURCE_ID_RE = /^[A-Za-z0-9_.:-]{1,64}$/;

/**
 * Erases the subject from the listed sources: the panel's sign-in records
 * (deleted) and audit entries (the value replaced) here, everything else
 * through the agent, which acts on its erasable sources only. Never deletes
 * a panel account. The audit entry names the kind and a hash, not the value.
 */
export async function eraseSubject(
  kind: unknown,
  value: unknown,
  sources: unknown,
  actorId: number | null,
): Promise<{ ok: true; results: SubjectEraseItem[] } | { ok: false; error: string }> {
  const s = parseSubject(kind, value);
  if ("error" in s) return { ok: false, error: s.error };
  if (!Array.isArray(sources) || sources.length === 0 || sources.length > 50) {
    return { ok: false, error: "Choose at least one source to erase from." };
  }
  if (!sources.every((id): id is string => typeof id === "string" && SOURCE_ID_RE.test(id))) {
    return { ok: false, error: "The list of sources is not valid." };
  }
  const ids = [...new Set(sources)];

  const results: SubjectEraseItem[] = [];
  const panelErase = (id: string, run: () => SubjectEraseItem) => {
    if (!ids.includes(id)) return;
    try {
      results.push(run());
    } catch (e: unknown) {
      results.push({ id, ok: false, changed: 0, detail: e instanceof Error ? e.message : String(e) });
    }
  };
  if (ids.includes(PANEL_USERS)) {
    results.push({
      id: PANEL_USERS,
      ok: false,
      changed: 0,
      detail: "Accounts are not deleted here: use the Users page.",
    });
  }
  panelErase(PANEL_LOGINS, () => eraseLogins(s));
  panelErase(PANEL_AUDIT, () => eraseAudit(s));

  const forAgent = ids.filter((id) => id !== PANEL_USERS && id !== PANEL_LOGINS && id !== PANEL_AUDIT);
  if (forAgent.length > 0) {
    try {
      const resp = await agent.privacy.subjectErase({ kind: s.kind, value: s.value, sources: forAgent });
      const got = (Array.isArray(resp?.results) ? resp.results : []).filter(
        (r) => typeof r === "object" && r !== null && typeof r.id === "string",
      );
      for (const r of got) {
        results.push({
          id: r.id,
          ok: r.ok === true,
          changed: typeof r.changed === "number" && r.changed > 0 ? Math.floor(r.changed) : 0,
          detail: text(r.detail),
        });
      }
      for (const id of forAgent) {
        if (!got.some((r) => r.id === id)) {
          results.push({ id, ok: false, changed: 0, detail: "The server did not report on this source." });
        }
      }
    } catch (e: unknown) {
      const detail = privacyAgentError(e);
      for (const id of forAgent) results.push({ id, ok: false, changed: 0, detail });
    }
  }

  // Ids, counts and outcomes only: a `detail` may quote the value.
  writeAudit({
    actorId,
    action: "privacy.subject.erase",
    target: subjectTag(s),
    result: results.every((r) => r.ok) ? "ok" : "error",
    message: results.map((r) => `${r.id}: ${r.ok ? `${r.changed} changed` : "failed"}`).join("; "),
  });
  return { ok: true, results };
}
