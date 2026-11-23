"use server";

import { revalidatePath } from "next/cache";
import { agent } from "@/lib/agent";
import { requireAdmin } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";

// Returns the agent's refusal: thrown, its message is replaced by Next in a
// production build and the operator gets the page's error screen.
export async function serviceActionExec(
  unit: string,
  action: string,
): Promise<{ ok: true } | { ok: false; error: string }> {
  const s = await requireAdmin();
  try {
    await agent.services.action(unit, action);
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({ actorId: s.userId, action: `services.${action}`, target: unit, result: "error", message: msg });
    // A refused restart can still leave the unit in another state.
    revalidatePath("/services");
    return { ok: false, error: msg };
  }
  writeAudit({ actorId: s.userId, action: `services.${action}`, target: unit, result: "ok" });
  revalidatePath("/services");
  return { ok: true };
}
