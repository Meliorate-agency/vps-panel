import "server-only";
import { db } from "./db";

/**
 * Per-folder command history for /terminal.
 *
 * The rule the UI promises: a command never appears twice in a folder, and
 * re-running it moves it to the top. That's an upsert on
 * `(actor_id, path, command)` bumping `last_used_at` — the UNIQUE index does
 * the deduplication, so there's no read-then-write race between two tabs.
 *
 * History is scoped to the exact directory string. `/srv/apps` and
 * `/srv/apps/myapp` are separate lists by design: the whole point is that what
 * you ran in one folder doesn't clutter another.
 */

export type HistoryRow = {
  id: number;
  command: string;
  uses: number;
  last_used_at: string;
};

/** Keep the most recent N commands per folder; older ones are pruned on write. */
const PER_PATH_LIMIT = 100;

/** Commands longer than this are almost certainly a paste accident. */
const MAX_COMMAND_LENGTH = 4000;

/** Trailing slashes and whitespace would otherwise fork one folder's history
 *  into several lists that look identical in the UI. */
export function normalizePath(p: string): string {
  const trimmed = p.trim();
  if (!trimmed) return "/";
  const stripped = trimmed.replace(/\/+$/, "");
  return stripped === "" ? "/" : stripped;
}

export function recordCommand(actorId: number, path: string, command: string): void {
  const cmd = command.trim();
  if (!cmd || cmd.length > MAX_COMMAND_LENGTH) return;
  const dir = normalizePath(path);

  db()
    .prepare(
      `INSERT INTO terminal_history (actor_id, path, command)
       VALUES (?, ?, ?)
       ON CONFLICT (actor_id, path, command)
       DO UPDATE SET last_used_at = datetime('now'), uses = uses + 1`,
    )
    .run(actorId, dir, cmd);

  // Prune by rank rather than by age: a folder you use twice a year should
  // keep its history, a folder you hammer shouldn't grow without limit.
  db()
    .prepare(
      `DELETE FROM terminal_history
        WHERE actor_id = ? AND path = ? AND id NOT IN (
          SELECT id FROM terminal_history
           WHERE actor_id = ? AND path = ?
           ORDER BY last_used_at DESC
           LIMIT ?
        )`,
    )
    .run(actorId, dir, actorId, dir, PER_PATH_LIMIT);
}

export function historyForPath(actorId: number, path: string, limit = 50): HistoryRow[] {
  return db()
    .prepare(
      `SELECT id, command, uses, last_used_at
         FROM terminal_history
        WHERE actor_id = ? AND path = ?
        ORDER BY last_used_at DESC
        LIMIT ?`,
    )
    .all(actorId, normalizePath(path), limit) as HistoryRow[];
}

export function deleteHistoryEntry(actorId: number, id: number): void {
  // actor_id in the WHERE clause, not just the id — one admin must not be able
  // to delete another's history by guessing a row id.
  db()
    .prepare(`DELETE FROM terminal_history WHERE id = ? AND actor_id = ?`)
    .run(id, actorId);
}

export function clearHistoryForPath(actorId: number, path: string): void {
  db()
    .prepare(`DELETE FROM terminal_history WHERE actor_id = ? AND path = ?`)
    .run(actorId, normalizePath(path));
}

/** Folders this user has actually run something in, most recent first. Drives
 *  the "jump back to where you were working" list on the terminal page. */
export function recentPaths(actorId: number, limit = 12): { path: string; last_used_at: string }[] {
  return db()
    .prepare(
      `SELECT path, MAX(last_used_at) AS last_used_at
         FROM terminal_history
        WHERE actor_id = ?
        GROUP BY path
        ORDER BY last_used_at DESC
        LIMIT ?`,
    )
    .all(actorId, limit) as { path: string; last_used_at: string }[];
}
