// NOT "use server": these helpers are called only server-to-server (sites.ts
// createSiteAction / panelHost.ts, both behind requireAdmin). Marking the file
// "use server" would publish ensureDnsZone()/findParentManagedZone() as
// POST-invokable action endpoints with a caller-supplied actorId and no auth
// gate of their own. `import "server-only"` keeps them server-side and out of
// any client bundle without exposing them as actions.
import "server-only";

import { agent } from "@/lib/agent";
import { writeAudit } from "@/lib/audit";
import { db } from "@/lib/db";
import { getServerAddress } from "@/lib/serverAddress";

/**
 * Walk up the labels of `domain` and return the first one that's already a
 * managed PowerDNS zone, or null if none match. E.g. for
 * `mail.example.com` and zones `["example.com", ...]` returns
 * `"example.com"`. Used by ensureDnsZone() to avoid creating a spurious
 * child zone when an A record in the parent is what the user actually wants.
 *
 * Client-side forms (AddDomainForm.tsx, PanelHostForm.tsx) duplicate this
 * logic because "use server" modules cannot be imported into client
 * components.
 */
export async function findParentManagedZone(
  domain: string,
  zoneNames: Set<string>,
): Promise<string | null> {
  const labels = domain.split(".");
  for (let i = 1; i < labels.length; i++) {
    const candidate = labels.slice(i).join(".");
    if (zoneNames.has(candidate)) return candidate;
  }
  return null;
}

/**
 * Ensure DNS exists for `domain`: either as a managed PowerDNS zone, or as
 * an A record inside an already-managed parent zone. Idempotent. Never
 * throws, so the caller's main operation (site create, panel host rewrite) is
 * not rolled back. Returns a sentence for the operator when something was
 * not done (null when all is well); it used to go only to the audit log, so
 * "Site created" hid a failed zone.
 */
export async function ensureDnsZone(actorId: number, domain: string): Promise<string | null> {
  try {
    const zones = await agent.dns.listZones().catch(() => []);
    const zoneNames = new Set(zones.map((z) => z.name));

    // Already a top-level managed zone — just track in panel.db.
    if (zoneNames.has(domain)) {
      db().prepare(`INSERT OR IGNORE INTO domains (name) VALUES (?)`).run(domain);
      writeAudit({
        actorId,
        action: "dns.zones.create",
        target: domain,
        result: "ok",
        message: "already existed",
      });
      return null;
    }

    // Subdomain of a managed parent → add an A record in the parent zone
    // instead of creating a spurious child zone (which is what was happening
    // before this guard). Idempotent: skips the insert if the record already
    // exists with the same content.
    const parent = await findParentManagedZone(domain, zoneNames);
    if (parent) {
      const label = domain.slice(0, domain.length - parent.length - 1);
      const addr = await getServerAddress();
      const seedIp = addr.ip ?? undefined;
      let recordCreated = false;
      let warning: string | null = null;
      if (seedIp) {
        const existing = await agent.dns.listRecords(parent).catch(() => []);
        const fqdn = `${label}.${parent}`;
        const alreadyHas = existing.some(
          (r) => r.name === fqdn && r.type === "A" && r.content === seedIp,
        );
        // A name already pointed at another host keeps pointing there: adding
        // ours next to it made DNS round-robin between the two servers.
        const elsewhere = existing.filter(
          (r) => r.name === fqdn && (r.type === "A" || r.type === "CNAME") && r.content !== seedIp,
        );
        if (!alreadyHas && elsewhere.length > 0) {
          warning = `${fqdn} already points at ${elsewhere.map((r) => r.content).join(", ")}, so no record for this server was added. Change it on the ${parent} zone page if this server should serve it.`;
        } else if (!alreadyHas) {
          await agent.dns.createRecord(parent, {
            name: label,
            type: "A",
            content: seedIp,
            ttl: 3600,
          });
          recordCreated = true;
        }
      }
      writeAudit({
        actorId,
        action: "dns.records.create",
        target: `${parent}/${label}/A`,
        result: "ok",
        message:
          warning ??
          (recordCreated
            ? `subdomain of managed zone '${parent}': added A record in parent instead of creating new zone`
            : `subdomain of managed zone '${parent}': A record already present in parent`),
      });
      return warning;
    }

    // No parent zone — create a fresh top-level zone (original behavior).
    const addr = await getServerAddress();
    const seedIp = addr.ip ?? undefined;
    await agent.dns.createZone(domain, seedIp);
    db().prepare(`INSERT OR IGNORE INTO domains (name) VALUES (?)`).run(domain);
    writeAudit({
      actorId,
      action: "dns.zones.create",
      target: domain,
      result: "ok",
    });
    return null;
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId,
      action: "dns.zones.create",
      target: domain,
      result: "error",
      message: msg,
    });
    return `DNS for ${domain} was not set up: ${msg}`;
  }
}
