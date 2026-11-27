"use server";

import { revalidatePath } from "next/cache";
import { agent, type ServerSnapshotEstimate } from "@/lib/agent";
import { requireAdmin } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";
import { agentErrorMessage } from "@/lib/redirects";
import { passphraseProblem } from "@/lib/serverSnapshots/passphrase";
import { SNAPSHOT_NAME } from "@/lib/serverSnapshots/relay";

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

export async function deleteSnapshotAction(name: string): Promise<DeleteResult> {
  const s = await requireAdmin();
  if (!SNAPSHOT_NAME.test(name)) return { ok: false, error: "not a snapshot name" };
  try {
    await agent.serverSnapshots.delete(name);
    writeAudit({ actorId: s.userId, action: "snapshots.delete", target: name, result: "ok" });
    revalidatePath("/backups");
    return { ok: true };
  } catch (e: unknown) {
    const msg = plain(e);
    writeAudit({ actorId: s.userId, action: "snapshots.delete", target: name, result: "error", message: msg });
    return { ok: false, error: msg };
  }
}
