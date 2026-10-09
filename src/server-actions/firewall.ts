"use server";

import { revalidatePath } from "next/cache";
import { agent, type FailedByIp } from "@/lib/agent";
import { requireAdmin, requireSession } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";

export type ActionResult = { ok: true } | { ok: false; error: string };

export async function firewallPortAction(
  _prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
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
  try {
    if (intent === "allow") {
      await agent.firewall.allow(port, proto);
    } else {
      await agent.firewall.deny(port, proto);
    }
    writeAudit({
      actorId: s.userId,
      action: `firewall.${intent}`,
      target: `${port}/${proto}`,
      result: "ok",
    });
    revalidatePath("/firewall");
    return { ok: true };
  } catch (e: unknown) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }
}

export async function unbanIpAction(jail: string, ip: string): Promise<void> {
  const s = await requireAdmin();
  await agent.firewall.unban(jail, ip);
  writeAudit({ actorId: s.userId, action: "fail2ban.unban", target: `${jail}:${ip}`, result: "ok" });
  revalidatePath("/firewall");
}

export async function unbanAllAction(jail: string): Promise<void> {
  const s = await requireAdmin();
  await agent.firewall.unbanAll(jail);
  writeAudit({
    actorId: s.userId,
    action: "fail2ban.unban_all",
    target: jail,
    result: "ok",
  });
  revalidatePath("/firewall");
}

export async function fetchFailedByIpAction(jail: string): Promise<FailedByIp[]> {
  await requireSession();
  return agent.firewall.failedByIp(jail);
}

export async function enableUfwAction(): Promise<void> {
  const s = await requireAdmin();
  await agent.firewall.enable();
  writeAudit({ actorId: s.userId, action: "firewall.enable", result: "ok" });
  revalidatePath("/firewall");
}

export async function restartFail2banAction(): Promise<void> {
  const s = await requireAdmin();
  await agent.services.action("fail2ban", "restart");
  writeAudit({
    actorId: s.userId,
    action: "fail2ban.restart",
    target: "service",
    result: "ok",
  });
  revalidatePath("/firewall");
}

// Toggle the fail2ban systemd unit on/off. Bind the current state into the
// form when rendering so the action knows which direction to flip.
//
// - active=true  -> stop + disable (operator wants the service OFF)
// - active=false -> enable + start (operator wants the service ON)
//
// Stopping pairs with disable so the next reboot doesn't silently bring
// fail2ban back; starting pairs with enable so it survives a reboot.
export async function toggleFail2banAction(currentlyActive: boolean): Promise<void> {
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
    writeAudit({
      actorId: s.userId,
      action: "fail2ban.toggle",
      target,
      result: "error",
      message: e instanceof Error ? e.message : String(e),
    });
    throw e;
  }
  revalidatePath("/firewall");
}
