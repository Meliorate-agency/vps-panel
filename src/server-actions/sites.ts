"use server";

import { revalidatePath } from "next/cache";
import { redirect } from "next/navigation";
import { agent, AgentError, type SiteMode } from "@/lib/agent";
import { requireAdmin } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";
import { config } from "@/lib/config";
import { db } from "@/lib/db";
import { pollSslTaskAndReconcile } from "@/lib/sslReconciler";
import type { SslTaskResult } from "@/lib/sslTaskResult";
import { ensureDnsZone } from "./_dns_ensure";

export type ActionResult<T = undefined> =
  | { ok: true; data?: T; taskId?: string; warnings?: string[] }
  | { ok: false; error: string }
  | { ok: false; error: string; conflict: "agent_only"; domain: string };

const FQDN_RE = /^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)+$/;
const FOLDER_RE = /^\/[A-Za-z0-9._/-]+$/;

type ResolvedRoute =
  | {
      mode: SiteMode;
      upstreamPort: number | null;
      folderPath: string | null;
    }
  | { mode: "dns_only"; upstreamPort: null; folderPath: null };

function resolveRoute(
  domain: string,
  mode: string,
  rawPort: FormDataEntryValue | null,
  rawFolder: FormDataEntryValue | null,
): ResolvedRoute | { error: string } {
  if (mode === "port") {
    const port = Number(rawPort ?? 0);
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
      return { error: "upstream_port must be 1..65535" };
    }
    return { mode: "port", upstreamPort: port, folderPath: null };
  }
  if (mode === "folder") {
    const folder = String(rawFolder ?? "").trim() || `/var/www/${domain}`;
    if (!FOLDER_RE.test(folder) || folder.split("/").includes("..") || folder.length > 256) {
      return { error: "invalid folder_path" };
    }
    // Mirrors the agent's is_safe_folder_path: the folder is chowned to
    // www-data, so only a folder of its own under /var/www or /srv/drive.
    if (!/^\/(var\/www|srv\/drive)\/[^/]/.test(folder) || folder.startsWith("/var/www/vps-panel-acme")) {
      return { error: "a site folder must be inside /var/www/ or /srv/drive/" };
    }
    return { mode: "folder", upstreamPort: null, folderPath: folder };
  }
  if (mode === "dns_only") {
    return { mode: "dns_only", upstreamPort: null, folderPath: null };
  }
  return { error: "mode must be 'port', 'folder', or 'dns_only'" };
}

type ParsedForm =
  | {
      ok: true;
      domain: string;
      route: ResolvedRoute;
      forceWwwRedirect: boolean;
      issueSsl: boolean;
      createDnsZone: boolean;
      configureMail: boolean;
    }
  | { ok: false; result: ActionResult };

function parseAndValidate(fd: FormData): ParsedForm {
  const domain = String(fd.get("domain") ?? "").trim().toLowerCase();
  const mode = String(fd.get("mode") ?? "port");
  const issueSsl = fd.get("issue_ssl") === "on";
  let forceWwwRedirect = fd.get("force_www_redirect") === "on";
  const createDnsZone = fd.get("create_dns_zone") === "on";
  let configureMail = fd.get("configure_mail") === "on";

  if (!FQDN_RE.test(domain)) {
    return { ok: false, result: { ok: false, error: "invalid domain" } };
  }
  if (forceWwwRedirect && domain.startsWith("www.")) {
    return {
      ok: false,
      result: { ok: false, error: "cannot force www redirect on a www subdomain" },
    };
  }
  const isSubdomain = domain.split(".").filter((l) => l.length > 0).length >= 3;
  if (isSubdomain) {
    forceWwwRedirect = false;
    configureMail = false;
  }
  const route = resolveRoute(domain, mode, fd.get("upstream_port"), fd.get("folder_path"));
  if ("error" in route) {
    return { ok: false, result: { ok: false, error: route.error } };
  }
  return {
    ok: true,
    domain,
    route,
    forceWwwRedirect,
    issueSsl,
    createDnsZone,
    configureMail,
  };
}

/** Name of the panel app bound to this routing slot, if any. */
function boundAppFor(domain: string): string | null {
  const row = db()
    .prepare(`SELECT name FROM apps WHERE subdomain = ?`)
    .get(domain) as { name: string } | undefined;
  return row?.name ?? null;
}

function checkAppSubdomainCollision(domain: string): ActionResult | null {
  const conflictApp = db()
    .prepare(`SELECT name FROM apps WHERE subdomain = ?`)
    .get(domain) as { name: string } | undefined;
  if (conflictApp) {
    return { ok: false, error: `domain is in use by app "${conflictApp.name}"` };
  }
  return null;
}

