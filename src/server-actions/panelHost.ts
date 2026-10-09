"use server";

import { revalidatePath } from "next/cache";
import { agent } from "@/lib/agent";
import { requireAdmin } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";
import { db } from "@/lib/db";

export type StartResult =
  | { ok: true; taskId: string }
  | { ok: false; error: string };

export type CompleteResult = { ok: true } | { ok: false; error: string };

function validateHost(raw: string): string | null {
  if (raw === "") return null;
  if (/\s/.test(raw)) return "hostname must not contain whitespace";
  if (raw.length > 253) return "hostname too long";
  if (!raw.includes(".")) {
    return "hostname must contain a dot (e.g. panel.example.com)";
  }
  if (/^[0-9.]+$/.test(raw)) {
    return "hostname must not be an IP literal — Postfix needs a real name";
  }
  if (!/^[a-zA-Z0-9.-]+$/.test(raw)) {
    return "hostname contains invalid characters";
  }
  return null;
}

export type SetPanelHostOptions = {
  createParentZone?: boolean;
  issueSsl?: boolean;
};

/**
 * Start the panel-host setup task on the agent and return its task id. The
 * caller (PanelHostForm) streams progress via `/api/tasks/:id/stream` and
 * calls `completePanelHostSetupAction` after the task succeeds.
 */
export async function setPanelHostAction(
  value: string,
  opts: SetPanelHostOptions = {},
): Promise<StartResult> {
  const s = await requireAdmin();
  const trimmed = value.trim().toLowerCase();
  const err = validateHost(trimmed);
  if (err) {
    writeAudit({
      actorId: s.userId,
      action: "panel-host.set",
      target: trimmed || "(empty)",
      result: "error",
      message: err,
    });
    return { ok: false, error: err };
  }
  try {
    const r = await agent.system.setPanelHost(trimmed, opts);
    writeAudit({
      actorId: s.userId,
      action: "panel-host.set",
      target: trimmed || "(empty)",
      result: "ok",
      message: `task ${r.task_id}`,
    });
    return { ok: true, taskId: r.task_id };
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "panel-host.set",
      target: trimmed || "(empty)",
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
}

/**
 * Called by `PanelHostForm` once the agent task fires `Done(succeeded)`.
 * The agent has already created the PowerDNS zone(s) and (optionally)
 * issued the cert; this action backfills the panel SQLite `domains` table
 * so the new hostname (and its parent, if one was created) show up under
 * `/domains` as managed zones.
 *
 * `host` is the panel hostname the user just saved. We re-query the agent
 * for the current zone list so we never insert a row for a zone that
 * doesn't actually exist (e.g. if SSL succeeded but DNS creation had a
 * race we couldn't see from the SSE stream).
 */
export async function completePanelHostSetupAction(
  host: string,
): Promise<CompleteResult> {
  const s = await requireAdmin();
  const trimmed = host.trim().toLowerCase();
  if (trimmed === "") {
    return { ok: true };
  }
  try {
    const zones = await agent.dns.listZones().catch(() => []);
    const zoneSet = new Set(zones.map((z) => z.name));
    const labels = trimmed.split(".");
    let inserted = 0;
    const stmt = db().prepare(
      `INSERT OR IGNORE INTO domains (name) VALUES (?)`,
    );

    // If the agent created/used a parent zone, register it. We walk
    // labels right-to-left and pick the first ancestor that's now
    // managed.
    let parentZone: string | null = null;
    for (let i = 1; i < labels.length; i++) {
      const candidate = labels.slice(i).join(".");
      if (zoneSet.has(candidate)) {
        parentZone = candidate;
        break;
      }
    }
    if (parentZone) {
      const info = stmt.run(parentZone);
      if (info.changes > 0) inserted += 1;
    }

    // If the host itself became its own zone (no managed parent), register it.
    if (!parentZone && zoneSet.has(trimmed)) {
      const info = stmt.run(trimmed);
      if (info.changes > 0) inserted += 1;
    }

    writeAudit({
      actorId: s.userId,
      action: "panel-host.complete",
      target: trimmed,
      result: "ok",
      message: `inserted ${inserted} domains row(s)`,
    });
    revalidatePath("/domains");
    return { ok: true };
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "panel-host.complete",
      target: trimmed,
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
}
