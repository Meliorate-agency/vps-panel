import { NextRequest } from "next/server";
import { getSession } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";
import { SNAPSHOT_NAME, relaySnapshotFile } from "@/lib/serverSnapshots/relay";

export const dynamic = "force-dynamic";
export const runtime = "nodejs";

/**
 * Downloads a server snapshot (Backups > Server snapshot). Admins only: the
 * file holds every database, mailbox and secret of the server, encrypted
 * with the passphrase chosen when it was made. Resumable (Range).
 */
async function handle(req: NextRequest, params: Promise<{ name: string }>, method: "GET" | "HEAD") {
  const session = await getSession();
  if (!session || session.needsTotp) return new Response("unauthorized", { status: 401 });
  if (session.role !== "admin") return new Response("admins only", { status: 403 });
  const { name } = await params;
  if (!SNAPSHOT_NAME.test(name)) return new Response("not a snapshot name", { status: 400 });

  const range = req.headers.get("range");
  const res = await relaySnapshotFile({ name, method, range, ifRange: req.headers.get("if-range") });
  // One audit row per download, not per resumed piece of it.
  const fromStart = !range || /^bytes=0-/.test(range.trim());
  if (method === "GET" && fromStart) {
    writeAudit({
      actorId: session.userId,
      action: "snapshots.download",
      target: name,
      result: res.ok ? "ok" : "error",
      message: res.ok ? undefined : `HTTP ${res.status}`,
    });
  }
  return res;
}

export async function GET(req: NextRequest, { params }: { params: Promise<{ name: string }> }) {
  return handle(req, params, "GET");
}

export async function HEAD(req: NextRequest, { params }: { params: Promise<{ name: string }> }) {
  return handle(req, params, "HEAD");
}
