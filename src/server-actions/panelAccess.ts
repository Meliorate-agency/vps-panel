"use server";

import { revalidatePath } from "next/cache";
import { agent } from "@/lib/agent";
import { requireAdmin } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";
import { getClientIp } from "@/lib/getClientIp";
import { ipAllowed, parseCidr } from "@/lib/ipMatch";
import { PANEL_ACCESS_KEY, splitEntries } from "@/lib/panelAccess";
import { setSetting } from "@/lib/settings";

export type ActionResult = { ok: true } | { ok: false; error: string };

/**
 * Save the panel access allowlist. The agent writes it into nginx, so it
 * applies to every request, the MCP endpoint included. A list that leaves
 * out the address this request came from is refused: saving it would lock
 * the operator out on the next click.
 */
export async function setPanelAccessAction(
  _prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
  const s = await requireAdmin();
  const entries = splitEntries(String(fd.get("entries") ?? ""));
  const bad = entries.find((e) => parseCidr(e) === null);
  if (bad) return { ok: false, error: `Not an IP address or CIDR range: ${bad}` };

  const ip = await getClientIp();
  if (entries.length > 0 && !ipAllowed(ip, entries)) {
    return {
      ok: false,
      error: `Your current address ${ip} is not in the list, so saving it would lock you out. Add it first.`,
    };
  }

  try {
    await agent.system.setPanelAccess(entries);
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({ actorId: s.userId, action: "panel.access.set", result: "error", message: msg });
    return { ok: false, error: msg };
  }
  setSetting(PANEL_ACCESS_KEY, entries.join(", "));
  writeAudit({
    actorId: s.userId,
    action: "panel.access.set",
    result: "ok",
    message: entries.length ? entries.join(", ") : "open to every address",
  });
  revalidatePath("/settings");
  return { ok: true };
}
