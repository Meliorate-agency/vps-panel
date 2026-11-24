"use server";

import { revalidatePath } from "next/cache";
import type { PrivacyStatus, SubjectEraseItem } from "@/lib/agent";
import { requireAdmin } from "@/lib/auth";
import { pruneByRetention } from "@/lib/housekeeping";
import {
  applyPrivacy,
  eraseSubject,
  loadPrivacyStatus,
  parseRetention,
  privacySettings,
  retentionIsLower,
  savePrivacySettings,
  searchSubject,
  type SubjectSearch,
} from "@/lib/privacy";

// Settings > Privacy. Every action returns its refusal instead of throwing
// it: Next replaces a thrown message in a production build.

/** How many rows of the panel's own tables the new retention deleted. */
export type PrivacyPruned = { audit: number; logins: number; tasks: number };

export type PrivacySaveResult =
  | { ok: true; status: PrivacyStatus; pruned?: PrivacyPruned | null }
  | {
      ok: false;
      error: string;
      /** The setting is stored; only applying it on the server failed. */
      saved?: boolean;
      /** Saving deletes data: the operator has to confirm first. */
      needsConfirm?: boolean;
    };

export type PrivacyResult = { ok: true } | { ok: false; error: string };
export type PrivacyDataResult<T> = { ok: true; data: T } | { ok: false; error: string };

function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** The settings are stored; the agent then brings the system to them. */
async function applyAfterSave(
  actorId: number,
  pruned?: PrivacyPruned | null,
): Promise<PrivacySaveResult> {
  const applied = await applyPrivacy(actorId);
  if (!applied.ok) {
    return {
      ok: false,
      saved: true,
      error: `Saved, but not applied: ${applied.error.replace(/\.$/, "")}. Save again to retry.`,
    };
  }
  return { ok: true, status: applied.status, pruned };
}

/** Website logs: visitor addresses full or masked, query strings kept or left out. */
export async function savePrivacyLogsAction(logIp: string, logQuery: string): Promise<PrivacySaveResult> {
  const s = await requireAdmin();
  try {
    const saved = savePrivacySettings({ log_ip: logIp, log_query: logQuery }, s.userId);
    if (!saved.ok) return saved;
    revalidatePath("/settings");
    return await applyAfterSave(s.userId);
  } catch (e: unknown) {
    return { ok: false, error: errorText(e) };
  }
}

/**
 * "Keep logs for N days"; `days` empty = no limit. A value that deletes data
 * the current one keeps is only saved with `confirmed`, which the form sends
 * after the operator said yes. The panel's own tables are pruned right away,
 * not at the next daily run.
 */
export async function savePrivacyRetentionAction(
  days: string,
  confirmed: boolean,
): Promise<PrivacySaveResult> {
  const s = await requireAdmin();
  try {
    const next = parseRetention(days);
    if ("error" in next) return { ok: false, error: next.error };
    if (retentionIsLower(privacySettings().retention_days, next.days) && confirmed !== true) {
      return {
        ok: false,
        needsConfirm: true,
        error: `This deletes logs older than ${next.days} days. Confirm to save it.`,
      };
    }
    const saved = savePrivacySettings({ retention_days: next.days }, s.userId);
    if (!saved.ok) return saved;
    revalidatePath("/settings");
    const pruned = pruneByRetention();
    return await applyAfterSave(s.userId, pruned);
  } catch (e: unknown) {
    return { ok: false, error: errorText(e) };
  }
}

/** Outside lookups: the server locations on My Networks. Panel only, no agent. */
export async function savePrivacyLookupsAction(serverGeo: string): Promise<PrivacyResult> {
  const s = await requireAdmin();
  try {
    const saved = savePrivacySettings({ server_geo: serverGeo }, s.userId);
    if (!saved.ok) return saved;
    revalidatePath("/settings");
    revalidatePath("/networks");
    return { ok: true };
  } catch (e: unknown) {
    return { ok: false, error: errorText(e) };
  }
}

/** What holds on the server now ("Check again"). */
export async function privacyStatusAction(): Promise<PrivacyDataResult<PrivacyStatus>> {
  await requireAdmin();
  const status = await loadPrivacyStatus();
  return "error" in status ? { ok: false, error: status.error } : { ok: true, data: status };
}

/**
 * Personal data request: what the server and the panel hold about one email
 * address or IP address. The value comes in the action's POST body, never in
 * a URL, and the audit entry holds a hash of it.
 */
export async function subjectSearchAction(kind: string, value: string): Promise<PrivacyDataResult<SubjectSearch>> {
  const s = await requireAdmin();
  try {
    const r = await searchSubject(kind, value, s.userId);
    return r.ok ? { ok: true, data: r.report } : r;
  } catch (e: unknown) {
    return { ok: false, error: errorText(e) };
  }
}

/** Erases the subject from the listed sources; see lib/privacy.ts eraseSubject. */
export async function subjectEraseAction(
  kind: string,
  value: string,
  sources: string[],
): Promise<PrivacyDataResult<SubjectEraseItem[]>> {
  const s = await requireAdmin();
  try {
    const r = await eraseSubject(kind, value, sources, s.userId);
    return r.ok ? { ok: true, data: r.results } : r;
  } catch (e: unknown) {
    return { ok: false, error: errorText(e) };
  }
}
