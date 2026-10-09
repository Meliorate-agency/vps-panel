"use server";

import { agent, type FileEntry } from "@/lib/agent";
import { requireAdmin } from "@/lib/auth";
import {
  clearHistoryForPath,
  deleteHistoryEntry,
  historyForPath,
  normalizePath,
  recentPaths,
  recordCommand,
  type HistoryRow,
} from "@/lib/terminalHistory";

/**
 * Actions behind /terminal. Command EXECUTION is not here — it streams through
 * `/api/terminal/exec`, because a server action's result is serialised and
 * couldn't deliver output line by line while the command is still running.
 * These cover the tree and the per-folder history around it.
 */

export type TreeResult =
  | { ok: true; path: string; dirs: string[] }
  | { ok: false; error: string };

/** Absolute path -> the `fs` root's relative form (`/` is ""). */
function toRel(absolute: string): string {
  return normalizePath(absolute).replace(/^\/+/, "");
}

/**
 * Subdirectories of `absolute`, for one level of the tree. The tree loads
 * lazily — expanding a node fetches only its children, so opening /terminal
 * doesn't walk the entire filesystem.
 */
export async function listDirsAction(absolute: string): Promise<TreeResult> {
  await requireAdmin();
  const path = normalizePath(absolute);
  try {
    const listing = await agent.files.list("fs", toRel(path));
    const dirs = listing.entries
      .filter((e: FileEntry) => e.kind === "dir" || (e.kind === "symlink" && e.link_is_dir))
      .map((e: FileEntry) => e.name);
    return { ok: true, path, dirs };
  } catch (e: unknown) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }
}

export async function historyAction(path: string): Promise<HistoryRow[]> {
  const s = await requireAdmin();
  return historyForPath(s.userId, path);
}

/**
 * Called by the client after a command is dispatched. Kept separate from the
 * exec route so a command that fails, or that the operator stops halfway, is
 * still remembered — you usually want to recall the one that went wrong.
 */
export async function recordCommandAction(
  path: string,
  command: string,
): Promise<HistoryRow[]> {
  const s = await requireAdmin();
  recordCommand(s.userId, path, command);
  return historyForPath(s.userId, path);
}

export async function deleteHistoryAction(id: number, path: string): Promise<HistoryRow[]> {
  const s = await requireAdmin();
  deleteHistoryEntry(s.userId, id);
  return historyForPath(s.userId, path);
}

export async function clearHistoryAction(path: string): Promise<HistoryRow[]> {
  const s = await requireAdmin();
  clearHistoryForPath(s.userId, path);
  return historyForPath(s.userId, path);
}

export async function recentPathsAction(): Promise<{ path: string; last_used_at: string }[]> {
  const s = await requireAdmin();
  return recentPaths(s.userId);
}
