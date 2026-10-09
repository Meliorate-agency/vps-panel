"use server";

import { agent } from "@/lib/agent";
import { requireAdmin } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";
import { agentErrorMessage } from "@/lib/redirects";

export type SwapResult = { ok: true; taskId: string } | { ok: false; error: string };

/** A missing field must never read as 0, which would turn swap off. */
function wholeNumber(fd: FormData, name: string): number | null {
  const raw = fd.get(name);
  return typeof raw === "string" && /^\d{1,6}$/.test(raw) ? Number(raw) : null;
}

/**
 * Starts the agent's swap task (create, resize or remove /swapfile + set
 * vm.swappiness) and returns its id. The form streams the task log and
 * refreshes the page when the task ends, so nothing is revalidated here.
 */
export async function setSwapAction(
  _prev: SwapResult | null,
  fd: FormData,
): Promise<SwapResult> {
  const s = await requireAdmin();
  const sizeMb = wholeNumber(fd, "size_mb");
  const swappiness = wholeNumber(fd, "swappiness");
  if (sizeMb === null) return { ok: false, error: "Pick a swap file size." };
  // Kernels since 5.8 take up to 200 (meant for zram); the form offers the
  // live value, so it has to save.
  if (swappiness === null || swappiness > 200) {
    return { ok: false, error: "Swappiness must be between 0 and 200." };
  }
  const target = JSON.stringify({ size_mb: sizeMb, swappiness });
  try {
    const { task_id } = await agent.system.setSwap({ size_mb: sizeMb, swappiness });
    writeAudit({ actorId: s.userId, action: "system.swap.set", target, result: "ok" });
    return { ok: true, taskId: task_id };
  } catch (e: unknown) {
    // The agent's refusals are sentences once its error-kind prefix is off.
    const msg = agentErrorMessage(e).replace(/^(conflict|invalid input): /, "");
    writeAudit({
      actorId: s.userId,
      action: "system.swap.set",
      target,
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
}
