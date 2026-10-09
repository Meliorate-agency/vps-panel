"use server";

import { revalidatePath } from "next/cache";
import { requireAdmin } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";
import {
  clearMcpToken,
  regenerateMcpToken,
  setMcpTokenScope,
  type McpScope,
} from "@/lib/mcpToken";

export type GenerateResult =
  | { ok: true; token: string }
  | { ok: false; error: string };
type Result = { ok: true } | { ok: false; error: string };

function validScope(scope: unknown): scope is McpScope {
  return scope === "read" || scope === "full";
}

function refresh() {
  revalidatePath("/settings");
  revalidatePath("/ai-agent");
}

/**
 * Generate (or rotate) the MCP bearer token with the given scope. Returns
 * the plaintext this one time: only its hash is stored, so it cannot be
 * shown again later.
 */
export async function generateMcpTokenAction(scope: McpScope): Promise<GenerateResult> {
  const s = await requireAdmin();
  if (!validScope(scope)) return { ok: false, error: "unknown scope" };
  const token = regenerateMcpToken(scope);
  writeAudit({ actorId: s.userId, action: "mcp.token.rotate", result: "ok", message: `scope=${scope}` });
  refresh();
  return { ok: true, token };
}

/** Change what the current token may do, without replacing it. */
export async function setMcpScopeAction(scope: McpScope): Promise<Result> {
  const s = await requireAdmin();
  if (!validScope(scope)) return { ok: false, error: "unknown scope" };
  setMcpTokenScope(scope);
  writeAudit({ actorId: s.userId, action: "mcp.token.scope", result: "ok", message: `scope=${scope}` });
  refresh();
  return { ok: true };
}

/**
 * Remove the panel's token. The MCP_TOKEN env fallback applies if one is
 * set; otherwise the MCP endpoint is off.
 */
export async function clearMcpTokenAction(): Promise<Result> {
  const s = await requireAdmin();
  clearMcpToken();
  writeAudit({ actorId: s.userId, action: "mcp.token.clear", result: "ok" });
  refresh();
  return { ok: true };
}
