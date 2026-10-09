"use client";

import type { Tool, ToolProperty } from "@/lib/mcp/types";
import local from "./page.module.css";

type PreviewTool = Omit<Tool, "call">;

type Props = {
  groups: { page: string; tools: PreviewTool[] }[];
};

/**
 * Collapsible per-page browser of every MCP tool the panel exposes. Each
 * page is one `<details>` (defaulting collapsed); expanding it shows a
 * table per tool with name, type, required/optional, and description.
 */
export default function ToolPreview({ groups }: Props) {
  return (
    <div>
      {groups.map((g) => (
        <details key={g.page} className={local.pageSection}>
          <summary>
            <svg
              className={local.summaryCaret}
              viewBox="0 0 12 12"
              fill="none"
              aria-hidden="true"
            >
              <path
                d="M4 2 L8 6 L4 10"
                stroke="currentColor"
                strokeWidth="1.8"
                strokeLinecap="round"
                strokeLinejoin="round"
              />
            </svg>
            {g.page}
            <span className={local.summaryCount}>
              {g.tools.length} {g.tools.length === 1 ? "tool" : "tools"}
            </span>
          </summary>
          <div className={local.toolList}>
            {g.tools.map((t) => (
              <ToolBlock key={t.name} tool={t} />
            ))}
          </div>
        </details>
      ))}
    </div>
  );
}

function ToolBlock({ tool }: { tool: PreviewTool }) {
  const props = tool.inputSchema.properties ?? {};
  const required = new Set(tool.inputSchema.required ?? []);
  const entries = Object.entries(props);

  return (
    <div className={local.tool}>
      <div className={local.toolHead}>
        <span className={local.toolName}>{tool.name}</span>
        <span className={local.summaryCount} title={tool.readOnly ? "Read-only tokens can call this" : "Needs a full-access token"}>
          {tool.readOnly ? "read-only OK" : "full access"}
        </span>
      </div>
      <div className={local.toolDesc}>{tool.description}</div>
      {entries.length === 0 ? (
        <div className={local.noArgs}>No arguments.</div>
      ) : (
        <table className={local.argTable}>
          <thead>
            <tr>
              <th style={{ width: "22%" }}>Arg</th>
              <th style={{ width: "18%" }}>Type</th>
              <th style={{ width: "10%" }}>Required</th>
              <th>Description</th>
            </tr>
          </thead>
          <tbody>
            {entries.map(([name, schema]) => (
              <PropRow
                key={name}
                name={name}
                schema={schema as ToolProperty}
                required={required.has(name)}
              />
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}

function PropRow({
  name,
  schema,
  required,
}: {
  name: string;
  schema: ToolProperty;
  required: boolean;
}) {
  const typeLabel = typeDescription(schema);
  return (
    <tr>
      <td className={local.argName}>
        {name}
        {schema.sensitive && <span className={local.argSensitive}>sensitive</span>}
      </td>
      <td className={local.argType}>{typeLabel}</td>
      <td>
        {required ? (
          <span className={local.argReq}>required</span>
        ) : (
          <span className={local.argOpt}>optional</span>
        )}
      </td>
      <td>
        {schema.description ?? <span className={local.argOpt}>—</span>}
        {schema.enum && (
          <div style={{ marginTop: 4, fontSize: 11, color: "var(--text-dim)" }}>
            One of: {schema.enum.map((v) => JSON.stringify(v)).join(", ")}
          </div>
        )}
        {schema.default !== undefined && (
          <div style={{ marginTop: 4, fontSize: 11, color: "var(--text-dim)" }}>
            Default: <code>{JSON.stringify(schema.default)}</code>
          </div>
        )}
      </td>
    </tr>
  );
}

/** Produce a compact JSON-Schema-flavoured type description like
 *  `array<{ key, value }>` or `integer (1024..65535)`. */
function typeDescription(schema: ToolProperty): string {
  if (schema.type === "array") {
    const items = schema.items;
    if (items && "type" in items && items.type === "object") {
      const keys = Object.keys(items.properties ?? {}).join(", ");
      return `array<{ ${keys} }>`;
    }
    if (items && "type" in items) {
      return `array<${items.type}>`;
    }
    return "array";
  }
  if (schema.type === "object") {
    const keys = Object.keys(schema.properties ?? {}).join(", ");
    return keys ? `object<{ ${keys} }>` : "object";
  }
  if (typeof schema.minimum === "number" || typeof schema.maximum === "number") {
    const lo = schema.minimum ?? "";
    const hi = schema.maximum ?? "";
    return `${schema.type} (${lo}..${hi})`;
  }
  return schema.type;
}
