"use server";

import { revalidatePath } from "next/cache";
import { agent } from "@/lib/agent";
import { db } from "@/lib/db";
import { requireAdmin } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";
import { UPLOAD_LIMIT_KEY, UPLOAD_LIMIT_MAX_MB } from "@/lib/uploadLimit";
import { nsPairValue } from "@/lib/nsPair";
import { TAB_TITLE_KEY, tabTitleValue } from "@/lib/tabTitle";

export type ActionResult = { ok: true } | { ok: false; error: string };

export async function setSettingAction(key: string, value: string): Promise<ActionResult> {
  const s = await requireAdmin();
  if (!/^[a-z0-9_.\-]{1,64}$/i.test(key)) {
    return { ok: false, error: "invalid setting key" };
  }
  // Two nameservers or none: the agent ignores anything else and quietly
  // falls back to the per-zone defaults.
  if (key === "dns.ns_pair") {
    const [ns1 = "", ns2 = "", ...rest] = value.split(",");
    const pair = rest.length > 0 ? { error: "Two nameservers at most." } : nsPairValue(ns1, ns2);
    if ("error" in pair) return { ok: false, error: pair.error };
    value = pair.value;
  }
  db()
    .prepare(
      `INSERT INTO settings (key, value) VALUES (?, ?)
       ON CONFLICT(key) DO UPDATE SET value = excluded.value`,
    )
    .run(key, value);
  writeAudit({ actorId: s.userId, action: "settings.set", target: key, result: "ok" });

  revalidatePath("/settings");

  // Mirror the trusted IPs into fail2ban's dovecot ignoreip so a
  // misconfigured mail client can't get the office banned. The agent reads
  // the setting back from panel.db itself; we just signal that it changed.
  // The value stays saved either way, but a failed sync is reported instead
  // of showing "Saved." while fail2ban still bans those IPs.
  if (key === "panel.ip_allowlist") {
    try {
      await agent.firewall.syncOperatorIgnoreip();
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      writeAudit({
        actorId: s.userId,
        action: "settings.set",
        target: `${key}.sync`,
        result: "error",
        message: msg,
      });
      return { ok: false, error: `Saved, but fail2ban was not updated: ${msg}. Save again to retry.` };
    }
  }

  return { ok: true };
}

// Dedicated action for `panel.timezone` because we want IANA-TZ validation
// (a bad value would crash every page that formats a time). Same backing
// store as setSettingAction, just with a sanity check up front.
export async function setPanelTimezoneAction(
  _prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
  const s = await requireAdmin();
  const tz = String(fd.get("timezone") ?? "").trim();
  if (!tz) {
    return { ok: false, error: "timezone is required" };
  }
  try {
    // Throws RangeError if the runtime doesn't recognize the IANA name.
    new Intl.DateTimeFormat("en", { timeZone: tz });
  } catch {
    return {
      ok: false,
      error: `unknown timezone "${tz}". Use an IANA name like "Europe/Berlin", "America/New_York", or "UTC".`,
    };
  }
  db()
    .prepare(
      `INSERT INTO settings (key, value) VALUES (?, ?)
       ON CONFLICT(key) DO UPDATE SET value = excluded.value`,
    )
    .run("panel.timezone", tz);
  writeAudit({
    actorId: s.userId,
    action: "settings.set",
    target: "panel.timezone",
    result: "ok",
    message: tz,
  });
  // Topbar and audit page both read this on render; revalidate broadly so
  // the new tz lights up everywhere without a hard refresh.
  revalidatePath("/", "layout");
  return { ok: true };
}

// Dedicated action for the upload limit: a whole number of MB that the agent
// and nginx behind the upload route can take (see lib/uploadLimit.ts).
export async function setUploadLimitAction(next: string): Promise<ActionResult> {
  const s = await requireAdmin();
  const mb = Number(String(next).trim());
  if (!Number.isInteger(mb) || mb < 1 || mb > UPLOAD_LIMIT_MAX_MB) {
    return { ok: false, error: `Enter a whole number of MB from 1 to ${UPLOAD_LIMIT_MAX_MB}.` };
  }
  db()
    .prepare(
      `INSERT INTO settings (key, value) VALUES (?, ?)
       ON CONFLICT(key) DO UPDATE SET value = excluded.value`,
    )
    .run(UPLOAD_LIMIT_KEY, String(mb));
  writeAudit({
    actorId: s.userId,
    action: "settings.set",
    target: UPLOAD_LIMIT_KEY,
    result: "ok",
    message: `${mb} MB`,
  });
  revalidatePath("/settings");
  return { ok: true };
}

// Settings > Appearance: the panel's name in browser tabs (lib/tabTitle.ts).
// Empty puts the default back.
export async function setTabTitleAction(name: string): Promise<ActionResult> {
  const s = await requireAdmin();
  const r = tabTitleValue(String(name));
  if ("error" in r) return { ok: false, error: r.error };
  db()
    .prepare(
      `INSERT INTO settings (key, value) VALUES (?, ?)
       ON CONFLICT(key) DO UPDATE SET value = excluded.value`,
    )
    .run(TAB_TITLE_KEY, r.value);
  writeAudit({
    actorId: s.userId,
    action: "settings.set",
    target: TAB_TITLE_KEY,
    result: "ok",
    message: r.value || "(default)",
  });
  // Every page's <title> comes from the root layout.
  revalidatePath("/", "layout");
  return { ok: true };
}
