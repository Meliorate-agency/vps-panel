import "server-only";
import { db } from "./db";
import { agent, AgentError, type RedirectRuleInput } from "./agent";

/**
 * Redirect rules (301 / 302) live in panel.db and are pushed to the agent as
 * a whole per-domain set - the agent is a renderer, never a store. Every
 * mutation therefore goes: write the row(s), then `syncDomainRedirects`.
 */

export type RedirectCode = 301 | 302;
export type RedirectMatchMode = "exact" | "prefix";

export type RedirectRow = {
  id: number;
  domain: string;
  source_path: string;
  target_url: string;
  code: RedirectCode;
  match_mode: RedirectMatchMode;
  created_at: string;
};

const SELECT = `SELECT id, domain, source_path, target_url, code, match_mode, created_at FROM redirects`;

export function listRedirects(): RedirectRow[] {
  return db()
    .prepare(`${SELECT} ORDER BY domain, match_mode, source_path`)
    .all() as RedirectRow[];
}

export function listRedirectsForDomain(domain: string): RedirectRow[] {
  return db()
    .prepare(`${SELECT} WHERE domain = ? ORDER BY match_mode, source_path`)
    .all(domain) as RedirectRow[];
}

export function getRedirect(id: number): RedirectRow | null {
  return (db().prepare(`${SELECT} WHERE id = ?`).get(id) as RedirectRow | undefined) ?? null;
}

/**
 * Push the domain's complete rule set to the agent, which rewrites
 * `/etc/nginx/vps-redirects/<domain>/redirects.conf`, ensures the vhost
 * includes it, tests and reloads nginx. An empty set removes the file.
 */
export async function syncDomainRedirects(domain: string): Promise<void> {
  const rules: RedirectRuleInput[] = listRedirectsForDomain(domain).map((r) => ({
    id: r.id,
    source_path: r.source_path,
    target_url: r.target_url,
    code: r.code,
    match_mode: r.match_mode,
  }));
  await agent.redirects.put(domain, rules);
}

/** The agent answers `{error, code}` JSON; surface the sentence, not the envelope. */
export function agentErrorMessage(e: unknown): string {
  if (e instanceof AgentError) {
    try {
      const parsed = JSON.parse(e.body) as { error?: string };
      if (parsed.error) return parsed.error;
    } catch {
      // not JSON - fall through to the raw message
    }
    return e.body || e.message;
  }
  return e instanceof Error ? e.message : String(e);
}

// ─── Validation ─────────────────────────────────────────────────────────
// Mirrors agent/src/handlers/redirects.rs so the form gets an immediate,
// specific message instead of a relayed agent error. The agent re-checks
// everything; this is UX, not the security boundary.

const DOMAIN_RE =
  /^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)+$/;

export function isSafeDomain(d: string): boolean {
  return DOMAIN_RE.test(d);
}

export function parseCode(v: string): RedirectCode | null {
  return v === "301" ? 301 : v === "302" ? 302 : null;
}

export function parseMatchMode(v: string): RedirectMatchMode | null {
  return v === "exact" || v === "prefix" ? v : null;
}

/** Trim, and for prefix rules drop the trailing slash (`/blog/` == `/blog`). */
export function normalizeSourcePath(raw: string, mode: RedirectMatchMode): string {
  let p = raw.trim();
  if (mode === "prefix" && p.length > 1) p = p.replace(/\/+$/, "");
  return p;
}

export function validateSourcePath(p: string): string | null {
  if (!p.startsWith("/")) return "Source path must start with /.";
  if (p.length > 512) return "Source path is too long (max 512 characters).";
  if (!/^[A-Za-z0-9/_.~%-]+$/.test(p)) {
    return "Source path may only contain letters, digits and / _ . ~ % - (no spaces or query strings).";
  }
  if (p.split("/").includes("..")) return "Source path may not contain ..";
  if (p.startsWith("/.well-known")) {
    return "/.well-known is reserved for Let's Encrypt validation.";
  }
  return null;
}

export function validateTargetUrl(t: string): string | null {
  if (!(t.startsWith("https://") || t.startsWith("http://") || t.startsWith("/"))) {
    return "Redirect target must start with https://, http:// or /.";
  }
  if (t.length > 2048) return "Redirect target is too long (max 2048 characters).";
  if (!/^[A-Za-z0-9._~:/?=&%+,@!*()[\]#-]+$/.test(t)) {
    return "Redirect target contains characters that are not allowed (no spaces, quotes, $ ; { } \\ < >).";
  }
  return null;
}