type SiteRoute = {
  mode: SiteMode;
  upstreamPort: number | null;
  folderPath: string | null;
};

async function doCreate(
  actorId: number,
  domain: string,
  route: SiteRoute,
  forceWwwRedirect: boolean,
  issueSsl: boolean,
  auditAction: "sites.create" | "sites.replace",
): Promise<ActionResult> {
  try {
    const resp = await agent.sites.create({
      domain,
      mode: route.mode,
      upstream_port: route.upstreamPort,
      folder_path: route.folderPath,
      force_www_redirect: forceWwwRedirect,
      issue_ssl: issueSsl,
    });
    db()
      .prepare(
        `INSERT INTO sites (domain, mode, upstream_port, folder_path, force_www_redirect, ssl_requested, ssl_issued_at, last_ssl_error)
         VALUES (?, ?, ?, ?, ?, ?, NULL, NULL)`,
      )
      .run(
        domain,
        route.mode,
        route.upstreamPort,
        route.folderPath,
        forceWwwRedirect ? 1 : 0,
        issueSsl ? 1 : 0,
      );

    // No UFW rule for a port-mode site's upstream: nginx reaches it on
    // 127.0.0.1, which UFW does not filter. Opening the port (as this used
    // to) only made the app reachable on <ip>:<port>, past nginx and HTTPS.

    if (issueSsl && resp.task_id) {
      // Fire-and-forget: poll the agent's task until it settles so the
      // /domains row flips out of "issuing..." to either a real cert badge
      // or a Retry button with the actual failure reason.
      pollSslTaskAndReconcile(domain, resp.task_id, actorId);
    }

    writeAudit({
      actorId,
      action: auditAction,
      target: domain,
      request: {
        mode: route.mode,
        upstream_port: route.upstreamPort,
        folder_path: route.folderPath,
        force_www_redirect: forceWwwRedirect,
        issue_ssl: issueSsl,
      },
      result: "ok",
      message: resp.task_id ? `ssl task ${resp.task_id}` : undefined,
    });
    revalidatePath("/domains");
    revalidatePath("/firewall");
    return { ok: true, taskId: resp.task_id ?? undefined };
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId,
      action: auditAction,
      target: domain,
      result: "error",
      message: msg,
    });
    if (e instanceof AgentError && e.status === 409) {
      return { ok: false, error: msg, conflict: "agent_only", domain };
    }
    return { ok: false, error: msg };
  }
}

/** Like ensureDnsZone: never throws, returns a sentence when mail setup failed. */
async function ensureMailDomain(
  actorId: number,
  domain: string,
  opts: { issueSsl: boolean },
): Promise<string | null> {
  try {
    const resp = await agent.mail.createDomain(domain, { issueSsl: opts.issueSsl });
    writeAudit({
      actorId,
      action: "mail.domains.create",
      target: domain,
      result: "ok",
      message:
        [resp.note, resp.ssl_task_id ? `mail ssl task ${resp.ssl_task_id}` : ""]
          .filter(Boolean)
          .join(" ") || undefined,
    });
    return null;
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId,
      action: "mail.domains.create",
      target: domain,
      result: "error",
      message: msg,
    });
    return `Mail for ${domain} was not set up: ${msg}`;
  }
}

export async function createSiteAction(
  _prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
  const s = await requireAdmin();
  const parsed = parseAndValidate(fd);
  if (!parsed.ok) return parsed.result;
  const { domain, route, forceWwwRedirect, issueSsl, createDnsZone, configureMail } = parsed;

  // The panel's own hostname is served by the panel. A site with that name
  // would be loaded first by nginx and hide the panel.
  if (domain === config.panelHost.toLowerCase()) {
    return {
      ok: false,
      error: `${domain} is the panel's own hostname and is already served by the panel.`,
    };
  }

  const collision = checkAppSubdomainCollision(domain);
  if (collision) return collision;

  const warnings: string[] = [];
  if (createDnsZone) {
    const w = await ensureDnsZone(s.userId, domain);
    if (w) warnings.push(w);
  }

  // Mail BEFORE the site. The site's SSL task builds its SAN list from the
  // agent's view of mail tenants, so the tenant row has to exist by then for
  // the cert to carry mail.<domain>. When the site issues SSL itself the
  // agent is told not to order a separate mail cert (the site cert covers
  // it); a DNS-only or no-SSL site gets a standalone mail.<domain> cert.
  if (configureMail) {
    const siteWillIssueSsl = route.mode !== "dns_only" && issueSsl;
    const w = await ensureMailDomain(s.userId, domain, { issueSsl: !siteWillIssueSsl });
    if (w) warnings.push(w);
  }

  let siteResult: ActionResult = { ok: true };
  if (route.mode !== "dns_only") {
    const existingSite = db()
      .prepare(`SELECT 1 FROM sites WHERE domain = ?`)
      .get(domain);
    if (existingSite) {
      siteResult = { ok: false, error: "site already exists" };
    } else {
      siteResult = await doCreate(
        s.userId,
        domain,
        { mode: route.mode, upstreamPort: route.upstreamPort, folderPath: route.folderPath },
        forceWwwRedirect,
        issueSsl,
        "sites.create",
      );
    }
  }

  revalidatePath("/domains");
  if (siteResult.ok && warnings.length > 0) {
    return { ...siteResult, warnings };
  }
  return siteResult;
}

