"use server";

import { revalidatePath } from "next/cache";
import { agent, type ImportPreflight, type ServerSnapshotEstimate } from "@/lib/agent";
import { requireAdmin } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";
import { agentErrorMessage } from "@/lib/redirects";
import { passphraseProblem } from "@/lib/serverSnapshots/passphrase";
import { SNAPSHOT_NAME } from "@/lib/serverSnapshots/relay";
import {
  LINK_ID,
  LINK_TTLS_HOURS,
  createLink,
  getLink,
  revokeLink,
  revokeLinksFor,
} from "@/lib/serverSnapshots/links";

export type EstimateResult = { ok: true; estimate: ServerSnapshotEstimate } | { ok: false; error: string };
export type StartResult = { ok: true; taskId: string } | { ok: false; error: string };
export type DeleteResult = { ok: true } | { ok: false; error: string };

function plain(e: unknown): string {
  return agentErrorMessage(e).replace(/^(conflict|invalid input): /, "");
}

/** What a snapshot would hold and whether it fits on the disk. */
export async function estimateSnapshotAction(includeDrive: boolean): Promise<EstimateResult> {
  await requireAdmin();
  try {
    return { ok: true, estimate: await agent.serverSnapshots.estimate(includeDrive) };
  } catch (e: unknown) {
    return { ok: false, error: plain(e) };
  }
}

/**
 * Starts the export. The passphrase goes to the agent in the request body
 * and nowhere else: not into the audit row (not even hashed), a URL, the
 * database or a log.
 */
export async function createSnapshotAction(
  passphrase: string,
  includeDrive: boolean,
  pauseDocker: boolean,
): Promise<StartResult> {
  const s = await requireAdmin();
  const problem = passphraseProblem(passphrase);
  if (problem) return { ok: false, error: problem };
  const what = includeDrive ? "with Drive" : "without Drive";
  try {
    const resp = await agent.serverSnapshots.create(passphrase, includeDrive, pauseDocker);
    writeAudit({ actorId: s.userId, action: "snapshots.create", target: what, result: "ok" });
    return { ok: true, taskId: resp.task_id };
  } catch (e: unknown) {
    const msg = plain(e);
    writeAudit({ actorId: s.userId, action: "snapshots.create", target: what, result: "error", message: msg });
    return { ok: false, error: msg };
  }
}

export type LinkResult =
  | { ok: true; url: string; expiresAt: number; warning: string | null }
  | { ok: false; error: string };

/**
 * A one-time link another server pulls `name` with. The link is returned
 * once; only a hash of its secret is kept.
 */
export async function createLinkAction(name: string, ttlHours: number): Promise<LinkResult> {
  const s = await requireAdmin();
  if (!SNAPSHOT_NAME.test(name)) return { ok: false, error: "not a snapshot name" };
  if (!(LINK_TTLS_HOURS as readonly number[]).includes(ttlHours)) return { ok: false, error: "pick how long the link works" };
  const list = await agent.serverSnapshots.list().catch(() => null);
  if (!list?.snapshots.some((x) => x.name === name && x.location === "local")) {
    return { ok: false, error: "no snapshot of that name was made on this server" };
  }
  const link = createLink(name, ttlHours, s.userId);
  writeAudit({
    actorId: s.userId,
    action: "snapshots.link.create",
    target: name,
    result: "ok",
    message: `link ${link.id}, ${ttlHours} h`,
  });
  revalidatePath("/server-snapshots");
  return { ok: true, url: link.url, expiresAt: link.expiresAt, warning: link.warning };
}

export async function revokeLinkAction(id: string): Promise<DeleteResult> {
  const s = await requireAdmin();
  if (!LINK_ID.test(id)) return { ok: false, error: "not a link" };
  const row = getLink(id);
  if (!row) return { ok: false, error: "no such link" };
  revokeLink(id);
  writeAudit({ actorId: s.userId, action: "snapshots.link.revoke", target: row.snapshot, result: "ok", message: `link ${id}` });
  revalidatePath("/server-snapshots");
  return { ok: true };
}

/**
 * On the NEW server: fetch a snapshot from the old one's link. The link is
 * `https://<panel>/api/snapshot-pull/<id>#<secret>`; the secret (fragment)
 * goes to the agent apart from the address and is not audited.
 */
