import "server-only";
import crypto from "node:crypto";
import { db } from "@/lib/db";
import { config } from "@/lib/config";
import { ownUrl } from "@/lib/networks/identity";
import { randomToken } from "@/lib/networks/protocol";

/**
 * One-time links another server pulls a server snapshot with
 * (`/api/snapshot-pull/<id>#<secret>`). The secret travels in the fragment,
 * which a browser or curl never sends: the pulling agent sends it in an
 * Authorization header, so it is in no log. Only its hash is kept here.
 *
 * "One-time but resumable": the first server that uses the link with the
 * right secret is bound to it (its client id) and may ask for ranges until
 * the link expires, it says it is done, or three times the file has been
 * served. Any other server is refused.
 */

export const LINK_ID = /^[A-Za-z0-9_-]{22}$/;
export const LINK_TTLS_HOURS = [1, 24, 72, 168] as const;
/** Wrong secrets on one link before it is revoked. */
const MAX_LINK_FAILURES = 10;

export type LinkRow = {
  id: string;
  snapshot: string;
  secret_hash: string;
  created_by: number | null;
  created_at: number;
  expires_at: number;
  client_id: string | null;
  claimed_at: number | null;
  claimed_ip: string | null;
  bytes_served: number;
  failures: number;
  completed_at: number | null;
  revoked_at: number | null;
};

export type LinkView = Omit<LinkRow, "secret_hash"> & { state: "open" | "in use" | "done" | "expired" | "revoked" };

function hash(secret: string): string {
  return crypto.createHash("sha256").update(secret).digest("hex");
}

export function linkState(r: LinkRow, now = Date.now()): LinkView["state"] {
  if (r.revoked_at) return "revoked";
  if (r.completed_at) return "done";
  if (r.expires_at <= now) return "expired";
  return r.client_id ? "in use" : "open";
}

/** The address another server reaches this panel at, and a caveat when it
 *  is plain http (the file stays encrypted, but the link's secret then
 *  travels in clear). */
export function linkBase(): { base: string; warning: string | null } {
  const own = ownUrl();
  if (own.url) return { base: own.url, warning: null };
  return {
    base: `http://${config.panelHost}`,
    warning:
      "This panel has no HTTPS name yet, so the link is plain http: the snapshot stays encrypted, but the link's secret can be read on the way. Set a panel hostname first if you can.",
  };
}

export function createLink(snapshot: string, ttlHours: number, userId: number) {
  const id = randomToken(16);
  const secret = randomToken(32);
  const now = Date.now();
  const expiresAt = now + ttlHours * 3600_000;
  db()
    .prepare(
      `INSERT INTO snapshot_links (id, snapshot, secret_hash, created_by, created_at, expires_at)
       VALUES (?, ?, ?, ?, ?, ?)`,
    )
    .run(id, snapshot, hash(secret), userId, now, expiresAt);
  const { base, warning } = linkBase();
  return { id, url: `${base}/api/snapshot-pull/${id}#${secret}`, expiresAt, warning };
}

export function getLink(id: string): LinkRow | undefined {
  return db().prepare(`SELECT * FROM snapshot_links WHERE id = ?`).get(id) as LinkRow | undefined;
}

/** True when `secret` is this link's, compared in constant time. */
export function secretMatches(row: LinkRow, secret: string): boolean {
  const a = Buffer.from(hash(secret), "hex");
  const b = Buffer.from(row.secret_hash, "hex");
  return a.length === b.length && crypto.timingSafeEqual(a, b);
}

/** A wrong secret for an existing link; true when that revoked it. */
export function noteWrongSecret(id: string): boolean {
  const r = db()
    .prepare(`UPDATE snapshot_links SET failures = failures + 1 WHERE id = ? RETURNING failures, revoked_at`)
    .get(id) as { failures: number; revoked_at: number | null } | undefined;
  if (r && r.failures >= MAX_LINK_FAILURES && !r.revoked_at) {
    revokeLink(id);
    return true;
  }
  return false;
}

/**
 * Binds the link to `clientId` if nobody has it yet. Returns "claimed" the
 * first time, "same" for the server that holds it, "other" for anyone else.
 */
export function bindClient(id: string, clientId: string, ip: string): "claimed" | "same" | "other" {
  const claimed = db()
    .prepare(
      `UPDATE snapshot_links SET client_id = ?, claimed_at = ?, claimed_ip = ?
       WHERE id = ? AND client_id IS NULL`,
    )
    .run(clientId, Date.now(), ip, id);
  if (claimed.changes === 1) return "claimed";
  const row = getLink(id);
  return row?.client_id === clientId ? "same" : "other";
}

export function addServed(id: string, bytes: number) {
  if (bytes <= 0) return;
  db().prepare(`UPDATE snapshot_links SET bytes_served = bytes_served + ? WHERE id = ?`).run(bytes, id);
}

export function completeLink(id: string) {
  db().prepare(`UPDATE snapshot_links SET completed_at = ? WHERE id = ? AND completed_at IS NULL`).run(Date.now(), id);
}

export function revokeLink(id: string) {
  db().prepare(`UPDATE snapshot_links SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL`).run(Date.now(), id);
}

/** Every link of a snapshot, when the snapshot is deleted. */
export function revokeLinksFor(snapshot: string) {
  db()
    .prepare(`UPDATE snapshot_links SET revoked_at = ? WHERE snapshot = ? AND revoked_at IS NULL`)
    .run(Date.now(), snapshot);
}

/** Links for the Backups page: open and in-use ones, and those that ended
 *  in the last 7 days. Never the secret (it is not stored). */
export function listLinks(): LinkView[] {
  const weekAgo = Date.now() - 7 * 24 * 3600_000;
  const rows = db()
    .prepare(
      `SELECT * FROM snapshot_links
       WHERE created_at > ? OR (revoked_at IS NULL AND completed_at IS NULL)
       ORDER BY created_at DESC`,
    )
    .all(weekAgo) as LinkRow[];
  const now = Date.now();
  return rows.map(({ secret_hash: _omit, ...r }) => ({ ...r, state: linkState({ ...r, secret_hash: "" }, now) }));
}