export async function replaceSiteAction(
  _prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
  const s = await requireAdmin();
  const parsed = parseAndValidate(fd);
  if (!parsed.ok) return parsed.result;
  const { domain, route, forceWwwRedirect, issueSsl } = parsed;
  if (route.mode === "dns_only") {
    return { ok: false, error: "cannot replace a site with a DNS-only entry" };
  }

  const collision = checkAppSubdomainCollision(domain);
  if (collision) return collision;

  try {
    await agent.sites.delete(domain);
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "sites.replace",
      target: domain,
      result: "error",
      message: `delete failed: ${msg}`,
    });
    return { ok: false, error: `could not clear agent state: ${msg}` };
  }

  // If a stale panel row exists, clear it so the INSERT in doCreate doesn't collide.
  db().prepare(`DELETE FROM sites WHERE domain = ?`).run(domain);

  return doCreate(
    s.userId,
    domain,
    { mode: route.mode, upstreamPort: route.upstreamPort, folderPath: route.folderPath },
    forceWwwRedirect,
    issueSsl,
    "sites.replace",
  );
}

export async function adoptSiteAction(
  _prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
  const s = await requireAdmin();
  const parsed = parseAndValidate(fd);
  if (!parsed.ok) return parsed.result;
  const { domain, route, forceWwwRedirect, issueSsl } = parsed;
  if (route.mode === "dns_only") {
    return { ok: false, error: "cannot adopt a DNS-only entry" };
  }

  const existingSite = db()
    .prepare(`SELECT 1 FROM sites WHERE domain = ?`)
    .get(domain);
  if (existingSite) return { ok: false, error: "site already exists" };

  const collision = checkAppSubdomainCollision(domain);
  if (collision) return collision;

  // Adopt only registers an existing vhost; it starts no issuance. Marking
  // SSL requested without a cert left the row on "issuing..." forever, so the
  // flag follows the cert that is really there (Install SSL covers the rest).
  const certNow = await agent.sites.status(domain).catch(() => null);
  const hasCert = !!certNow?.cert_exists && (certNow.days_remaining ?? -1) >= 0;
  const sslRequested = issueSsl && hasCert;

  try {
    db()
      .prepare(
        `INSERT INTO sites (domain, mode, upstream_port, folder_path, force_www_redirect, ssl_requested, ssl_issued_at, last_ssl_error)
         VALUES (?, ?, ?, ?, ?, ?, ${sslRequested ? "datetime('now')" : "NULL"}, NULL)`,
      )
      .run(
        domain,
        route.mode,
        route.upstreamPort,
        route.folderPath,
        forceWwwRedirect ? 1 : 0,
        sslRequested ? 1 : 0,
      );
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "sites.adopt",
      target: domain,
      result: "error",
      message: msg,
    });
    if (msg.includes("UNIQUE") || msg.includes("constraint")) {
      return { ok: false, error: "site already exists" };
    }
    return { ok: false, error: msg };
  }

  writeAudit({
    actorId: s.userId,
    action: "sites.adopt",
    target: domain,
    request: {
      mode: route.mode,
      upstream_port: route.upstreamPort,
      folder_path: route.folderPath,
      force_www_redirect: forceWwwRedirect,
      issue_ssl: issueSsl,
    },
    result: "ok",
  });
  revalidatePath("/domains");
  revalidatePath("/firewall");
  return { ok: true };
}

export async function siteFormDispatchAction(
  prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
  const intent = String(fd.get("intent") ?? "create");
  if (intent === "replace") return replaceSiteAction(prev, fd);
  if (intent === "adopt") return adoptSiteAction(prev, fd);
  return createSiteAction(prev, fd);
}