export async function pullSnapshotAction(link: string): Promise<StartResult> {
  const s = await requireAdmin();
  let url: URL;
  try {
    url = new URL(link.trim());
  } catch {
    return { ok: false, error: "That is not a link." };
  }
  const secret = url.hash.replace(/^#/, "");
  url.hash = "";
  if (!/^https?:$/.test(url.protocol) || !/^\/api\/snapshot-pull\/[A-Za-z0-9_-]{22}$/.test(url.pathname) || url.search) {
    return { ok: false, error: "That is not a snapshot link (it looks like https://panel.example.com/api/snapshot-pull/...#...)." };
  }
  if (!/^[A-Za-z0-9_-]{43}$/.test(secret)) {
    return { ok: false, error: "The link is missing its secret (the part after #). Copy the whole link." };
  }
  try {
    const resp = await agent.serverSnapshots.pull(url.toString(), secret);
    writeAudit({ actorId: s.userId, action: "snapshots.pull", target: url.host, result: "ok" });
    return { ok: true, taskId: resp.task_id };
  } catch (e: unknown) {
    const msg = plain(e);
    writeAudit({ actorId: s.userId, action: "snapshots.pull", target: url.host, result: "error", message: msg });
    return { ok: false, error: msg };
  }
}

export type PreflightResult = { ok: true; preflight: ImportPreflight } | { ok: false; error: string };

/** Opens a snapshot with its passphrase and says what it holds and whether
 *  this panel can import it. Changes nothing. */
export async function preflightImportAction(name: string, passphrase: string): Promise<PreflightResult> {
  await requireAdmin();
  if (!SNAPSHOT_NAME.test(name)) return { ok: false, error: "not a snapshot name" };
  if (!passphrase) return { ok: false, error: "Enter the snapshot's passphrase." };
  try {
    return { ok: true, preflight: await agent.serverSnapshots.importPreflight(name, passphrase) };
  } catch (e: unknown) {
    return { ok: false, error: plain(e) };
  }
}

/**
 * Imports a snapshot: this server takes over everything the old one
 * hosted, its panel logins included. The passphrase goes to the agent only.
 */
export async function startImportAction(name: string, passphrase: string, replacesOld: boolean): Promise<StartResult> {
  const s = await requireAdmin();
  if (!SNAPSHOT_NAME.test(name)) return { ok: false, error: "not a snapshot name" };
  try {
    const resp = await agent.serverSnapshots.import(name, passphrase, replacesOld);
    writeAudit({
      actorId: s.userId,
      action: "snapshots.import",
      target: name,
      result: "ok",
      message: replacesOld ? "replaces the old server" : undefined,
    });
    return { ok: true, taskId: resp.task_id };
  } catch (e: unknown) {
    const msg = plain(e);
    writeAudit({ actorId: s.userId, action: "snapshots.import", target: name, result: "error", message: msg });
    return { ok: false, error: msg };
  }
}

/** Undoes an import that stopped part way (its journal says what it did). */
export async function discardImportAction(name: string): Promise<DeleteResult> {
  const s = await requireAdmin();
  if (!SNAPSHOT_NAME.test(name)) return { ok: false, error: "not a snapshot name" };
  try {
    await agent.serverSnapshots.discardImport(name);
    writeAudit({ actorId: s.userId, action: "snapshots.import.discard", target: name, result: "ok" });
    return { ok: true };
  } catch (e: unknown) {
    const msg = plain(e);
    writeAudit({ actorId: s.userId, action: "snapshots.import.discard", target: name, result: "error", message: msg });
    return { ok: false, error: msg };
  }
}

export async function deleteSnapshotAction(name: string): Promise<DeleteResult> {
  const s = await requireAdmin();
  if (!SNAPSHOT_NAME.test(name)) return { ok: false, error: "not a snapshot name" };
  try {
    await agent.serverSnapshots.delete(name);
    // A link for a file that is gone must not work for a new file that
    // gets the same name.
    revokeLinksFor(name);
    writeAudit({ actorId: s.userId, action: "snapshots.delete", target: name, result: "ok" });
    revalidatePath("/server-snapshots");
    return { ok: true };
  } catch (e: unknown) {
    const msg = plain(e);
    writeAudit({ actorId: s.userId, action: "snapshots.delete", target: name, result: "error", message: msg });
    return { ok: false, error: msg };
  }
}
