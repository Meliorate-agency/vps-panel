"use server";

import { revalidatePath } from "next/cache";
import { agent } from "@/lib/agent";
import { requireAdmin } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";

export async function serviceActionExec(unit: string, action: string): Promise<void> {
  const s = await requireAdmin();
  await agent.services.action(unit, action);
  writeAudit({ actorId: s.userId, action: `services.${action}`, target: unit, result: "ok" });
  revalidatePath("/services");
}
