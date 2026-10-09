import "server-only";
import { createHash, randomBytes, timingSafeEqual } from "node:crypto";
import { config } from "./config";
import { getSetting, setSetting } from "./settings";

/**
 * The bearer token external AI clients send to `/api/mcp`.
 *
 * Only its SHA-256 is stored (settings `mcp.token_hash`), with its scope and
 * last four characters; the token itself is shown once, when it is
 * generated. It used to sit in panel.db in plaintext and always gave every
 * tool. A token from those versions is moved to hash storage on first use
 * with scope "full", so connected clients keep working.
 *
 * `MCP_TOKEN` in /etc/vps-panel/.env still works as a legacy fallback (full
 * scope) when no token was generated in the panel.
 */

/** "read": only tools that read state without exposing secrets. */
export type McpScope = "read" | "full";

const HASH_KEY = "mcp.token_hash";
const SCOPE_KEY = "mcp.token_scope";
const HINT_KEY = "mcp.token_hint";
const CREATED_KEY = "mcp.token_created";
const LEGACY_KEY = "mcp.token";

export type McpTokenInfo = {
  source: "db" | "env";
  scope: McpScope;
  /** Last four characters, to tell tokens apart. */
  hint: string;
  createdAt: string | null;
};

function sha256(s: string): Buffer {
  return createHash("sha256").update(s).digest();
}

function migrateLegacyPlaintext(): void {
  const legacy = getSetting(LEGACY_KEY, "");
  if (!legacy) return;
  setSetting(HASH_KEY, sha256(legacy).toString("hex"));
  setSetting(SCOPE_KEY, "full");
  setSetting(HINT_KEY, legacy.slice(-4));
  setSetting(LEGACY_KEY, "");
}

export function getMcpTokenInfo(): McpTokenInfo | null {
  migrateLegacyPlaintext();
  if (getSetting(HASH_KEY, "")) {
    return {
      source: "db",
      scope: getSetting(SCOPE_KEY, "full") === "read" ? "read" : "full",
      hint: getSetting(HINT_KEY, ""),
      createdAt: getSetting(CREATED_KEY, "") || null,
    };
  }
  if (config.mcpToken) {
    return { source: "env", scope: "full", hint: config.mcpToken.slice(-4), createdAt: null };
  }
  return null;
}

/** The scope a presented token grants, or null when it is not the token. */
export function verifyMcpToken(presented: string): McpScope | null {
  const info = getMcpTokenInfo();
  if (!info) return null;
  const expected =
    info.source === "db" ? Buffer.from(getSetting(HASH_KEY, ""), "hex") : sha256(config.mcpToken ?? "");
  const got = sha256(presented);
  if (expected.length !== got.length) return null;
  return timingSafeEqual(expected, got) ? info.scope : null;
}

/** Replace the token; returns it in plaintext, the only time it is known. */
export function regenerateMcpToken(scope: McpScope): string {
  const t = randomBytes(32).toString("hex");
  setSetting(HASH_KEY, sha256(t).toString("hex"));
  setSetting(SCOPE_KEY, scope);
  setSetting(HINT_KEY, t.slice(-4));
  setSetting(CREATED_KEY, new Date().toISOString());
  setSetting(LEGACY_KEY, "");
  return t;
}

export function setMcpTokenScope(scope: McpScope): void {
  setSetting(SCOPE_KEY, scope);
}

/** After this the env fallback (if any) applies, else MCP is off. */
export function clearMcpToken(): void {
  for (const k of [HASH_KEY, HINT_KEY, CREATED_KEY, LEGACY_KEY]) setSetting(k, "");
}
