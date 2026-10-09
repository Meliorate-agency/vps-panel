"use server";

import { revalidatePath } from "next/cache";
import { agent, type FileListing, type TrashRow } from "@/lib/agent";
import { requireAdmin } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";

/**
 * Server actions behind /files.
 *
 * Every one is admin-only and audited with the ABSOLUTE path, not the
 * root-relative one — an audit row reading `apps: foo` is useless six months
 * later, `/srv/apps/foo` isn't. The agent owns all path validation (jail
 * escape, protected paths, name rules); these actions don't second-guess it,
 * they just surface its error text.
 */

export type FilesResult = { ok: true } | { ok: false; error: string };

/** Narrowed to the failure arm so it composes with the actions that return a
 *  richer success shape (`{ok:true, listing}`), not just `{ok:true}`. */
function fail(e: unknown): { ok: false; error: string } {
  return { ok: false, error: e instanceof Error ? e.message : String(e) };
}

/** Absolute path of `rel` inside `root`, for audit rows. */
function abs(rootPath: string, rel: string): string {
  const clean = rel.replace(/^\/+/, "");
  if (!clean) return rootPath;
  return rootPath === "/" ? `/${clean}` : `${rootPath}/${clean}`;
}

async function rootPathOf(rootId: string): Promise<string> {
  const roots = await agent.files.roots().catch(() => []);
  return roots.find((r) => r.id === rootId)?.path ?? rootId;
}

/** Re-read a directory. The browser calls this after every mutation instead
 *  of a full route refresh so the listing updates without a navigation. */
export async function listDirAction(
  root: string,
  path: string,
): Promise<{ ok: true; listing: FileListing } | { ok: false; error: string }> {
  await requireAdmin();
  try {
    return { ok: true, listing: await agent.files.list(root, path) };
  } catch (e: unknown) {
    return fail(e);
  }
}

export async function createFolderAction(
  root: string,
  path: string,
  name: string,
): Promise<FilesResult> {
  const s = await requireAdmin();
  try {
    await agent.files.mkdir(root, path, name);
    writeAudit({
      actorId: s.userId,
      action: "files.mkdir",
      target: abs(await rootPathOf(root), `${path}/${name}`),
      result: "ok",
    });
    revalidatePath(`/files/${root}`);
    return { ok: true };
  } catch (e: unknown) {
    return fail(e);
  }
}

export async function createFileAction(
  root: string,
  path: string,
  name: string,
): Promise<FilesResult> {
  const s = await requireAdmin();
  try {
    await agent.files.newFile(root, path, name);
    writeAudit({
      actorId: s.userId,
      action: "files.create",
      target: abs(await rootPathOf(root), `${path}/${name}`),
      result: "ok",
    });
    revalidatePath(`/files/${root}`);
    return { ok: true };
  } catch (e: unknown) {
    return fail(e);
  }
}

export async function renameAction(
  root: string,
  path: string,
  newName: string,
): Promise<FilesResult> {
  const s = await requireAdmin();
  try {
    await agent.files.rename(root, path, newName);
    writeAudit({
      actorId: s.userId,
      action: "files.rename",
      target: `${abs(await rootPathOf(root), path)} -> ${newName}`,
      result: "ok",
    });
    revalidatePath(`/files/${root}`);
    return { ok: true };
  } catch (e: unknown) {
    return fail(e);
  }
}

/**
 * Drag-drop move. The agent reports partial success (`moved` + `errors`), and
 * so do we: a five-item drop where one name collides should land the other
 * four and say so, not roll back.
 */
export async function moveAction(
  root: string,
  paths: string[],
  dest: string,
): Promise<FilesResult> {
  const s = await requireAdmin();
  try {
    const res = await agent.files.move(root, paths, dest);
    const rootPath = await rootPathOf(root);
    writeAudit({
      actorId: s.userId,
      action: "files.move",
      target: `${paths.length} item(s) -> ${abs(rootPath, dest)}`,
      result: res.ok ? "ok" : "error",
      message: res.errors.join("; ") || undefined,
    });
    revalidatePath(`/files/${root}`);
    if (!res.ok) {
      return {
        ok: false,
        error:
          res.moved && res.moved > 0
            ? `Moved ${res.moved}, failed ${res.errors.length}: ${res.errors.join("; ")}`
            : res.errors.join("; "),
      };
    }
    return { ok: true };
  } catch (e: unknown) {
    return fail(e);
  }
}

/** Delete = move to the agent's trash. Recoverable for 30 days. */
export async function trashAction(
  root: string,
  paths: string[],
): Promise<FilesResult> {
  const s = await requireAdmin();
  try {
    const res = await agent.files.trash(root, paths);
    const rootPath = await rootPathOf(root);
    writeAudit({
      actorId: s.userId,
      action: "files.trash",
      target: paths.map((p) => abs(rootPath, p)).join(", ").slice(0, 500),
      result: res.ok ? "ok" : "error",
      message: res.errors.join("; ") || undefined,
    });
    revalidatePath(`/files/${root}`);
    revalidatePath("/files/trash");
    if (!res.ok) {
      return {
        ok: false,
        error:
          res.trashed && res.trashed > 0
            ? `Moved ${res.trashed} to Trash, failed ${res.errors.length}: ${res.errors.join("; ")}`
            : res.errors.join("; "),
      };
    }
    return { ok: true };
  } catch (e: unknown) {
    return fail(e);
  }
}

export async function listTrashAction(): Promise<
  { ok: true; rows: TrashRow[] } | { ok: false; error: string }
> {
  await requireAdmin();
  try {
    return { ok: true, rows: await agent.files.listTrash() };
  } catch (e: unknown) {
    return fail(e);
  }
}

export async function restoreAction(id: string): Promise<FilesResult> {
  const s = await requireAdmin();
  try {
    const res = await agent.files.restore(id);
    writeAudit({
      actorId: s.userId,
      action: "files.restore",
      target: res.restored_to,
      result: "ok",
    });
    revalidatePath("/files/trash");
    revalidatePath("/files");
    return { ok: true };
  } catch (e: unknown) {
    return fail(e);
  }
}

/** Permanent. This is the only endpoint in /files that actually destroys
 *  data — the 30-day purge loop in the agent does the same thing on a timer. */
export async function purgeTrashAction(id: string): Promise<FilesResult> {
  const s = await requireAdmin();
  try {
    await agent.files.purge(id);
    writeAudit({ actorId: s.userId, action: "files.purge", target: id, result: "ok" });
    revalidatePath("/files/trash");
    return { ok: true };
  } catch (e: unknown) {
    return fail(e);
  }
}

export async function emptyTrashAction(): Promise<FilesResult> {
  const s = await requireAdmin();
  try {
    await agent.files.emptyTrash();
    writeAudit({
      actorId: s.userId,
      action: "files.trash_empty",
      target: "all",
      result: "ok",
    });
    revalidatePath("/files/trash");
    return { ok: true };
  } catch (e: unknown) {
    return fail(e);
  }
}
