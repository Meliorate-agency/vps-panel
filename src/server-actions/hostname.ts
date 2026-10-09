"use server";

import { revalidatePath } from "next/cache";
import { isIP } from "node:net";
import { agent } from "@/lib/agent";
import { requireAdmin } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";
import { agentErrorMessage } from "@/lib/redirects";

export type HostnameResult =
  | { ok: true; hostname: string; steps: string[] }
  | { ok: false; error: string };

const LABEL = /^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$/;

/**
 * Sets the server's own hostname (Settings > Server). The checks mirror the
 * agent's (agent/src/handlers/hostname.rs) so a typo gets its message
 * before the round trip; the agent checks again.
 */
export async function setServerHostnameAction(
  _prev: HostnameResult | null,
  fd: FormData,
): Promise<HostnameResult> {
  const s = await requireAdmin();
  const raw = String(fd.get("hostname") ?? "").trim().toLowerCase();
  const name = raw.endsWith(".") ? raw.slice(0, -1) : raw;
  if (!name || name.length > 64 || isIP(name) || !name.split(".").every((l) => LABEL.test(l))) {
    return {
      ok: false,
      error:
        "Use letters, digits and hyphens, with dots between the parts (vps.example.com), 64 characters at most.",
    };
  }
  try {
    const r = await agent.system.setHostname(name);
    writeAudit({ actorId: s.userId, action: "system.hostname.set", target: r.hostname, result: "ok" });
    revalidatePath("/settings");
    return { ok: true, hostname: r.hostname, steps: r.steps };
  } catch (e: unknown) {
    const msg = agentErrorMessage(e).replace(/^invalid input: /, "");
    writeAudit({
      actorId: s.userId,
      action: "system.hostname.set",
      target: name,
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
}
