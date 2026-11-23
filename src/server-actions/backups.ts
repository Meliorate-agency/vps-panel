"use server";

import { revalidatePath } from "next/cache";
import { agent } from "@/lib/agent";
import { requireAdmin } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";
import { agentErrorMessage } from "@/lib/redirects";

export type RunBackupResult = { ok: true; taskId: string } | { ok: false; error: string };

/** Returns the refusal (e.g. a backup already running) instead of throwing it
 *  into the page's error screen. */
export async function runBackupAction(): Promise<RunBackupResult> {
  const s = await requireAdmin();
  try {
    const resp = await agent.backups.run();
    writeAudit({ actorId: s.userId, action: "backups.run", result: "ok" });
    return { ok: true, taskId: resp.task_id };
  } catch (e: unknown) {
    const msg = agentErrorMessage(e).replace(/^(conflict|invalid input): /, "");
    writeAudit({ actorId: s.userId, action: "backups.run", result: "error", message: msg });
    return { ok: false, error: msg };
  }
}

/** Like runBackupAction, returns the refusal instead of throwing it. */
export async function restoreBackupAction(snapshot: string, target: string): Promise<RunBackupResult> {
  const s = await requireAdmin();
  try {
    const resp = await agent.backups.restore(snapshot, target);
    writeAudit({ actorId: s.userId, action: "backups.restore", target: `${snapshot}->${target}`, result: "ok" });
    return { ok: true, taskId: resp.task_id };
  } catch (e: unknown) {
    const msg = agentErrorMessage(e).replace(/^(conflict|invalid input): /, "");
    writeAudit({
      actorId: s.userId,
      action: "backups.restore",
      target: `${snapshot}->${target}`,
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
}
