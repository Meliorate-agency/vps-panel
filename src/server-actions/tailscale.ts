"use server";

import { revalidatePath } from "next/cache";
import { agent, type TailscaleOptions, type TailscaleStatus } from "@/lib/agent";
import { requireSession } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";
import { agentErrorMessage } from "@/lib/redirects";

export type TailscaleResult<T> = { ok: true; data: T } | { ok: false; error: string };
export type TailscaleStepsResult = TailscaleResult<{ steps: string[] }>;

const NOT_ADMIN = "Only admins can manage Tailscale.";
/** Mirrors agent/src/handlers/tailscale.rs::check_hostname. */
const NAME = /^[A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?$/;

/**
 * Admin session, or null. Every action here is admin-only, reads included:
 * the status carries the login link, and whoever opens it can put this
 * server in their own tailnet.
 */
async function admin() {
  const s = await requireSession();
  return s.role === "admin" ? s : null;
}

function agentError(e: unknown): string {
  return agentErrorMessage(e).replace(/^(conflict|invalid input): /, "");
}

function cleanOptions(input: TailscaleOptions): TailscaleOptions | string {
  const hostname = String(input?.hostname ?? "").trim();
  if (hostname && !NAME.test(hostname)) {
    return "The name on the tailnet takes letters, digits and hyphens (no dots or spaces), 63 characters at most.";
  }
  return {
    hostname,
    ssh: input.ssh === true,
    accept_routes: input.accept_routes === true,
    accept_dns: input.accept_dns === true,
    exit_node: input.exit_node === true,
  };
}

function describe(o: TailscaleOptions): string {
  return [
    `name=${o.hostname || "(server hostname)"}`,
    `ssh=${o.ssh}`,
    `exit_node=${o.exit_node}`,
    `accept_routes=${o.accept_routes}`,
    `accept_dns=${o.accept_dns}`,
  ].join(" ");
}

export async function getTailscaleStatusAction(): Promise<TailscaleResult<TailscaleStatus>> {
  if (!(await admin())) return { ok: false, error: NOT_ADMIN };
  try {
    return { ok: true, data: await agent.tailscale.status() };
  } catch (e) {
    return { ok: false, error: agentError(e) };
  }
}

/** Starts `tailscale up` (agent task `tailscale:up`): with a link someone
 *  opens within 15 minutes, or with an auth key. The key is never logged. */
export async function tailscaleLoginAction(
  input: TailscaleOptions & { authKey: string },
): Promise<TailscaleResult<{ taskId: string }>> {
  const s = await admin();
  if (!s) return { ok: false, error: NOT_ADMIN };
  const o = cleanOptions(input);
  if (typeof o === "string") return { ok: false, error: o };
  const authKey = String(input.authKey ?? "").trim();
  const target = authKey ? "auth key" : "login link";
  try {
    const r = await agent.tailscale.login({ ...o, auth_key: authKey });
    writeAudit({ actorId: s.userId, action: "tailscale.login", target, result: "ok", message: describe(o) });
    revalidatePath("/apps");
    return { ok: true, data: { taskId: r.task_id } };
  } catch (e) {
    const msg = agentError(e);
    writeAudit({ actorId: s.userId, action: "tailscale.login", target, result: "error", message: msg });
    return { ok: false, error: msg };
  }
}

/** `tailscale set` on a logged-in server. Settings the panel does not show
 *  (routes added by hand, tags) stay as they are. */
export async function saveTailscaleSettingsAction(
  input: TailscaleOptions,
): Promise<TailscaleStepsResult> {
  const s = await admin();
  if (!s) return { ok: false, error: NOT_ADMIN };
  const o = cleanOptions(input);
  if (typeof o === "string") return { ok: false, error: o };
  try {
    const r = await agent.tailscale.saveSettings(o);
    writeAudit({ actorId: s.userId, action: "tailscale.settings", result: "ok", message: describe(o) });
    revalidatePath("/apps");
    return { ok: true, data: { steps: r.steps } };
  } catch (e) {
    const msg = agentError(e);
    writeAudit({ actorId: s.userId, action: "tailscale.settings", result: "error", message: msg });
    return { ok: false, error: msg };
  }
}

async function simple(kind: "connect" | "disconnect" | "logout"): Promise<TailscaleStepsResult> {
  const s = await admin();
  if (!s) return { ok: false, error: NOT_ADMIN };
  try {
    const r = await agent.tailscale[kind]();
    writeAudit({ actorId: s.userId, action: `tailscale.${kind}`, result: "ok" });
    revalidatePath("/apps");
    return { ok: true, data: { steps: r.steps } };
  } catch (e) {
    const msg = agentError(e);
    writeAudit({ actorId: s.userId, action: `tailscale.${kind}`, result: "error", message: msg });
    return { ok: false, error: msg };
  }
}

/** Starts tailscaled if it is stopped; brings a disconnected server back up
 *  with its settings unchanged. */
export async function tailscaleConnectAction(): Promise<TailscaleStepsResult> {
  return simple("connect");
}

/** `tailscale down`: off the tailnet, still logged in. */
export async function tailscaleDisconnectAction(): Promise<TailscaleStepsResult> {
  return simple("disconnect");
}

/** `tailscale logout`, which also cancels a login still waiting. */
export async function tailscaleLogoutAction(): Promise<TailscaleStepsResult> {
  return simple("logout");
}
