"use server";

import { revalidatePath } from "next/cache";
import { redirect } from "next/navigation";
import {
  agent,
  AgentError,
  type CreateRecord,
  type ReconcileZoneResp,
  type UpdateRecord,
} from "@/lib/agent";
import { requireAdmin } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";
import { db } from "@/lib/db";
import { getServerAddress } from "@/lib/serverAddress";

export type ActionResult<T = undefined> =
  | { ok: true; data?: T }
  | { ok: false; error: string };

export async function deleteZoneAction(zone: string): Promise<void> {
  const s = await requireAdmin();
  await agent.dns.deleteZone(zone);
  db().prepare(`DELETE FROM domains WHERE name = ?`).run(zone);
  writeAudit({ actorId: s.userId, action: "dns.zones.delete", target: zone, result: "ok" });
  revalidatePath("/domains");
  revalidatePath(`/domains/${encodeURIComponent(zone)}`);
}

function isDuplicateZoneError(msg: string): boolean {
  return msg.includes("Duplicate entry") && msg.includes("name_index");
}

export async function enableZoneAction(
  _prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
  const s = await requireAdmin();
  const name = String(fd.get("zone") ?? "").trim().toLowerCase();
  if (!name) return { ok: false, error: "zone name required" };
  try {
    const addr = await getServerAddress();
    const seedIp = addr.ip ?? undefined;
    await agent.dns.createZone(name, seedIp);
    db().prepare(`INSERT OR IGNORE INTO domains (name) VALUES (?)`).run(name);
    writeAudit({ actorId: s.userId, action: "dns.zones.create", target: name, result: "ok" });
    revalidatePath("/domains");
    revalidatePath(`/domains/${encodeURIComponent(name)}`);
    return { ok: true };
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    if (isDuplicateZoneError(msg)) {
      db().prepare(`INSERT OR IGNORE INTO domains (name) VALUES (?)`).run(name);
      writeAudit({
        actorId: s.userId,
        action: "dns.zones.adopt",
        target: name,
        result: "ok",
        message: "fallback from enable: zone already in PowerDNS",
      });
      revalidatePath("/domains");
      revalidatePath(`/domains/${encodeURIComponent(name)}`);
      return { ok: true };
    }
    writeAudit({ actorId: s.userId, action: "dns.zones.create", target: name, result: "error", message: msg });
    return { ok: false, error: msg };
  }
}

export async function adoptZoneAction(zone: string): Promise<void> {
  const s = await requireAdmin();
  const name = zone.trim().toLowerCase();
  const zones = await agent.dns.listZones().catch(() => []);
  const exists = zones.some((z) => z.name === name);
  if (!exists) {
    writeAudit({
      actorId: s.userId,
      action: "dns.zones.adopt",
      target: name,
      result: "error",
      message: "zone not present in PowerDNS",
    });
    throw new Error("zone not present in PowerDNS");
  }
  db().prepare(`INSERT OR IGNORE INTO domains (name) VALUES (?)`).run(name);
  writeAudit({ actorId: s.userId, action: "dns.zones.adopt", target: name, result: "ok" });
  revalidatePath("/domains");
  revalidatePath(`/domains/${encodeURIComponent(name)}`);
}

export async function adoptZoneActionForm(
  _prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
  const name = String(fd.get("zone") ?? "").trim().toLowerCase();
  if (!name) return { ok: false, error: "zone name required" };
  try {
    await adoptZoneAction(name);
    return { ok: true };
  } catch (e: unknown) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }
}

