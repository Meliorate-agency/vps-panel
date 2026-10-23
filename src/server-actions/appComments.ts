"use server";

import { revalidatePath } from "next/cache";
import { db } from "@/lib/db";
import { requireSession } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";

export type ActionResult = { ok: true } | { ok: false; error: string };

const MAX_COMMENT_LEN = 200;
/** Panel app names, and the folder names external apps go by. */
const APP_NAME_RE = /^[A-Za-z0-9_-]{1,64}$/;

/**
 * Map of app name → comment for the /apps Comment column. `async` to satisfy
 * "use server" — the SQLite read itself is synchronous.
 */
export async function listAppComments(): Promise<Record<string, string>> {
  await requireSession();
  const rows = db().prepare(`SELECT name, comment FROM app_comments`).all() as {
    name: string;
    comment: string;
  }[];
  return Object.fromEntries(rows.map((r) => [r.name, r.comment]));
}

/**
 * Set an app's comment, or clear it with an empty one. A display note like
 * app folders: it changes nothing on the server, so any signed-in user may
 * edit it. Whitespace (newlines included) collapses to single spaces, since
 * it shows in one table cell.
 */
export async function setAppCommentAction(name: string, comment: string): Promise<ActionResult> {
  const s = await requireSession();
  if (!APP_NAME_RE.test(name)) return { ok: false, error: "invalid app name" };
  const text = comment.replace(/\s+/g, " ").trim();
  if (text.length > MAX_COMMENT_LEN) {
    return { ok: false, error: `A comment can be at most ${MAX_COMMENT_LEN} characters.` };
  }
  if (text) {
    db()
      .prepare(
        `INSERT INTO app_comments (name, comment) VALUES (?, ?)
         ON CONFLICT(name) DO UPDATE SET comment = excluded.comment, updated_at = datetime('now')`,
      )
      .run(name, text);
  } else {
    db().prepare(`DELETE FROM app_comments WHERE name = ?`).run(name);
  }
  writeAudit({
    actorId: s.userId,
    action: "apps.comment",
    target: name,
    result: "ok",
    message: text || "(cleared)",
  });
  revalidatePath("/apps");
  return { ok: true };
}
