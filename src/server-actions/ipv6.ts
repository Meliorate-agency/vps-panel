"use server";

import { isIP } from "node:net";
import { revalidatePath } from "next/cache";
import { agent, AgentError, type DomainIpv6, type Ipv6NewDomains, type Ipv6Status } from "@/lib/agent";
import { requireAdmin } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";
import { agentErrorMessage, isSafeDomain } from "@/lib/redirects";

// IPv6 per domain: Settings > DNS & SSL > IPv6 and the IPv6 card on a
// domain's page. Every action returns its refusal instead of throwing it:
// Next replaces a thrown message in a production build.

export type Ipv6Result<T> =
  | { ok: true; data: T }
  | {
      ok: false;
      error: string;
      /** The agent on this server has no IPv6 routes yet (an older version). */
      unsupported?: boolean;
    };

/** An agent older than this feature answers its routes with a bare 404. */
function isUnsupported(e: unknown): boolean {
  return e instanceof AgentError && e.status === 404 && !e.body;
}

/** The agent's message without its "conflict: " style prefix. */
function ipv6Error(e: unknown, domain?: string): string {
  if (isUnsupported(e)) {
    return "The agent on this server does not have the IPv6 settings yet. Update the panel, then try again.";
  }
  if (domain && e instanceof AgentError && e.status === 404) {
    return `${domain} has no DNS zone on this server.`;
  }
  return agentErrorMessage(e).replace(/^(conflict|invalid input): /, "");
}

function failure<T>(e: unknown, domain?: string): Ipv6Result<T> {
  return { ok: false, error: ipv6Error(e, domain), ...(isUnsupported(e) ? { unsupported: true } : {}) };
}

/**
 * Settings > DNS & SSL > IPv6. Only the fields given are sent, so saving the
 * default for new domains does not touch the address. `address` "" =
 * automatic. A new address moves the records of every domain with IPv6 on.
 */
export async function saveIpv6SettingsAction(change: {
  address?: string;
  new_domains?: string;
}): Promise<Ipv6Result<Ipv6Status>> {
  const s = await requireAdmin();
  const body: { address?: string; new_domains?: Ipv6NewDomains } = {};
  if (change?.address !== undefined) {
    const address = String(change.address).trim().toLowerCase();
    if (address !== "" && isIP(address) !== 6) {
      return { ok: false, error: "Choose Automatic or one of the IPv6 addresses listed." };
    }
    body.address = address;
  }
  if (change?.new_domains !== undefined) {
    if (change.new_domains !== "on" && change.new_domains !== "off") {
      return { ok: false, error: "Choose on or off for new domains." };
    }
    body.new_domains = change.new_domains;
  }
  if (body.address === undefined && body.new_domains === undefined) {
    return { ok: false, error: "Nothing was changed." };
  }
  const what = [
    body.address !== undefined ? `address ${body.address || "automatic"}` : "",
    body.new_domains !== undefined ? `new domains ${body.new_domains}` : "",
  ]
    .filter(Boolean)
    .join(", ");
  try {
    const data = await agent.ipv6.setSettings(body);
    writeAudit({
      actorId: s.userId,
      action: "ipv6.settings.set",
      target: body.address !== undefined ? body.address || "automatic" : undefined,
      request: body,
      result: "ok",
      message: `${what}; in effect: ${data.address ?? "none"}`,
    });
    revalidatePath("/settings");
    // The Add domain checkbox follows the default; a new address moved the
    // records shown on the domains' pages.
    revalidatePath("/domains", "layout");
    return { ok: true, data };
  } catch (e: unknown) {
    const r = failure<Ipv6Status>(e);
    writeAudit({
      actorId: s.userId,
      action: "ipv6.settings.set",
      target: body.address !== undefined ? body.address || "automatic" : undefined,
      request: body,
      result: "error",
      message: r.ok ? undefined : r.error,
    });
    return r;
  }
}

/** One domain's IPv6: state, records and checks. Slow (DNS lookups), so the
 *  domain page asks for it after it is up. Reads only: nothing revalidated. */
export async function domainIpv6Action(domain: string): Promise<Ipv6Result<DomainIpv6>> {
  await requireAdmin();
  const name = String(domain ?? "").trim().toLowerCase();
  if (!isSafeDomain(name)) return { ok: false, error: "Not a valid domain name." };
  try {
    return { ok: true, data: await agent.dns.zoneIpv6(name) };
  } catch (e: unknown) {
    return failure(e, name);
  }
}

/** Switches one domain's IPv6 on (publishes its records) or off (removes
 *  them). Returns the domain's state as the agent has it afterwards. */
export async function setDomainIpv6Action(
  domain: string,
  enabled: boolean,
): Promise<Ipv6Result<DomainIpv6>> {
  const s = await requireAdmin();
  const name = String(domain ?? "").trim().toLowerCase();
  if (!isSafeDomain(name)) return { ok: false, error: "Not a valid domain name." };
  if (typeof enabled !== "boolean") return { ok: false, error: "Choose on or off." };
  try {
    const data = await agent.dns.setZoneIpv6(name, enabled);
    writeAudit({
      actorId: s.userId,
      action: "dns.zones.ipv6",
      target: name,
      result: "ok",
      message: data.enabled
        ? `on, ${data.address ?? "no address"}`
        : "off",
    });
    revalidatePath(`/domains/${encodeURIComponent(name)}`);
    revalidatePath("/domains");
    revalidatePath("/settings");
    return { ok: true, data };
  } catch (e: unknown) {
    const r = failure<DomainIpv6>(e, name);
    writeAudit({
      actorId: s.userId,
      action: "dns.zones.ipv6",
      target: name,
      result: "error",
      message: `${enabled ? "on" : "off"}: ${r.ok ? "" : r.error}`,
    });
    return r;
  }
}
