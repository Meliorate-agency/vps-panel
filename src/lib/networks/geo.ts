import "server-only";
import { lookup } from "node:dns/promises";
import { isIP } from "node:net";

/** Where a server is, from the IP its address resolves to. */
export type ServerLocation = {
  /** "Falkenstein, Germany". */
  label: string;
  /** Hover text: region, network operator and the IP looked up. */
  detail: string;
};

type Entry = { at: number; loc: ServerLocation | null };

/** Servers rarely move: a found location is kept for a day. */
const FOUND_TTL_MS = 24 * 3600_000;
/** A failed lookup (service down, rate limited, no DNS) is retried after this. */
const FAILED_TTL_MS = 10 * 60_000;
const TIMEOUT_MS = 5_000;

// On globalThis so dev-mode module reloads keep the cache.
const g = globalThis as { __networksGeo?: { cache: Map<string, Entry>; pending: Set<string> } };
const state = (g.__networksGeo ??= { cache: new Map(), pending: new Set() });

const countryNames = new Intl.DisplayNames(["en"], { type: "region" });

/**
 * The location of each server URL as known now (null = not known, or a
 * private address). Unknown and stale hosts are looked up in the background
 * so the page never waits on DNS or the geolocation service; the networks
 * page re-renders every 10 s and picks them up.
 */
export function serverLocations(urls: string[]): Map<string, ServerLocation | null> {
  const now = Date.now();
  const out = new Map<string, ServerLocation | null>();
  for (const url of urls) {
    const host = hostOf(url);
    if (!host) continue;
    const e = state.cache.get(host);
    out.set(url, e?.loc ?? null);
    const fresh = e && now - e.at < (e.loc ? FOUND_TTL_MS : FAILED_TTL_MS);
    if (fresh || state.pending.has(host)) continue;
    state.pending.add(host);
    void locate(host)
      .catch(() => null)
      .then((loc) => {
        // A failed refresh keeps the location found before.
        state.cache.set(host, { at: Date.now(), loc: loc ?? e?.loc ?? null });
      })
      .finally(() => state.pending.delete(host));
  }
  return out;
}

function hostOf(url: string): string | null {
  try {
    return new URL(url).hostname.replace(/^\[|\]$/g, "").toLowerCase() || null;
  } catch {
    return null;
  }
}

async function locate(host: string): Promise<ServerLocation | null> {
  const ip = isIP(host) ? host : await resolve(host);
  if (!ip || isPrivate(ip)) return null;
  return (await fromIpinfo(ip)) ?? (await fromGeojs(ip));
}

async function resolve(host: string): Promise<string | null> {
  for (const family of [4, 6] as const) {
    try {
      return (await lookup(host, { family })).address;
    } catch {
      // try the other family
    }
  }
  return null;
}

/** Addresses no public geolocation knows: loopback, LAN, CGNAT (Tailscale). */
function isPrivate(ip: string): boolean {
  if (isIP(ip) === 4) {
    const [a, b] = ip.split(".").map(Number);
    return (
      a === 10 ||
      a === 127 ||
      a === 0 ||
      (a === 169 && b === 254) ||
      (a === 172 && b >= 16 && b <= 31) ||
      (a === 192 && b === 168) ||
      (a === 100 && b >= 64 && b <= 127)
    );
  }
  const v6 = ip.toLowerCase();
  return v6 === "::1" || v6.startsWith("fc") || v6.startsWith("fd") || v6.startsWith("fe80:");
}

async function getJson(url: string): Promise<Record<string, unknown> | null> {
  const res = await fetch(url, {
    headers: { Accept: "application/json" },
    signal: AbortSignal.timeout(TIMEOUT_MS),
    cache: "no-store",
  });
  if (!res.ok) return null;
  return (await res.json()) as Record<string, unknown>;
}

function str(v: unknown): string {
  return typeof v === "string" ? v.trim() : "";
}

function build(ip: string, city: string, region: string, country: string, org: string): ServerLocation | null {
  const label = [city, country].filter(Boolean).join(", ");
  if (!label) return null;
  const place = [city, region !== city ? region : "", country].filter(Boolean).join(", ");
  return { label, detail: [place, org, ip].filter(Boolean).join(" · ") };
}

function countryName(code: string): string {
  try {
    return code ? (countryNames.of(code.toUpperCase()) ?? code) : "";
  } catch {
    return code;
  }
}

/** ipinfo.io: no key needed for this volume (a handful of hosts a day). */
async function fromIpinfo(ip: string): Promise<ServerLocation | null> {
  const j = await getJson(`https://ipinfo.io/${encodeURIComponent(ip)}/json`).catch(() => null);
  if (!j || j.bogon) return null;
  return build(ip, str(j.city), str(j.region), countryName(str(j.country)), str(j.org));
}

/** GeoJS, if ipinfo.io is down or rate limits this server. */
async function fromGeojs(ip: string): Promise<ServerLocation | null> {
  const j = await getJson(`https://get.geojs.io/v1/ip/geo/${encodeURIComponent(ip)}.json`).catch(() => null);
  if (!j) return null;
  const country = countryName(str(j.country_code)) || str(j.country);
  return build(ip, str(j.city), str(j.region), country, str(j.organization_name));
}
