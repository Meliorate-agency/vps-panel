/**
 * MCP (Model Context Protocol) server endpoint.
 *
 * External AI clients (Claude Desktop, Cursor, custom GPTs, …) POST
 * JSON-RPC 2.0 messages here. We implement the subset of the MCP spec
 * needed for tool exposure:
 *
 *   - initialize          → protocol handshake
 *   - tools/list          → enumerate src/lib/mcp/registry.ts
 *   - tools/call          → dispatch into the matching Tool.call
 *   - ping                → cheap liveness
 *
 * Auth is a bearer token in the `Authorization` header, checked against
 * the SHA-256 stored by lib/mcpToken.ts (or MCP_TOKEN from .env). No token
 * configured → 503. Wrong header → 401. The token's scope decides which
 * tools it sees and may call ("read": READ_ONLY_TOOLS in the registry).
 * Requests are rate-limited per client address, and an address that keeps
 * sending wrong tokens is refused for a while.
 *
 * Errors follow JSON-RPC 2.0 conventions:
 *   -32700 parse error / -32600 invalid request / -32601 method not
 *   found / -32602 invalid params / -32603 internal error.
 * Tool-call failures are returned as `result.isError: true` with the
 * error message in the text content, per MCP spec — they're successful
 * JSON-RPC calls that surface a logical failure to the AI.
 *
 * Audit: every tool call (and every refused one) writes an audit-log row
 * with `actorId: null` and `message: "via=mcp ip=<client> scope=<scope>"`.
 */

import { NextRequest } from "next/server";
import { writeAudit } from "@/lib/audit";
import { getMcpTokenInfo, verifyMcpToken, type McpScope } from "@/lib/mcpToken";
import { getTool, listTools } from "@/lib/mcp/registry";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const PROTOCOL_VERSION = "2025-06-18";

type JsonRpcId = string | number | null;

type JsonRpcRequest = {
  jsonrpc?: string;
  id?: JsonRpcId;
  method?: string;
  params?: unknown;
};

type JsonRpcSuccess = { jsonrpc: "2.0"; id: JsonRpcId; result: unknown };
type JsonRpcError = {
  jsonrpc: "2.0";
  id: JsonRpcId;
  error: { code: number; message: string; data?: unknown };
};

const ERROR_CODES = {
  PARSE_ERROR: -32700,
  INVALID_REQUEST: -32600,
  METHOD_NOT_FOUND: -32601,
  INVALID_PARAMS: -32602,
  INTERNAL_ERROR: -32603,
} as const;

function ok(id: JsonRpcId, result: unknown): JsonRpcSuccess {
  return { jsonrpc: "2.0", id, result };
}

function err(id: JsonRpcId, code: number, message: string, data?: unknown): JsonRpcError {
  return { jsonrpc: "2.0", id, error: { code, message, ...(data !== undefined ? { data } : {}) } };
}

type Caller = { scope: McpScope; ip: string };

/** nginx sets X-Real-IP; the panel only listens on 127.0.0.1, so it is the
 *  real client address. */
function clientIp(req: NextRequest): string {
  return (
    req.headers.get("x-real-ip") ||
    req.headers.get("x-forwarded-for")?.split(",")[0]?.trim() ||
    "unknown"
  );
}

// Rate limits, per client address, kept in this process.
const REQUEST_WINDOW_MS = 60_000;
const MAX_REQUESTS = 120;
const FAILURE_WINDOW_MS = 10 * 60_000;
const MAX_AUTH_FAILURES = 10;
type Window = { start: number; count: number };
const requestWindows = new Map<string, Window>();
const failureWindows = new Map<string, Window>();

function countIn(map: Map<string, Window>, key: string, windowMs: number, add: boolean): number {
  const now = Date.now();
  let w = map.get(key);
  if (!w || now - w.start >= windowMs) {
    if (!add) return 0;
    w = { start: now, count: 0 };
    map.set(key, w);
  }
  if (add) w.count += 1;
  if (map.size > 5000) {
    for (const [k, v] of map) if (now - v.start >= windowMs) map.delete(k);
  }
  return w.count;
}

function denyAuth(ip: string, status: number, body: string) {
  countIn(failureWindows, ip, FAILURE_WINDOW_MS, true);
  writeAudit({ actorId: null, action: "mcp.auth", result: "error", message: `via=mcp ip=${ip}; ${body}` });
  return { ok: false as const, status, body };
}

function authorize(
  req: NextRequest,
): ({ ok: true } & Caller) | { ok: false; status: number; body: string } {
  const ip = clientIp(req);
  if (countIn(failureWindows, ip, FAILURE_WINDOW_MS, false) >= MAX_AUTH_FAILURES) {
    return { ok: false, status: 429, body: "too many failed attempts from this address, try again later" };
  }
  if (countIn(requestWindows, ip, REQUEST_WINDOW_MS, true) > MAX_REQUESTS) {
    return { ok: false, status: 429, body: `rate limit: at most ${MAX_REQUESTS} requests per minute` };
  }
  if (!getMcpTokenInfo()) {
    return {
      ok: false,
      status: 503,
      body: "MCP disabled — generate a token under Settings → MCP token, or set MCP_TOKEN in /etc/vps-panel/.env.",
    };
  }
  const header = req.headers.get("authorization") ?? "";
  const m = header.match(/^Bearer\s+(.+)$/i);
  if (!m) return denyAuth(ip, 401, "missing bearer token");
  const scope = verifyMcpToken(m[1].trim());
  if (!scope) return denyAuth(ip, 401, "invalid bearer token");
  return { ok: true, scope, ip };
}

/** Convert a Tool[] to MCP `tools/list` shape: drop the implementation +
 *  any internal flags (`sensitive`) that aren't part of JSON Schema. */
