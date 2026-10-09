"use server";

import { redirect } from "next/navigation";
import { revalidatePath } from "next/cache";
import { requireAdmin } from "@/lib/auth";
import { getSetting, setSetting } from "@/lib/settings";
import { agent } from "@/lib/agent";
import { writeAudit } from "@/lib/audit";
import { SETUP_STEPS, type SetupStep } from "@/app/(setup)/setup/steps";

export type ActionResult = { ok: true } | { ok: false; error: string };
export type TaskResult =
  | { ok: true; data: { taskId: string } }
  | { ok: false; error: string };

const COMPLETE_KEY = "panel.setup_complete";
const STEP_KEY = "panel.setup_step";
const POLICY_KEY = "security.fail2ban_policy";

export type SetupState = {
  complete: boolean;
  step: SetupStep;
};

export async function getSetupState(): Promise<SetupState> {
  const complete = getSetting(COMPLETE_KEY, "false") === "true";
  const raw = getSetting(STEP_KEY, SETUP_STEPS[0]);
  const step = (SETUP_STEPS as readonly string[]).includes(raw)
    ? (raw as SetupStep)
    : SETUP_STEPS[0];
  return { complete, step };
}

export async function advanceSetupStepAction(next: SetupStep): Promise<ActionResult> {
  const s = await requireAdmin();
  if (!(SETUP_STEPS as readonly string[]).includes(next)) {
    return { ok: false, error: `unknown step: ${next}` };
  }
  setSetting(STEP_KEY, next);
  writeAudit({ actorId: s.userId, action: "setup.step", target: next, result: "ok" });
  return { ok: true };
}

export async function setFail2banPolicyAction(
  _prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
  const s = await requireAdmin();
  const bantime = String(fd.get("bantime") ?? "").trim();
  const findtime = String(fd.get("findtime") ?? "").trim();
  const maxretryRaw = String(fd.get("maxretry") ?? "").trim();
  const jails = fd.getAll("jails").map((v) => String(v).trim()).filter(Boolean);

  if (!/^[0-9]+[smhdw]?$/.test(bantime)) {
    return { ok: false, error: "bantime must be a positive integer with optional s/m/h/d/w suffix" };
  }
  if (!/^[0-9]+[smhdw]?$/.test(findtime)) {
    return { ok: false, error: "findtime must be a positive integer with optional s/m/h/d/w suffix" };
  }
  const maxretry = Number(maxretryRaw);
  if (!Number.isFinite(maxretry) || maxretry < 1 || maxretry > 1000) {
    return { ok: false, error: "maxretry must be 1..=1000" };
  }
  if (jails.length === 0) {
    return { ok: false, error: "select at least one jail" };
  }

  try {
    await agent.firewall.setFail2banPolicy({ bantime, findtime, maxretry, jails });
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "setup.fail2ban-policy",
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }

  setSetting(POLICY_KEY, JSON.stringify({ bantime, findtime, maxretry, jails }));
  writeAudit({
    actorId: s.userId,
    action: "setup.fail2ban-policy",
    result: "ok",
    target: jails.join(","),
  });
  revalidatePath("/setup");
  // Also surface the updated policy on the firewall page, which now has its
  // own tweaker form using this same action.
  revalidatePath("/firewall");
  return { ok: true };
}

export async function getFail2banPolicy(): Promise<{
  bantime: string;
  findtime: string;
  maxretry: number;
  jails: string[];
}> {
  const raw = getSetting(POLICY_KEY, "");
  if (!raw) {
    return {
      bantime: "1h",
      findtime: "10m",
      maxretry: 5,
      jails: ["sshd", "postfix-sasl", "dovecot"],
    };
  }
  try {
    const parsed = JSON.parse(raw);
    return {
      bantime: String(parsed.bantime ?? "1h"),
      findtime: String(parsed.findtime ?? "10m"),
      maxretry: Number(parsed.maxretry ?? 5),
      jails: Array.isArray(parsed.jails) ? parsed.jails.map(String) : [],
    };
  } catch {
    return {
      bantime: "1h",
      findtime: "10m",
      maxretry: 5,
      jails: ["sshd"],
    };
  }
}

export async function triggerAptUpgradeAction(): Promise<TaskResult> {
  const s = await requireAdmin();
  try {
    const accepted = await agent.system.aptUpgrade();
    writeAudit({
      actorId: s.userId,
      action: "setup.apt-upgrade",
      target: accepted.task_id,
      result: "ok",
    });
    return { ok: true, data: { taskId: accepted.task_id } };
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "setup.apt-upgrade",
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
}

export async function completeSetupAction(): Promise<void> {
  const s = await requireAdmin();
  setSetting(COMPLETE_KEY, "true");
  writeAudit({ actorId: s.userId, action: "setup.complete", result: "ok" });
  redirect("/dashboard");
}
