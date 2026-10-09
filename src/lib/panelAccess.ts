/** Settings key of the panel access allowlist (applied by the agent to the
 *  panel's nginx vhost). */
export const PANEL_ACCESS_KEY = "panel.access_allowlist";

/** Entries from a comma- or newline-separated list, de-duplicated. */
export function splitEntries(raw: string): string[] {
  return [...new Set(raw.split(/[\s,]+/).map((s) => s.trim()).filter(Boolean))];
}
