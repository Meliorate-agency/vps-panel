"use server";

import { revalidatePath } from "next/cache";
import { db } from "@/lib/db";
import { requireSession } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";

export type ActionResult = { ok: true } | { ok: false; error: string };

export type AppFolderRow = {
  id: number;
  name: string;
  sort_order: number;
  collapsed: number;
};

const MAX_NAME_LEN = 60;

/**
 * Read every folder ordered by sort_order then id. Used by the /apps
 * server component to drive the per-folder section rendering. `async`
 * to satisfy "use server" — the SQLite read itself is synchronous.
 */
export async function listAppFolders(): Promise<AppFolderRow[]> {
  await requireSession();
  return db()
    .prepare(
      `SELECT id, name, sort_order, collapsed
         FROM app_folders
        ORDER BY sort_order ASC, id ASC`,
    )
    .all() as AppFolderRow[];
}

/**
 * Map of app name → folder_id (or null) for all panel-managed app rows.
 * Read directly off `apps` because the agent's apps.list() doesn't expose
 * folder_id (it's a panel-only display concern, not something the agent
 * needs to know about).
 */
export async function listAppFolderAssignments(): Promise<
  Record<string, number | null>
> {
  await requireSession();
  const rows = db()
    .prepare(`SELECT name, folder_id FROM apps`)
    .all() as { name: string; folder_id: number | null }[];
  const out: Record<string, number | null> = {};
  for (const r of rows) out[r.name] = r.folder_id;
  return out;
}

export async function createAppFolderAction(
  _prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
  const s = await requireSession();
  const name = String(fd.get("name") ?? "").trim();
  if (!name) return { ok: false, error: "name required" };
  if (name.length > MAX_NAME_LEN) {
    return { ok: false, error: `name too long (max ${MAX_NAME_LEN})` };
  }
  // Reject duplicates so the operator notices instead of silently getting
  // two folders with the same label.
  const clash = db()
    .prepare(`SELECT id FROM app_folders WHERE name = ?`)
    .get(name) as { id: number } | undefined;
  if (clash) {
    return { ok: false, error: "a folder with that name already exists" };
  }
  const max = db()
    .prepare(`SELECT COALESCE(MAX(sort_order), 0) AS m FROM app_folders`)
    .get() as { m: number };
  db()
    .prepare(`INSERT INTO app_folders (name, sort_order) VALUES (?, ?)`)
    .run(name, max.m + 1);
  writeAudit({
    actorId: s.userId,
    action: "apps.folder.create",
    target: name,
    result: "ok",
  });
  revalidatePath("/apps");
  return { ok: true };
}

export async function renameAppFolderAction(
  folderId: number,
  _prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
  const s = await requireSession();
  const name = String(fd.get("name") ?? "").trim();
  if (!name) return { ok: false, error: "name required" };
  if (name.length > MAX_NAME_LEN) {
    return { ok: false, error: `name too long (max ${MAX_NAME_LEN})` };
  }
  const clash = db()
    .prepare(`SELECT id FROM app_folders WHERE name = ? AND id != ?`)
    .get(name, folderId) as { id: number } | undefined;
  if (clash) {
    return { ok: false, error: "another folder already has that name" };
  }
  const target = db()
    .prepare(`SELECT name FROM app_folders WHERE id = ?`)
    .get(folderId) as { name: string } | undefined;
  if (!target) return { ok: false, error: "folder not found" };
  db()
    .prepare(`UPDATE app_folders SET name = ? WHERE id = ?`)
    .run(name, folderId);
  writeAudit({
    actorId: s.userId,
    action: "apps.folder.rename",
    target: String(folderId),
    result: "ok",
    message: `${target.name} -> ${name}`,
  });
  revalidatePath("/apps");
  return { ok: true };
}

/**
 * Delete a folder. Apps inside go back to the "Ungrouped" bucket (folder_id
 * = NULL) — we never delete app rows here. No-op if the folder is already
 * gone, so a double-click on the delete button doesn't throw.
 */
export async function deleteAppFolderAction(folderId: number): Promise<void> {
  const s = await requireSession();
  const target = db()
    .prepare(`SELECT name FROM app_folders WHERE id = ?`)
    .get(folderId) as { name: string } | undefined;
  if (!target) return;
  db()
    .prepare(`UPDATE apps SET folder_id = NULL WHERE folder_id = ?`)
    .run(folderId);
  db().prepare(`DELETE FROM app_folders WHERE id = ?`).run(folderId);
  writeAudit({
    actorId: s.userId,
    action: "apps.folder.delete",
    target: target.name,
    result: "ok",
  });
  revalidatePath("/apps");
}

/**
 * Move (or unassign) an app to/from a folder. `folderId === null` puts the
 * app back in "Ungrouped". Validates folder existence so a stale UI doesn't
 * insert dangling references.
 */
export async function setAppFolderAction(
  appName: string,
  folderId: number | null,
): Promise<void> {
  const s = await requireSession();
  if (folderId !== null) {
    const exists = db()
      .prepare(`SELECT id FROM app_folders WHERE id = ?`)
      .get(folderId) as { id: number } | undefined;
    if (!exists) throw new Error("folder not found");
  }
  const app = db()
    .prepare(`SELECT name FROM apps WHERE name = ?`)
    .get(appName) as { name: string } | undefined;
  if (!app) throw new Error("app not found (only panel-managed apps can be foldered)");
  db()
    .prepare(`UPDATE apps SET folder_id = ? WHERE name = ?`)
    .run(folderId, appName);
  writeAudit({
    actorId: s.userId,
    action: "apps.folder.assign",
    target: appName,
    result: "ok",
    message: folderId === null ? "(none)" : `folder=${folderId}`,
  });
  revalidatePath("/apps");
}

/**
 * Toggle the collapsed flag for a folder. Persisted so the UI state
 * survives a page refresh / a different session. Returns void because the
 * UI doesn't need confirmation — revalidatePath re-renders with the new
 * row.
 */
export async function toggleAppFolderCollapsedAction(
  folderId: number,
): Promise<void> {
  await requireSession();
  const row = db()
    .prepare(`SELECT collapsed FROM app_folders WHERE id = ?`)
    .get(folderId) as { collapsed: number } | undefined;
  if (!row) return;
  const next = row.collapsed ? 0 : 1;
  db()
    .prepare(`UPDATE app_folders SET collapsed = ? WHERE id = ?`)
    .run(next, folderId);
  revalidatePath("/apps");
}
