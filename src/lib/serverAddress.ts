import "server-only";
import dns from "node:dns/promises";
import os from "node:os";
import { config } from "./config";

// Both fields independently optional so callers can render whichever they have.
// `domain` is whatever hostname we know for this server (from PANEL_HOST or
// reverse-DNS); `ip` is the resolved IPv4. Operators sometimes set PANEL_HOST
// to an IP literal and we want to still surface the domain (via PTR) on the
// dashboard, and vice versa.
export type ServerAddress = {
  domain: string | null;
  ip: string | null;
};

let cache: { addr: ServerAddress; expires: number } | null = null;
const TTL_MS = 5 * 60 * 1000;

export async function getServerAddress(): Promise<ServerAddress> {
  const now = Date.now();
  if (cache && cache.expires > now) return cache.addr;

  const addr = await resolveServerAddress();
  cache = { addr, expires: now + TTL_MS };
  return addr;
}

async function resolveServerAddress(): Promise<ServerAddress> {
  let host: string;
  try {
    host = config.panelHost;
  } catch {
    return { domain: null, ip: null };
  }

  if (!host) return { domain: null, ip: null };

  // PANEL_HOST is a hostname: keep it as the domain, resolve forward DNS for
  // the IP. Fall back to ipify if forward DNS fails (e.g. the operator hasn't
  // wired up A records yet) or gives no public address: Debian maps the
  // server's own name to 127.0.1.1 in /etc/hosts (Settings > Server hostname
  // writes the same line) and the local resolver answers from that file, so
  // with the panel hostname as the server's hostname, new zones got 127.0.1.1
  // in every record.
  if (!looksLikeIp(host)) {
    let named: string[] = [];
    try {
      named = (await dns.resolve4(host)).filter(isPublicIpv4);
    } catch {
      // forward DNS failed — fall through to the interfaces and the probe
    }
    let ip = pickServerIpv4(named, interfaceIpv4s());
    if (!ip) ip = await fetchPublicIp();
    return { domain: host, ip };
  }

  // PANEL_HOST is a raw IP literal: surface the IP, and best-effort lookup the
  // domain via reverse DNS (PTR). On this VPS the PTR is wired up for mail, so
  // the dashboard can still show the friendly name even though PANEL_HOST is
  // the bare IP.
  const domain = await reverseLookup(host);
  return { domain, ip: host };
}

/** IPv4 addresses on this server's interfaces, loopback aside. The panel
 *  runs on the server itself, so these are the server's. */
function interfaceIpv4s(): string[] {
  return Object.values(os.networkInterfaces())
    .flat()
    .filter((a) => a && a.family === "IPv4" && !a.internal)
    .map((a) => a!.address);
}

/**
 * The address DNS records for this server should carry, by the agent's rule
 * (server_ip::Ipv4Facts::address): the panel hostname's answer when it is on
 * an interface; else a public address on an interface, since an answer that
 * is not here is a proxy in front of the panel or the server the name used to
 * point at (after a move), and records naming it sent mail and sites away;
 * else that answer (1:1 NAT: the interface holds only a private address).
 */
export function pickServerIpv4(named: string[], local: string[]): string | null {
  return named.find((a) => local.includes(a)) ?? local.find(isPublicIpv4) ?? named[0] ?? null;
}

function looksLikeIp(s: string): boolean {
  return /^\d{1,3}(\.\d{1,3}){3}$/.test(s) || s.includes(":");
}

/** Not loopback, private, link-local, carrier-grade NAT or 0/8: the ranges
 *  install.sh and the agent's server_ip::is_private_v4 skip too. */
function isPublicIpv4(ip: string): boolean {
  return !/^(0|10|127)\.|^169\.254\.|^192\.168\.|^172\.(1[6-9]|2\d|3[01])\.|^100\.(6[4-9]|[7-9]\d|1[01]\d|12[0-7])\./.test(
    ip,
  );
}

async function fetchPublicIp(): Promise<string | null> {
  try {
    const res = await fetch("https://api.ipify.org", {
      signal: AbortSignal.timeout(2000),
      cache: "no-store",
    });
    if (!res.ok) return null;
    const txt = (await res.text()).trim();
    return looksLikeIp(txt) ? txt : null;
  } catch {
    return null;
  }
}

async function reverseLookup(ip: string): Promise<string | null> {
  try {
    const names = await dns.reverse(ip);
    // dns.reverse returns FQDNs without a trailing dot already on Node.
    // Pick the first plausible name (must contain a dot and not be a numeric
    // ARPA reflection).
    const pick = names.find((n) => n.includes(".") && !/^\d/.test(n));
    return pick ?? null;
  } catch {
    return null;
  }
}