export async function reconcileZoneAction(
  zone: string,
): Promise<ActionResult<ReconcileZoneResp>> {
  const s = await requireAdmin();
  try {
    const result = await agent.dns.reconcileZone(zone);
    writeAudit({
      actorId: s.userId,
      action: "dns.zones.reconcile",
      target: zone,
      result: "ok",
      message: `+${result.added.length} ~${result.updated.length} =${result.unchanged.length} extras:${result.non_standard_extras.length}`,
    });
    revalidatePath(`/domains/${encodeURIComponent(zone)}`);
    return { ok: true, data: result };
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "dns.zones.reconcile",
      target: zone,
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
}

export async function deleteDomainAction(zone: string): Promise<void> {
  const s = await requireAdmin();
  const target = zone.trim().toLowerCase();

  // The cascade wipes every site vhost under the zone. One of them being an
  // app's routing slot means the app must be unassigned first, otherwise its
  // unit keeps listening for a name that no longer routes anywhere.
  const boundApps = db()
    .prepare(
      `SELECT name, subdomain FROM apps
        WHERE subdomain = ? OR subdomain LIKE ? ESCAPE '\\'`,
    )
    .all(target, `%.${target.replace(/[%_]/g, "\\$&")}`) as {
    name: string;
    subdomain: string;
  }[];
  if (boundApps.length > 0) {
    const list = boundApps.map((a) => `${a.subdomain} (app ${a.name})`).join(", ");
    redirect(
      `/domains?error=${encodeURIComponent(
        `${target} still routes to an app: ${list}. Remove the domain on the app page first.`,
      )}`,
    );
  }

  // 1. Collect every nginx vhost under this zone so the cascade also wipes
  //    agent-only files (mail.<zone> ACME helpers, manually-deployed external
  //    vhosts, etc.) — not just rows in the panel `sites` table. Otherwise the
  //    /domains list keeps showing a "parent only" synthetic apex with an
  //    orphan "Remove from agent" row underneath after the cascade.
  const trackedSites = db()
    .prepare(
      `SELECT domain FROM sites WHERE domain = ? OR domain LIKE ? ESCAPE '\\'`,
    )
    .all(target, `%.${target.replace(/[%_]/g, "\\$&")}`) as { domain: string }[];

  const agentVhosts = await agent.sites.list().catch(() => []);
  const agentManaged = agentVhosts
    .filter((v) => v.domain === target || v.domain.endsWith(`.${target}`))
    // panel-host can't be deleted via /sites/:domain (the agent refuses); app
    // vhosts are managed via /apps and removing them here would orphan the
    // backing systemd unit. Mirror the UI's canRemove logic from page.tsx.
    .filter((v) => v.kind === "site" || v.kind === "external")
    .map((v) => v.domain);

  const allDomains = Array.from(
    new Set<string>([...trackedSites.map((r) => r.domain), ...agentManaged]),
  );

  let siteResult: "deleted" | "missing" | "error" = "missing";
  let sitesDeleted = 0;
  for (const domain of allDomains) {
    try {
      await agent.sites.delete(domain);
      siteResult = "deleted";
      sitesDeleted += 1;
    } catch (e) {
      if (e instanceof AgentError && e.status === 404) {
        // vhost wasn't on disk for some reason; treat as missing, keep going
        // so we still purge the SQLite row.
      } else {
        siteResult = "error";
        throw e;
      }
    }
  }

  // 2. Drop the PowerDNS zone — this wipes the zone itself plus every record
  //    inside it (apex A, www, mail, ns1, ns2, MX, SPF, DMARC, DKIM TXT, …),
  //    so we don't need to enumerate subdomain records separately.
  let dnsResult: "deleted" | "missing" | "error" = "missing";
  try {
    await agent.dns.deleteZone(target);
    dnsResult = "deleted";
  } catch (e) {
    if (e instanceof AgentError && e.status === 404) {
      dnsResult = "missing";
    } else {
      dnsResult = "error";
      throw e;
    }
  }

  // 3. Purge panel SQLite rows for the apex and any subdomain sites we just
  //    deleted on the agent side. Mailboxes, DKIM key files, /var/vmail data
  //    and any Let's Encrypt cert under /etc/vps-panel/certs/ are
  //    intentionally LEFT IN PLACE: the operator may re-add the domain
  //    later and they want the mailbox archive + reusable cert to survive.
  db()
    .prepare(
      `DELETE FROM sites WHERE domain = ? OR domain LIKE ? ESCAPE '\\'`,
    )
    .run(target, `%.${target.replace(/[%_]/g, "\\$&")}`);
  db()
    .prepare(
      `DELETE FROM redirects WHERE domain = ? OR domain LIKE ? ESCAPE '\\'`,
    )
    .run(target, `%.${target.replace(/[%_]/g, "\\$&")}`);
  db().prepare(`DELETE FROM domains WHERE name = ?`).run(target);

  writeAudit({
    actorId: s.userId,
    action: "domain.delete_cascade",
    target,
    result: "ok",
    message: `sites_deleted=${sitesDeleted} site=${siteResult} dns=${dnsResult}`,
  });
  revalidatePath("/domains");
  revalidatePath(`/domains/${encodeURIComponent(target)}`);
}

export async function signZoneAction(zone: string): Promise<void> {
  const s = await requireAdmin();
  await agent.dns.signZone(zone);
  db().prepare(`UPDATE domains SET dnssec = 1 WHERE name = ?`).run(zone);
  writeAudit({ actorId: s.userId, action: "dns.zones.sign", target: zone, result: "ok" });
  revalidatePath(`/domains/${encodeURIComponent(zone)}`);
}

export async function createRecordAction(
  zone: string,
  _prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
  const s = await requireAdmin();
  const body: CreateRecord = {
    name: String(fd.get("name") ?? "").trim(),
    type: String(fd.get("type") ?? "A").trim().toUpperCase(),
    content: String(fd.get("content") ?? "").trim(),
    ttl: Number(fd.get("ttl") ?? 3600),
    prio: Number(fd.get("prio") ?? 0),
  };
  try {
    await agent.dns.createRecord(zone, body);
    writeAudit({ actorId: s.userId, action: "dns.records.create", target: `${zone}/${body.name}/${body.type}`, request: body, result: "ok" });
    revalidatePath(`/domains/${encodeURIComponent(zone)}`);
    return { ok: true };
  } catch (e: unknown) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }
}

