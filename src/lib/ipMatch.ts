/** IPv4 / IPv6 parsing and CIDR matching, for the panel access allowlist. */

type Ip = { v: 4 | 6; n: bigint };
type Cidr = Ip & { prefix: number };

const B = (x: number) => BigInt(x);

export function parseIp(raw: string): Ip | null {
  const s = raw.trim();
  if (/^\d{1,3}(\.\d{1,3}){3}$/.test(s)) {
    const parts = s.split(".").map(Number);
    if (parts.some((p) => p > 255)) return null;
    return { v: 4, n: parts.reduce((acc, p) => (acc << B(8)) | B(p), B(0)) };
  }
  if (!s.includes(":")) return null;
  let str = s;
  // Embedded IPv4 tail (::ffff:1.2.3.4) becomes two hex groups.
  const tail4 = str.match(/(\d{1,3}(?:\.\d{1,3}){3})$/);
  if (tail4) {
    const v4 = parseIp(tail4[1]);
    if (!v4) return null;
    str = `${str.slice(0, -tail4[1].length)}${(v4.n >> B(16)).toString(16)}:${(v4.n & B(0xffff)).toString(16)}`;
  }
  const halves = str.split("::");
  if (halves.length > 2) return null;
  const head = halves[0] ? halves[0].split(":") : [];
  const tail = halves.length === 2 && halves[1] ? halves[1].split(":") : [];
  const missing = 8 - head.length - tail.length;
  if ((halves.length === 1 && missing !== 0) || (halves.length === 2 && missing < 1)) return null;
  const groups = [...head, ...Array<string>(halves.length === 2 ? missing : 0).fill("0"), ...tail];
  if (groups.length !== 8 || groups.some((g) => !/^[0-9a-fA-F]{1,4}$/.test(g))) return null;
  return { v: 6, n: groups.reduce((acc, g) => (acc << B(16)) | B(parseInt(g, 16)), B(0)) };
}

/** `1.2.3.4`, `10.0.0.0/8`, `2001:db8::/32`. */
export function parseCidr(raw: string): Cidr | null {
  const [addr, p, extra] = raw.trim().split("/");
  if (extra !== undefined) return null;
  const ip = parseIp(addr);
  if (!ip) return null;
  const max = ip.v === 4 ? 32 : 128;
  const prefix = p === undefined ? max : /^\d{1,3}$/.test(p) ? Number(p) : NaN;
  if (!Number.isInteger(prefix) || prefix < 0 || prefix > max) return null;
  return { ...ip, prefix };
}

export function cidrContains(cidr: Cidr, ip: Ip): boolean {
  if (cidr.v !== ip.v) return false;
  const shift = B((cidr.v === 4 ? 32 : 128) - cidr.prefix);
  return cidr.n >> shift === ip.n >> shift;
}

/** Whether `ip` falls in any of `entries` (unparsable entries never match). */
export function ipAllowed(ip: string, entries: string[]): boolean {
  const addr = parseIp(ip);
  if (!addr) return false;
  return entries.some((e) => {
    const c = parseCidr(e);
    return c !== null && cidrContains(c, addr);
  });
}