export async function removeAgentOnlySiteAction(domain: string): Promise<void> {
  const s = await requireAdmin();
  await agent.sites.delete(domain);
  writeAudit({
    actorId: s.userId,
    action: "sites.remove_agent_only",
    target: domain,
    result: "ok",
  });
  revalidatePath("/domains");
}

export async function deleteSiteAction(domain: string): Promise<void> {
  const s = await requireAdmin();
  // Removing a bound slot would leave the app's unit listening for a vhost
  // that no longer exists. Unassign it on the app page first.
  const boundApp = boundAppFor(domain);
  if (boundApp) {
    redirect(
      `/domains?error=${encodeURIComponent(
        `${domain} is bound to app "${boundApp}" - remove the domain on /apps/${boundApp} first.`,
      )}`,
    );
  }
  await agent.sites.delete(domain);
  db().prepare(`DELETE FROM sites WHERE domain = ?`).run(domain);
  // The agent already dropped the vhost + its redirects include; mirror that
  // in panel.db so /redirects does not show rules nothing serves.
  db().prepare(`DELETE FROM redirects WHERE domain = ?`).run(domain);
  writeAudit({
    actorId: s.userId,
    action: "sites.delete",
    target: domain,
    result: "ok",
  });
  revalidatePath("/domains");
  revalidatePath(`/domains/${encodeURIComponent(domain)}`);
}

export async function updateSiteRouteAction(
  domain: string,
  _prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
  const s = await requireAdmin();
  const mode = String(fd.get("mode") ?? "port");
  const issueSsl = fd.get("issue_ssl") === "on";
  const forceWwwRedirect = fd.get("force_www_redirect") === "on";
  if (forceWwwRedirect && domain.startsWith("www.")) {
    return { ok: false, error: "cannot force www redirect on a www subdomain" };
  }
  const route = resolveRoute(domain, mode, fd.get("upstream_port"), fd.get("folder_path"));
  if ("error" in route) return { ok: false, error: route.error };
  if (route.mode === "dns_only") {
    return { ok: false, error: "cannot update an existing site to dns_only" };
  }

  const conn = db();
  const row = conn
    .prepare(`SELECT upstream_port FROM sites WHERE domain = ?`)
    .get(domain) as { upstream_port: number | null } | undefined;
  if (!row) return { ok: false, error: "site not found" };

  // The bound app's unit carries this port; changing it here would desync
  // the two. Moving the app (its Domain form) is the supported path.
  const boundApp = boundAppFor(domain);
  if (
    boundApp &&
    (route.mode !== "port" || route.upstreamPort !== row.upstream_port)
  ) {
    return {
      ok: false,
      error: `${domain} is bound to app "${boundApp}" on port ${row.upstream_port} - change the app's domain on /apps/${boundApp} first, or keep the port.`,
    };
  }

  let sslTaskId: string | null = null;
  try {
    const resp = await agent.sites.update(domain, {
      mode: route.mode,
      upstream_port: route.upstreamPort,
      folder_path: route.folderPath,
      force_www_redirect: forceWwwRedirect,
      issue_ssl: issueSsl,
    });
    sslTaskId = resp.task_id ?? null;
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "sites.update_route",
      target: domain,
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
  conn
    .prepare(
      `UPDATE sites
          SET mode = ?, upstream_port = ?, folder_path = ?,
              force_www_redirect = ?, ssl_requested = ?
        WHERE domain = ?`,
    )
    .run(
      route.mode,
      route.upstreamPort,
      route.folderPath,
      forceWwwRedirect ? 1 : 0,
      issueSsl ? 1 : 0,
      domain,
    );
  // The agent started an SSL task: follow it like create does, so the row
  // flips to valid or to "failed + Retry" instead of "issuing..." forever.
  if (sslTaskId) {
    conn.prepare(`UPDATE sites SET last_ssl_error = NULL WHERE domain = ?`).run(domain);
    pollSslTaskAndReconcile(domain, sslTaskId, s.userId);
  }
  writeAudit({
    actorId: s.userId,
    action: "sites.update_route",
    target: domain,
    request: {
      mode: route.mode,
      upstream_port: route.upstreamPort,
      folder_path: route.folderPath,
      force_www_redirect: forceWwwRedirect,
      issue_ssl: issueSsl,
    },
    result: "ok",
  });
  revalidatePath("/domains");
  revalidatePath(`/domains/${encodeURIComponent(domain)}`);
  return { ok: true };
}

export async function renewSslAction(domain: string): Promise<void> {
  const s = await requireAdmin();
  try {
    await agent.ssl.renew(domain);
    db()
      .prepare(`UPDATE sites SET last_ssl_error = NULL WHERE domain = ?`)
      .run(domain);
    writeAudit({
      actorId: s.userId,
      action: "sites.ssl_renew",
      target: domain,
      result: "ok",
    });
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "sites.ssl_renew",
      target: domain,
      result: "error",
      message: msg,
    });
    throw new Error(msg);
  }
  revalidatePath("/domains");
  revalidatePath(`/domains/${encodeURIComponent(domain)}`);
}