export async function updateRecordAction(
  zone: string,
  id: number,
  _prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
  const s = await requireAdmin();
  const rawPrio = fd.get("prio");
  const rawName = fd.get("name");
  const submittedType =
    fd.get("type") !== null ? String(fd.get("type")).trim().toUpperCase() : "";
  const originalType = String(fd.get("original_type") ?? "").trim().toUpperCase();
  // Only send a type the user actually changed. Re-sending an unchanged (or
  // browser-substituted) type is how an SOA edit once turned into an A record.
  const changedType =
    submittedType !== "" && submittedType !== originalType ? submittedType : undefined;
  const body: UpdateRecord = {
    content: fd.get("content") ? String(fd.get("content")) : undefined,
    ttl: fd.get("ttl") ? Number(fd.get("ttl")) : undefined,
    disabled: fd.get("disabled") === "on" ? true : fd.get("disabled") === "off" ? false : undefined,
    // Empty-string priority comes through as "" rather than null; treat that
    // as "leave unchanged" so non-MX records aren't forced back to 0 every
    // edit. Only send a numeric prio when the user actually typed one.
    prio:
      rawPrio !== null && String(rawPrio).trim() !== ""
        ? Number(rawPrio)
        : undefined,
    // Name "@" is valid (= apex) so we send "" through to the agent which
    // treats empty/"@" identically. Skip the field only when the form
    // didn't include it at all.
    name: rawName !== null ? String(rawName).trim() : undefined,
    type: changedType,
  };
  try {
    await agent.dns.updateRecord(id, body);
    writeAudit({ actorId: s.userId, action: "dns.records.update", target: `${zone}/#${id}`, request: body, result: "ok" });
    revalidatePath(`/domains/${encodeURIComponent(zone)}`);
    return { ok: true };
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({ actorId: s.userId, action: "dns.records.update", target: `${zone}/#${id}`, request: body, result: "error", message: msg });
    return { ok: false, error: msg };
  }
}

export async function deleteRecordAction(zone: string, id: number): Promise<void> {
  const s = await requireAdmin();
  await agent.dns.deleteRecord(id);
  writeAudit({ actorId: s.userId, action: "dns.records.delete", target: `${zone}/#${id}`, result: "ok" });
  revalidatePath(`/domains/${encodeURIComponent(zone)}`);
}
