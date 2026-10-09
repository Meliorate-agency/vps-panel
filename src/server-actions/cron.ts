"use server";

import { revalidatePath } from "next/cache";
import { agent } from "@/lib/agent";
import { requireAdmin } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";

export type ActionResult = { ok: true } | { ok: false; error: string };

export async function createCronAction(
  _prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
  const s = await requireAdmin();
  const schedule = String(fd.get("schedule") ?? "").trim();
  const user = String(fd.get("user") ?? "root").trim();
  const command = String(fd.get("command") ?? "").trim();
  const label = String(fd.get("label") ?? "").trim() || undefined;
  try {
    await agent.cron.create({ schedule, user, command, label });
    writeAudit({ actorId: s.userId, action: "cron.create", target: command, result: "ok" });
    revalidatePath("/cron");
    return { ok: true };
  } catch (e: unknown) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }
}

export async function deleteCronAction(id: string): Promise<void> {
  const s = await requireAdmin();
  await agent.cron.delete(id);
  writeAudit({ actorId: s.userId, action: "cron.delete", target: id, result: "ok" });
  revalidatePath("/cron");
}