function serializeToolsForList(scope: McpScope) {
  return listTools()
    .filter((t) => scope === "full" || t.readOnly)
    .map((t) => ({
    name: t.name,
    description: t.description,
    inputSchema: stripInternalFields(t.inputSchema),
  }));
}

function stripInternalFields<T>(node: T): T {
  if (Array.isArray(node)) {
    return node.map(stripInternalFields) as T;
  }
  if (node && typeof node === "object") {
    const out: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(node as Record<string, unknown>)) {
      if (k === "sensitive") continue;
      out[k] = stripInternalFields(v);
    }
    return out as T;
  }
  return node;
}

async function dispatch(
  message: JsonRpcRequest,
  caller: Caller,
): Promise<JsonRpcSuccess | JsonRpcError | null> {
  const via = `via=mcp ip=${caller.ip} scope=${caller.scope}`;
  // Reject malformed JSON-RPC (notifications — no id — are tolerated:
  // we just don't reply).
  const id: JsonRpcId = message.id ?? null;
  if (message.jsonrpc !== "2.0" || typeof message.method !== "string") {
    if (message.id === undefined) return null; // notification, no reply
    return err(id, ERROR_CODES.INVALID_REQUEST, "not a valid JSON-RPC 2.0 request");
  }

  const method = message.method;

  if (method === "initialize") {
    return ok(id, {
      protocolVersion: PROTOCOL_VERSION,
      capabilities: { tools: { listChanged: false } },
      serverInfo: { name: "vps-panel", version: "1.0.0" },
    });
  }

  // initialized / notifications/initialized — client-sent notification
  // confirming the handshake. No response.
  if (method === "notifications/initialized" || method === "initialized") {
    return null;
  }

  if (method === "ping") {
    return ok(id, {});
  }

  if (method === "tools/list") {
    return ok(id, { tools: serializeToolsForList(caller.scope) });
  }

  if (method === "tools/call") {
    const params = (message.params ?? {}) as { name?: unknown; arguments?: unknown };
    if (typeof params.name !== "string") {
      return err(id, ERROR_CODES.INVALID_PARAMS, "tools/call: params.name must be a string");
    }
    const tool = getTool(params.name);
    if (!tool) {
      return err(id, ERROR_CODES.METHOD_NOT_FOUND, `unknown tool: ${params.name}`);
    }
    if (caller.scope === "read" && !tool.readOnly) {
      writeAudit({
        actorId: null,
        action: "mcp.call",
        target: tool.name,
        result: "error",
        message: `${via}; refused: needs a full-access token`,
      });
      return err(
        id,
        ERROR_CODES.INVALID_REQUEST,
        `${tool.name} needs a full-access MCP token; this token is read-only`,
      );
    }
    const args =
      params.arguments && typeof params.arguments === "object" && !Array.isArray(params.arguments)
        ? (params.arguments as Record<string, unknown>)
        : {};

    try {
      const data = await tool.call(args);
      writeAudit({
        actorId: null,
        action: "mcp.call",
        target: tool.name,
        result: "ok",
        message: via,
      });
      // Per MCP spec, tool results are an array of content blocks. We
      // always wrap the JSON-stringified return value in a single text
      // block so AI clients can read it as JSON or display as-is.
      return ok(id, {
        content: [
          {
            type: "text",
            text: typeof data === "string" ? data : JSON.stringify(data, null, 2),
          },
        ],
        isError: false,
      });
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      writeAudit({
        actorId: null,
        action: "mcp.call",
        target: tool.name,
        result: "error",
        message: `${via}; ${msg}`,
      });
      // Tool-level failure → successful JSON-RPC, but isError=true so
      // the AI client can see "this tool didn't work" and react.
      return ok(id, {
        content: [{ type: "text", text: msg }],
        isError: true,
      });
    }
  }

  return err(id, ERROR_CODES.METHOD_NOT_FOUND, `unknown method: ${method}`);
}

export async function POST(req: NextRequest) {
  const auth = authorize(req);
  if (!auth.ok) {
    return new Response(auth.body, {
      status: auth.status,
      headers: { "Content-Type": "text/plain" },
    });
  }

  let payload: unknown;
  try {
    payload = await req.json();
  } catch {
    return Response.json(err(null, ERROR_CODES.PARSE_ERROR, "request body is not valid JSON"), {
      status: 400,
    });
  }

  // Per JSON-RPC 2.0 spec, batches are an array of requests. MCP rarely
  // uses them but we handle the shape for compatibility — dispatch each,
  // drop nulls (notifications), return what's left as a single array.
  if (Array.isArray(payload)) {
    const out: (JsonRpcSuccess | JsonRpcError)[] = [];
    for (const msg of payload) {
      const r = await dispatch(msg as JsonRpcRequest, auth);
      if (r) out.push(r);
    }
    if (out.length === 0) {
      // All-notifications batch — per spec, no response body.
      return new Response(null, { status: 204 });
    }
    return Response.json(out);
  }

  const result = await dispatch(payload as JsonRpcRequest, auth);
  if (!result) {
    // Notification — per spec, return 204 with no body.
    return new Response(null, { status: 204 });
  }
  return Response.json(result);
}

/** GET on the same endpoint is reserved by the MCP Streamable HTTP
 *  transport for server-initiated event streams. We don't push anything
 *  yet — explicit 405 lets compliant clients know not to retry. */
export async function GET(req: NextRequest) {
  const auth = authorize(req);
  if (!auth.ok) {
    return new Response(auth.body, {
      status: auth.status,
      headers: { "Content-Type": "text/plain" },
    });
  }
  return new Response("MCP server does not push notifications", {
    status: 405,
    headers: { Allow: "POST" },
  });
}
