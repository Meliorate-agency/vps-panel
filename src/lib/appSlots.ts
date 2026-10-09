import "server-only";
import { db } from "./db";
import { QUICK_INSTALL_RECIPES } from "./quickInstallRecipes";

/** A port-mode site from /domains: `site-<domain>` proxies to 127.0.0.1:port. */
export type RoutingSlot = {
  domain: string;
  port: number;
  /** Panel app already on this slot (or sharing its port), if any. */
  boundTo: string | null;
  /** Quick-install service whose default port this slot proxies to, if any. */
  service: string | null;
};

const SERVICE_BY_PORT = new Map(
  QUICK_INSTALL_RECIPES.filter((r) => r.default_port > 0).map((r) => [
    r.default_port,
    r.name,
  ]),
);

/**
 * Every routing slot with who already owns it. Bound and service slots must
 * never be offered to another app - the agent still checks the port at
 * assignment time, but the dropdowns on /apps/new and /apps/<name> are what
 * keep an operator from picking a taken one in the first place.
 */
export function listRoutingSlots(): RoutingSlot[] {
  const conn = db();
  const rows = conn
    .prepare(
      `SELECT s.domain, s.upstream_port AS port, a.name AS bound_to
         FROM sites s
         LEFT JOIN apps a ON a.subdomain = s.domain
        WHERE s.mode = 'port' AND s.upstream_port IS NOT NULL
        ORDER BY s.domain ASC`,
    )
    .all() as { domain: string; port: number; bound_to: string | null }[];
  // A second site pointing at an app's port would serve that app too, so it
  // counts as taken by the same app.
  const appByPort = new Map(
    (
      conn
        .prepare(`SELECT name, port FROM apps WHERE port > 0`)
        .all() as { name: string; port: number }[]
    ).map((r) => [r.port, r.name]),
  );
  return rows.map((r) => ({
    domain: r.domain,
    port: r.port,
    boundTo: r.bound_to ?? appByPort.get(r.port) ?? null,
    service: SERVICE_BY_PORT.get(r.port) ?? null,
  }));
}

/** Slots `appName` may pick: unowned ones plus the one it already holds. */
export function freeSlotsFor(appName: string, slots = listRoutingSlots()): RoutingSlot[] {
  return slots.filter(
    (s) => s.service === null && (s.boundTo === null || s.boundTo === appName),
  );
}

/** Why a slot is hidden from `appName`'s picker, for the hint line. */
export function slotUnavailableReason(s: RoutingSlot, appName: string): string | null {
  if (s.service) return `service ${s.service}`;
  if (s.boundTo && s.boundTo !== appName) return `app ${s.boundTo}`;
  return null;
}
