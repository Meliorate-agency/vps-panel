/**
 * `dns.ns_pair`: the two nameservers every zone the panel creates gets,
 * stored as "ns1.example.com,ns2.example.com", the form the agent's
 * dns.rs::resolve_ns_pair reads. Empty = the per-zone defaults
 * ns1.<zone> / ns2.<zone>. Plain module: the form and setSettingAction
 * both use it.
 */

/** The two names in a stored value, for the form's two fields. */
export function nsPairFields(stored: string): [string, string] {
  const [ns1 = "", ns2 = ""] = stored.split(",").map((s) => s.trim());
  return [ns1, ns2];
}

const LABEL = "[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?";
// At least one dot; a trailing root dot is fine (the agent strips it).
const HOST = new RegExp(`^${LABEL}(?:\\.${LABEL})+\\.?$`, "i");

/** The value to store for the two fields, or what is wrong with them. */
export function nsPairValue(ns1: string, ns2: string): { value: string } | { error: string } {
  const a = ns1.trim();
  const b = ns2.trim();
  if (!a && !b) return { value: "" };
  if (!a || !b) {
    return { error: "Fill in both nameservers, or leave both empty for the per-zone defaults." };
  }
  for (const host of [a, b]) {
    if (!HOST.test(host)) return { error: `"${host}" is not a host name like ns1.example.com.` };
  }
  const name = (h: string) => h.toLowerCase().replace(/\.$/, "");
  if (name(a) === name(b)) return { error: "The two nameservers must be different names." };
  return { value: `${a},${b}` };
}
