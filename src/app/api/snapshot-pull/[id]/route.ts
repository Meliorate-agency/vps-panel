import { NextRequest } from "next/server";
import { agent, type ServerSnapshot } from "@/lib/agent";
import { writeAudit } from "@/lib/audit";
import { blocked, clientIp, noteFailure, overLimit } from "@/lib/networks/peer";
import {
  LINK_ID,
  addServed,
  bindClient,
  completeLink,
  getLink,
  linkState,
  noteWrongSecret,
  revokeLink,
  secretMatches,
  type LinkRow,
} from "@/lib/serverSnapshots/links";
import { relaySnapshotFile } from "@/lib/serverSnapshots/relay";

export const dynamic = "force-dynamic";
export const runtime = "nodejs";

/**
 * A server snapshot, pulled by another server with a one-time link
 * (Backups > Server snapshot > Link). Public: nginx lets this exact path
 * through from any address, past Settings > Panel access, so a freshly
 * installed panel elsewhere can fetch the file directly. Every request
 * carries the link's secret (Authorization: Bearer) and the pulling
 * server's client id (X-Snapshot-Client); the first one binds the link.
 *
 *   HEAD    size, sha256, format and panel commit, before any byte moves
 *   GET     the file, with Range so a broken transfer resumes
 *   DELETE  "done": the link closes
 */

const CLIENT_ID = /^[A-Za-z0-9_-]{16,64}$/;
/** Requests per minute from one address (a resuming client retries). */
const PER_MINUTE = 60;
/** How much one link may serve, in multiples of the file. */
const MAX_SERVED_FACTOR = 3;
/** bytes_served is written in steps of this, and at the end. */
const COUNT_STEP = 64 * 1024 * 1024;

function plain(status: number, message: string): Response {
  return Response.json({ error: message }, { status, headers: { "cache-control": "no-store" } });
}

type Authorized = { row: LinkRow; snapshot: ServerSnapshot; ip: string };

async function authorize(req: NextRequest, id: string): Promise<Authorized | Response> {
  const ip = clientIp(req);
  if (blocked(ip)) return plain(429, "too many bad requests from this address; try again in a few minutes");
  if (overLimit("snapshot-pull", ip, PER_MINUTE, 60_000)) return plain(429, "too many requests");
  // Unknown link and wrong secret look the same from outside.
  const notFound = () => plain(404, "no such link");
  if (!LINK_ID.test(id)) return notFound();
  const auth = req.headers.get("authorization") ?? "";
  const secret = auth.startsWith("Bearer ") ? auth.slice(7).trim() : "";
  const row = getLink(id);
  if (!row || !secret || !secretMatches(row, secret)) {
    noteFailure(ip);
    if (row && secret) {
      const revoked = noteWrongSecret(id);
      if (revoked) {
        writeAudit({
          actorId: null,
          action: "snapshots.link.locked",
          target: row.snapshot,
          result: "error",
          message: `link ${id} revoked after repeated wrong secrets; last from ip=${ip}`,
        });
      }
    }
    return notFound();
  }
  const state = linkState(row);
  if (state === "revoked" || state === "done") return plain(410, "this link is no longer valid");
  if (state === "expired") return plain(410, "this link has expired; create a new one on the other server");
  const clientId = req.headers.get("x-snapshot-client") ?? "";
  if (!CLIENT_ID.test(clientId)) return plain(400, "missing or invalid X-Snapshot-Client");
  const bound = bindClient(id, clientId, ip);
  if (bound === "other") return plain(409, "another server is already pulling with this link");
  if (bound === "claimed") {
    writeAudit({
      actorId: null,
      action: "snapshots.link.claim",
      target: row.snapshot,
      result: "ok",
      message: `link ${id} taken by ip=${ip}`,
    });
  }
  const list = await agent.serverSnapshots.list().catch(() => null);
  const snapshot = list?.snapshots.find((s) => s.name === row.snapshot && s.location === "local");
  if (!snapshot) {
    revokeLink(id);
    return plain(410, "the snapshot this link was for is gone");
  }
  if (row.bytes_served >= MAX_SERVED_FACTOR * snapshot.size) {
    return plain(410, "this link has served the file three times over; create a new one");
  }
  return { row, snapshot, ip };
}

function snapshotHeaders(res: Response, s: ServerSnapshot): Response {
  const headers = new Headers(res.headers);
  headers.set("X-Snapshot-Name", s.name);
  headers.set("X-Snapshot-Size", String(s.size));
  headers.set("X-Snapshot-Sha256", s.sha256);
  headers.set("X-Snapshot-Format", String(s.format));
  headers.set("X-Snapshot-Panel-Commit", s.panel_commit);
  return new Response(res.body, { status: res.status, headers });
}

export async function HEAD(req: NextRequest, { params }: { params: Promise<{ id: string }> }) {
  const { id } = await params;
  const a = await authorize(req, id);
  if (a instanceof Response) return a;
  const res = await relaySnapshotFile({ name: a.snapshot.name, method: "HEAD", range: null, ifRange: null });
  return snapshotHeaders(res, a.snapshot);
}

export async function GET(req: NextRequest, { params }: { params: Promise<{ id: string }> }) {
  const { id } = await params;
  const a = await authorize(req, id);
  if (a instanceof Response) return a;
  let pending = 0;
  const res = await relaySnapshotFile({
    name: a.snapshot.name,
    method: "GET",
    range: req.headers.get("range"),
    ifRange: req.headers.get("if-range"),
    onBytes: (n) => {
      pending += n;
      if (pending >= COUNT_STEP) {
        addServed(id, pending);
        pending = 0;
      }
    },
    onEnd: () => {
      addServed(id, pending);
      pending = 0;
    },
  });
  return snapshotHeaders(res, a.snapshot);
}

export async function DELETE(req: NextRequest, { params }: { params: Promise<{ id: string }> }) {
  const { id } = await params;
  const a = await authorize(req, id);
  if (a instanceof Response) return a;
  completeLink(id);
  const served = getLink(id)?.bytes_served ?? 0;
  writeAudit({
    actorId: null,
    action: "snapshots.link.complete",
    target: a.row.snapshot,
    result: "ok",
    message: `link ${id} done; ${served} bytes served to ip=${a.ip}`,
  });
  return Response.json({ ok: true }, { headers: { "cache-control": "no-store" } });
}