/**
 * Renew for the Domains list: same agent call as renewSslAction but hands the
 * task id back so <SslTaskButton> can follow the order and refresh the row
 * when the new cert lands, instead of the operator reloading blind.
 */
export async function renewSslTaskAction(domain: string): Promise<SslTaskResult> {
  const s = await requireAdmin();
  try {
    const resp = await agent.ssl.renew(domain);
    db()
      .prepare(`UPDATE sites SET last_ssl_error = NULL WHERE domain = ?`)
      .run(domain);
    writeAudit({
      actorId: s.userId,
      action: "sites.ssl_renew",
      target: domain,
      result: "ok",
      message: `task ${resp.task_id}`,
    });
    return { ok: true, taskId: resp.task_id };
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "sites.ssl_renew",
      target: domain,
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
}

export async function retrySslAction(domain: string): Promise<void> {
  const s = await requireAdmin();
  const conn = db();
  const row = conn
    .prepare(
      `SELECT mode, upstream_port, folder_path, force_www_redirect
         FROM sites WHERE domain = ?`,
    )
    .get(domain) as
    | {
        mode: string;
        upstream_port: number | null;
        folder_path: string | null;
        force_www_redirect: number;
      }
    | undefined;
  if (!row) throw new Error("site not found");

  try {
    const resp = await agent.sites.update(domain, {
      mode: row.mode as SiteMode,
      upstream_port: row.upstream_port,
      folder_path: row.folder_path,
      force_www_redirect: row.force_www_redirect === 1,
      issue_ssl: true,
    });
    conn
      .prepare(
        `UPDATE sites SET ssl_requested = 1, last_ssl_error = NULL WHERE domain = ?`,
      )
      .run(domain);
    if (resp.task_id) {
      pollSslTaskAndReconcile(domain, resp.task_id, s.userId);
    }
    writeAudit({
      actorId: s.userId,
      action: "sites.ssl_retry",
      target: domain,
      result: "ok",
      message: resp.task_id ? `ssl task ${resp.task_id}` : undefined,
    });
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    conn
      .prepare(`UPDATE sites SET last_ssl_error = ? WHERE domain = ?`)
      .run(msg, domain);
    writeAudit({
      actorId: s.userId,
      action: "sites.ssl_retry",
      target: domain,
      result: "error",
      message: msg,
    });
  }
  revalidatePath("/domains");
  revalidatePath(`/domains/${encodeURIComponent(domain)}`);
}

// One-click "Install SSL" for a DNS-only row (apex zone or A-record-in-managed-
// parent subdomain). Creates a default static-folder site at /var/www/<domain>
// with issue_ssl=true; the agent spawns the SSL task and pollSslTaskAndReconcile
// flips ssl_issued_at / last_ssl_error once it settles. force_www_redirect is
// false on purpose so an apex install doesn't pre-claim the www. sibling — if
// the operator wants www in the SAN list they use the full Add domain form.
export async function installSslAction(domain: string): Promise<void> {
  const s = await requireAdmin();
  if (!FQDN_RE.test(domain)) throw new Error("invalid domain");

  const collision = checkAppSubdomainCollision(domain);
  if (collision && collision.ok === false) {
    throw new Error(collision.error);
  }

  const existing = db()
    .prepare(`SELECT 1 FROM sites WHERE domain = ?`)
    .get(domain);
  if (existing) throw new Error("site already exists");

  await ensureDnsZone(s.userId, domain);

  const result = await doCreate(
    s.userId,
    domain,
    { mode: "folder", upstreamPort: null, folderPath: `/var/www/${domain}` },
    false,
    true,
    "sites.create",
  );
  if (result.ok === false) {
    if ("conflict" in result && result.conflict === "agent_only") {
      throw new Error(
        "Domain already has an nginx vhost on the agent; use the Add domain form to Adopt or Replace.",
      );
    }
    throw new Error(result.error);
  }
  revalidatePath("/domains");
}
