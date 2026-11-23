"use server";

import { revalidatePath } from "next/cache";
import { agent, AgentError, type FailedByIp } from "@/lib/agent";
import { requireAdmin, requireSession } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";
import { agentErrorMessage } from "@/lib/redirects";

export type ActionResult = { ok: true } | { ok: false; error: string };

/** The port form's result. `needsForce`: the agent refused to block a port
 *  the server needs (sshd's, 80 or 443); the operator may confirm and send it
 *  again with force. */
export type PortActionResult =
  | { ok: true }
  | { ok: false; error: string; needsForce?: boolean };

export async function firewallPortAction(
  _prev: PortActionResult | null,
  fd: FormData,
): Promise<PortActionResult> {
  const s = await requireAdmin();
  const intent = String(fd.get("intent") ?? "");
  const port = Number(fd.get("port"));
  const proto = String(fd.get("proto") ?? "tcp");
  if (!Number.isFinite(port) || port < 1 || port > 65535) {
    return { ok: false, error: "port must be 1-65535" };
  }
  if (intent !== "allow" && intent !== "deny") {
    return { ok: false, error: "intent must be allow or deny" };
  }
  // Only the form's "Block it anyway" button sends this.
  const forced = intent === "deny" && fd.get("force") === "1";
  try {
    if (intent === "allow") {
      await agent.firewall.allow(port, proto);
    } else {
      await agent.firewall.deny(port, proto, forced);
    }
    writeAudit({
      actorId: s.userId,
      action: `firewall.${intent}`,
      target: `${port}/${proto}`,
      result: "ok",
      message: forced ? "blocked after confirming the agent's warning" : undefined,
    });
    revalidatePath("/firewall");
    return { ok: true };
  } catch (e: unknown) {
    const error = agentErrorMessage(e).replace(/^(conflict|invalid input): /, "");
    // Refused because the server needs this port: nothing changed, the
    // operator decides.
    if (intent === "deny" && !forced && e instanceof AgentError && e.status === 409) {
      return { ok: false, error, needsForce: true };
    }
    return { ok: false, error };
  }
}

// The button actions below return a failure as `{ ok: false, error }`. Thrown,
// the message is replaced by Next in a production build and the operator gets
// the page's error screen instead of the agent's reason.
function failed(
  actorId: number,
  action: string,
  target: string | undefined,
  e: unknown,
): ActionResult {
  const msg = e instanceof Error ? e.message : String(e);
  writeAudit({ actorId, action, target, result: "error", message: msg });
  return { ok: false, error: msg };
}

export async function unbanIpAction(jail: string, ip: string): Promise<ActionResult> {
  const s = await requireAdmin();
  try {
    await agent.firewall.unban(jail, ip);
  } catch (e: unknown) {
    return failed(s.userId, "fail2ban.unban", `${jail}:${ip}`, e);
  }
  writeAudit({ actorId: s.userId, action: "fail2ban.unban", target: `${jail}:${ip}`, result: "ok" });
  revalidatePath("/firewall");
  return { ok: true };
}

export async function unbanAllAction(jail: string): Promise<ActionResult> {
  const s = await requireAdmin();
  try {
    await agent.firewall.unbanAll(jail);
  } catch (e: unknown) {
    // Some of the bans may be gone already.
    revalidatePath("/firewall");
    return failed(s.userId, "fail2ban.unban_all", jail, e);
  }
  writeAudit({
    actorId: s.userId,
    action: "fail2ban.unban_all",
    target: jail,
    result: "ok",
  });
  revalidatePath("/firewall");
  return { ok: true };
}

export async function fetchFailedByIpAction(
  jail: string,
): Promise<{ ok: true; rows: FailedByIp[] } | { ok: false; error: string }> {
  await requireSession();
  try {
    return { ok: true, rows: await agent.firewall.failedByIp(jail) };
  } catch (e: unknown) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }
}

export async function enableUfwAction(): Promise<ActionResult> {
  const s = await requireAdmin();
  try {
    await agent.firewall.enable();
  } catch (e: unknown) {
    return failed(s.userId, "firewall.enable", undefined, e);
  }
  writeAudit({ actorId: s.userId, action: "firewall.enable", result: "ok" });
  revalidatePath("/firewall");
  return { ok: true };
}

export async function restartFail2banAction(): Promise<ActionResult> {
  const s = await requireAdmin();
  try {
    await agent.services.action("fail2ban", "restart");
  } catch (e: unknown) {
    return failed(s.userId, "fail2ban.restart", "service", e);
  }
  writeAudit({
    actorId: s.userId,
    action: "fail2ban.restart",
    target: "service",
    result: "ok",
  });
  revalidatePath("/firewall");
  return { ok: true };
}

// Toggle the fail2ban systemd unit on/off. Bind the current state into the
// form when rendering so the action knows which direction to flip.
//
// - active=true  -> stop + disable (operator wants the service OFF)
// - active=false -> enable + start (operator wants the service ON)
//
// Stopping pairs with disable so the next reboot doesn't silently bring
// fail2ban back; starting pairs with enable so it survives a reboot.
export async function toggleFail2banAction(currentlyActive: boolean): Promise<ActionResult> {
  const s = await requireAdmin();
  const target = currentlyActive ? "off" : "on";
  try {
    if (currentlyActive) {
      await agent.services.action("fail2ban", "stop");
      await agent.services.action("fail2ban", "disable");
    } else {
      await agent.services.action("fail2ban", "enable");
      await agent.services.action("fail2ban", "start");
    }
    writeAudit({
      actorId: s.userId,
      action: "fail2ban.toggle",
      target,
      result: "ok",
    });
  } catch (e: unknown) {
    // The first of the two steps may have gone through.
    revalidatePath("/firewall");
    return failed(s.userId, "fail2ban.toggle", target, e);
  }
  revalidatePath("/firewall");
  return { ok: true };
}
