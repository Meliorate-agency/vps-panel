import "server-only";

/**
 * One agentic tool exposed over MCP. Tools are grouped by panel `page` so
 * the operator can scan them in the /ai-agent preview UI by area (Apps,
 * Mailboxes, Domains, …). The dotted `name` is the JSON-RPC identifier
 * external clients use in `tools/call`.
 *
 * Implementations are thin adapters over `src/lib/agent.ts` — they
 * marshal a typed slice out of the `args` object, delegate to the agent
 * client, and return the raw JSON for the route handler to wrap into the
 * MCP `content` envelope.
 */
export type Tool = {
  /** Sidebar page this tool surfaces under. Used for grouping in the
   *  /ai-agent preview and for any future per-token page scoping. Matches
   *  the sidebar label (e.g. "Apps", "Mailboxes", "Domains"). */
  page: string;
  /** Dotted JSON-RPC name. `<area>.<verb>` e.g. "apps.list". Must be
   *  unique across the whole registry. */
  name: string;
  /** Single-sentence description shown to the AI client. */
  description: string;
  /** JSON Schema for arguments. The route handler validates incoming
   *  `tools/call` args against this and the SDK rejects malformed calls
   *  with a -32602 JSON-RPC error before `call` ever fires. */
  inputSchema: ToolInputSchema;
  /** Implementation. Args have already been shape-checked against
   *  `inputSchema`. Returns any JSON-serialisable value — the route
   *  handler stringifies it into the MCP `content[0].text` envelope. */
  call: (args: Record<string, unknown>) => Promise<unknown>;
  /** Set by the registry from READ_ONLY_TOOLS: callable with a read-only
   *  token. */
  readOnly?: boolean;
};

export type ToolInputSchema = {
  type: "object";
  properties: Record<string, ToolProperty>;
  required?: string[];
  additionalProperties?: false;
};

export type ToolProperty = {
  type: "string" | "number" | "integer" | "boolean" | "array" | "object";
  description?: string;
  enum?: readonly (string | number)[];
  /** For type="array". */
  items?: ToolProperty | { type: "object"; properties: Record<string, ToolProperty>; required?: string[] };
  /** For type="object". */
  properties?: Record<string, ToolProperty>;
  required?: string[];
  /** Defaults / examples surface in the /ai-agent preview UI; the MCP
   *  client may also relay them to the model. */
  default?: unknown;
  minimum?: number;
  maximum?: number;
  /** When set, the preview UI flags the field as sensitive (e.g. won't
   *  surface in copy-as-curl snippets). Not part of the JSON Schema
   *  spec — stripped before sending to MCP clients. */
  sensitive?: boolean;
};

/**
 * Helper for tool implementations: pull a typed string out of a
 * Record<string, unknown> and throw a stable error if it's missing or
 * the wrong shape. The route handler turns thrown errors into MCP
 * `isError: true` responses.
 */
export function requireString(args: Record<string, unknown>, key: string): string {
  const v = args[key];
  if (typeof v !== "string" || v.length === 0) {
    throw new Error(`missing required string argument: ${key}`);
  }
  return v;
}

export function optionalString(args: Record<string, unknown>, key: string): string | undefined {
  const v = args[key];
  if (v === undefined || v === null) return undefined;
  if (typeof v !== "string") {
    throw new Error(`argument ${key} must be a string`);
  }
  return v;
}

export function requireNumber(args: Record<string, unknown>, key: string): number {
  const v = args[key];
  if (typeof v !== "number" || !Number.isFinite(v)) {
    throw new Error(`missing required number argument: ${key}`);
  }
  return v;
}

export function optionalNumber(args: Record<string, unknown>, key: string): number | undefined {
  const v = args[key];
  if (v === undefined || v === null) return undefined;
  if (typeof v !== "number" || !Number.isFinite(v)) {
    throw new Error(`argument ${key} must be a number`);
  }
  return v;
}

export function optionalBoolean(args: Record<string, unknown>, key: string): boolean | undefined {
  const v = args[key];
  if (v === undefined || v === null) return undefined;
  if (typeof v !== "boolean") {
    throw new Error(`argument ${key} must be a boolean`);
  }
  return v;
}
