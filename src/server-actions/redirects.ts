"use server";

import { revalidatePath } from "next/cache";
import { AgentError } from "@/lib/agent";
import { db } from "@/lib/db";
import { requireAdmin } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";
import {
  agentErrorMessage,
  getRedirect,
  isSafeDomain,
  normalizeSourcePath,
  parseCode,
  parseMatchMode,
  syncDomainRedirects,
  validateSourcePath,
  validateTargetUrl,
  type RedirectCode,
  type RedirectMatchMode,
} from "@/lib/redirects";

export type RedirectActionResult = { ok: true } | { ok: false; error: string };

type Fields = {
  source_path: string;
  target_url: string;
  code: RedirectCode;
  match_mode: RedirectMatchMode;
};

function parseFields(fd: FormData): Fields | { error: string } {
  const match_mode = parseMatchMode(String(fd.get("match_mode") ?? "exact"));
  if (!match_mode) return { error: "Match must be exact or prefix." };
  const code = parseCode(String(fd.get("code") ?? "301"));
  if (!code) return { error: "Type must be 301 or 302." };
  const source_path = normalizeSourcePath(String(fd.get("source_path") ?? ""), match_mode);
  const srcErr = validateSourcePath(source_path);
  if (srcErr) return { error: srcErr };
  const target_url = String(fd.get("target_url") ?? "").trim();
  const dstErr = validateTargetUrl(target_url);
  if (dstErr) return { error: dstErr };
  return { source_path, target_url, code, match_mode };
}

function isUniqueViolation(e: unknown): boolean {
  return (
    typeof e === "object" &&
    e !== null &&
    "code" in e &&
    String((e as { code: unknown }).code).startsWith("SQLITE_CONSTRAINT")
  );
}

function duplicateMessage(domain: string, f: Fields): string {
  const how = f.match_mode === "exact" ? "an exact" : "a prefix";
  return `${domain} already has ${how} redirect for ${f.source_path}.`;
}

/**
 * Every mutation is "write the row, then push the domain's whole rule set to
 * the agent". If the agent refuses (bad rule, no vhost, nginx -t failed) the
 * row is rolled back so the page never shows a rule nginx isn't serving.
 */
export async function createRedirectAction(
  _prev: RedirectActionResult | null,
  fd: FormData,
): Promise<RedirectActionResult> {
  const s = await requireAdmin();
  const domain = String(fd.get("domain") ?? "").trim().toLowerCase();
  if (!isSafeDomain(domain)) return { ok: false, error: "Pick a domain." };
  const f = parseFields(fd);
  if ("error" in f) return { ok: false, error: f.error };

  let id: number;
  try {
    const res = db()
      .prepare(
        `INSERT INTO redirects (domain, source_path, target_url, code, match_mode)
         VALUES (?, ?, ?, ?, ?)`,
      )
      .run(domain, f.source_path, f.target_url, f.code, f.match_mode);
    id = Number(res.lastInsertRowid);
  } catch (e) {
    if (isUniqueViolation(e)) return { ok: false, error: duplicateMessage(domain, f) };
    throw e;
  }

  const target = `${domain}${f.source_path}`;
  try {
    await syncDomainRedirects(domain);
  } catch (e) {
    db().prepare(`DELETE FROM redirects WHERE id = ?`).run(id);
    const msg = agentErrorMessage(e);
    writeAudit({ actorId: s.userId, action: "redirects.create", target, result: "error", message: msg });
    return { ok: false, error: msg };
  }
  writeAudit({
    actorId: s.userId,
    action: "redirects.create",
    target,
    request: { domain, ...f },
    result: "ok",
    message: `${f.code} ${f.match_mode} -> ${f.target_url}`,
  });
  revalidatePath("/redirects");
  return { ok: true };
}

export async function updateRedirectAction(
  id: number,
  _prev: RedirectActionResult | null,
  fd: FormData,
): Promise<RedirectActionResult> {
  const s = await requireAdmin();
  const before = getRedirect(id);
  if (!before) return { ok: false, error: "This redirect no longer exists." };
  const f = parseFields(fd);
  if ("error" in f) return { ok: false, error: f.error };

  const update = db().prepare(
    `UPDATE redirects SET source_path = ?, target_url = ?, code = ?, match_mode = ? WHERE id = ?`,
  );
  try {
    update.run(f.source_path, f.target_url, f.code, f.match_mode, id);
  } catch (e) {
    if (isUniqueViolation(e)) return { ok: false, error: duplicateMessage(before.domain, f) };
    throw e;
  }

  const target = `${before.domain}${f.source_path}`;
  try {
    await syncDomainRedirects(before.domain);
  } catch (e) {
    update.run(before.source_path, before.target_url, before.code, before.match_mode, id);
    const msg = agentErrorMessage(e);
    writeAudit({ actorId: s.userId, action: "redirects.update", target, result: "error", message: msg });
    return { ok: false, error: msg };
  }
  writeAudit({
    actorId: s.userId,
    action: "redirects.update",
    target,
    request: { domain: before.domain, ...f },
    result: "ok",
    message: `${f.code} ${f.match_mode} -> ${f.target_url}`,
  });
  revalidatePath("/redirects");
  return { ok: true };
}

export async function deleteRedirectAction(id: number): Promise<void> {
  const s = await requireAdmin();
  const row = getRedirect(id);
  if (!row) {
    revalidatePath("/redirects");
    return;
  }
  const target = `${row.domain}${row.source_path}`;
  db().prepare(`DELETE FROM redirects WHERE id = ?`).run(id);
  try {
    await syncDomainRedirects(row.domain);
  } catch (e) {
    // 409 = the domain has no panel-managed vhost any more (site or app was
    // removed). Nothing on disk is serving the rule, so dropping the row is
    // the right outcome - keep going instead of resurrecting a dead rule.
    if (!(e instanceof AgentError && e.status === 409)) {
      db()
        .prepare(
          `INSERT INTO redirects (id, domain, source_path, target_url, code, match_mode, created_at)
           VALUES (?, ?, ?, ?, ?, ?, ?)`,
        )
        .run(row.id, row.domain, row.source_path, row.target_url, row.code, row.match_mode, row.created_at);
      const msg = agentErrorMessage(e);
      writeAudit({ actorId: s.userId, action: "redirects.delete", target, result: "error", message: msg });
      throw new Error(msg);
    }
  }
  writeAudit({
    actorId: s.userId,
    action: "redirects.delete",
    target,
    result: "ok",
    message: `${row.code} ${row.match_mode} -> ${row.target_url}`,
  });
  revalidatePath("/redirects");
}
