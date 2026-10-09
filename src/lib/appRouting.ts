import "server-only";
import { agent } from "./agent";
import { db } from "./db";
import { writeAudit } from "./audit";
import { freeSlotsFor, listRoutingSlots, slotUnavailableReason } from "./appSlots";
import type { RoutingOutcome } from "@/server-actions/apps";

/**
 * Bind a panel app to a routing slot, or take it off its slot (`domain` null).
 * Shared by the /apps/<name> form and the MCP tool so panel.db is written the
 * same way on both paths: only after the agent has actually moved the unit.
 * Throws with an operator-readable message; callers decide how to surface it.
 */
export async function applyAppRouting(
  actorId: number | null,
  name: string,
  domain: string | null,
): Promise<RoutingOutcome> {
  const app = db()
    .prepare(`SELECT subdomain, port FROM apps WHERE name = ?`)
    .get(name) as { subdomain: string; port: number } | undefined;
  if (!app) {
    throw new Error(`no panel record for "${name}" - re-run Deploy to register it`);
  }

  let target: { domain: string; port: number } | null = null;
  if (domain !== null) {
    const d = domain.trim().toLowerCase();
    const slots = listRoutingSlots();
    const slot = freeSlotsFor(name, slots).find((s) => s.domain === d);
    if (!slot) {
      const taken = slots.find((s) => s.domain === d);
      throw new Error(
        taken
          ? `${d} is not available (${slotUnavailableReason(taken, name)})`
          : `${d} is not a port-mode site - create it on /domains first`,
      );
    }
    target = { domain: slot.domain, port: slot.port };
  }

  const request = {
    from: app.subdomain || null,
    to: target?.domain ?? null,
    port: target?.port ?? null,
  };
  let resp;
  try {
    resp = await agent.apps.setRouting(
      name,
      target
        ? { subdomain: target.domain, port: target.port }
        : { subdomain: null, port: null },
    );
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({ actorId, action: "apps.domain.set", target: name, request, result: "error", message: msg });
    throw e;
  }

  db()
    .prepare(`UPDATE apps SET subdomain = ?, port = ? WHERE name = ?`)
    .run(target?.domain ?? "", target?.port ?? 0, name);
  writeAudit({
    actorId,
    action: "apps.domain.set",
    target: name,
    request,
    result: "ok",
    message:
      resp.state === "assigned" && !resp.bound
        ? "assigned, but the app did not bind the port (stopped)"
        : undefined,
  });

  return {
    state: resp.state,
    // For an unassign, report the domain that just went dark.
    domain: target?.domain ?? app.subdomain,
    port: target?.port ?? app.port,
    bound: resp.bound,
    log: resp.log ?? [],
  };
}
