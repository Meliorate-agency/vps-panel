"use server";

import { agent, type UpdateCheckResult } from "@/lib/agent";
import { requireAdmin, requireSession } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";
import { agentErrorMessage } from "@/lib/redirects";

export type CheckResult =
  | { ok: true; data: UpdateCheckResult }
  | { ok: false; error: string };

export type TriggerResult =
  | { ok: true; taskId: string }
  | { ok: false; error: string };

export async function checkUpdateAction(): Promise<CheckResult> {
  await requireSession();
  try {
    const data = await agent.system.updateCheck();
    return { ok: true, data };
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }
}

export async function triggerUpdateAction(): Promise<TriggerResult> {
  const s = await requireAdmin();
  try {
    const accepted = await agent.system.selfUpdate();
    writeAudit({
      actorId: s.userId,
      action: "system.self-update",
      target: accepted.task_id,
      result: "ok",
    });
    return { ok: true, taskId: accepted.task_id };
  } catch (e) {
    // e.g. "wait for the running tasks to finish before updating the panel: apps.update:x"
    const msg = agentErrorMessage(e).replace(/^conflict: /, "");
    writeAudit({
      actorId: s.userId,
      action: "system.self-update",
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
}
