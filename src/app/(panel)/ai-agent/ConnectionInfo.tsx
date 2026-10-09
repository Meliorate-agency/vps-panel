"use client";

import Link from "next/link";
import { useState } from "react";
import { copyText } from "@/lib/clipboard";
import pageStyles from "@/components/page.module.css";
import local from "./page.module.css";

type Props = {
  /** Whether a token is set (DB or env). When false, the endpoint
   *  returns 503 to clients and we explain how to enable it. */
  enabled: boolean;
  /** Fully-qualified URL of the MCP endpoint, e.g.
   *  https://panel.example.com/api/mcp. */
  endpoint: string;
  /** Bearer-token to embed in the Claude Desktop config snippet. The
   *  DB-stored plaintext when source==="db"; the literal string
   *  `<YOUR_MCP_TOKEN>` when source==="env" (we don't surface
   *  process.env); null when MCP is disabled. */
  token: string | null;
  /** Where the active token came from, for the source pill. */
  tokenSource: "db" | "env" | null;
};

/**
 * Top card on /ai-agent. Shows whether MCP is enabled, the endpoint URL,
 * the active bearer token (when DB-managed), and one-click copy buttons
 * for the raw URL + a ready-to-paste Claude Desktop config snippet.
 */
export default function ConnectionInfo({ enabled, endpoint, token, tokenSource }: Props) {
  const [copied, setCopied] = useState<"url" | "snippet" | null>(null);

  const snippet = `{
  "mcpServers": {
    "vps-panel": {
      "url": "${endpoint}",
      "headers": {
        "Authorization": "Bearer ${token ?? "<YOUR_MCP_TOKEN>"}"
      }
    }
  }
}`;

  function copy(kind: "url" | "snippet", text: string) {
    void copyText(text).then((ok) => {
      // Not copied: the text is on-screen for manual copy.
      if (!ok) return;
      setCopied(kind);
      window.setTimeout(() => setCopied(null), 1500);
    });
  }

  return (
    <div className={pageStyles.section}>
      <h2 className={pageStyles.sectionTitle}>Connection</h2>

      <div className={local.statusRow}>
        <span
          className={`${local.pill} ${enabled ? local.pillOk : local.pillOff}`}
          title={
            enabled
              ? "MCP_TOKEN is set; the endpoint accepts authenticated requests."
              : "MCP_TOKEN is not set — the endpoint returns 503."
          }
        >
          <span
            className={`${local.pillDot} ${enabled ? local.pillDotOk : local.pillDotOff}`}
            aria-hidden="true"
          />
          {enabled ? "Enabled" : "Disabled"}
        </span>
        {!enabled && (
          <span className={pageStyles.muted} style={{ fontSize: 12 }}>
            No token set.{" "}
            <Link href="/settings#mcp-token" className={pageStyles.code}>
              Generate one in Settings →
            </Link>
          </span>
        )}
        {enabled && tokenSource === "env" && (
          <span className={pageStyles.muted} style={{ fontSize: 12 }}>
            Token sourced from <span className={pageStyles.code}>MCP_TOKEN</span>{" "}
            env var (legacy).{" "}
            <Link href="/settings#mcp-token" className={pageStyles.code}>
              Migrate to Settings →
            </Link>
          </span>
        )}
        {enabled && tokenSource === "db" && (
          <span className={pageStyles.muted} style={{ fontSize: 12 }}>
            Token managed in{" "}
            <Link href="/settings#mcp-token" className={pageStyles.code}>
              Settings → MCP token
            </Link>
          </span>
        )}
      </div>

      <div className={local.kvRow}>
        <span className={local.kvLabel}>Endpoint</span>
        <span className={local.kvValue}>{endpoint}</span>
        <span className={local.kvLabel}>Auth</span>
        <span className={local.kvValue}>
          Authorization: Bearer &lt;MCP_TOKEN&gt;
        </span>
        <span className={local.kvLabel}>Transport</span>
        <span className={local.kvValue}>
          HTTP POST, JSON-RPC 2.0 (MCP Streamable HTTP, protocol 2025-06-18)
        </span>
      </div>

      <div className={local.copyRow}>
        <button
          type="button"
          className={pageStyles.btnGhost}
          onClick={() => copy("url", endpoint)}
        >
          {copied === "url" ? "Copied!" : "Copy endpoint URL"}
        </button>
        <button
          type="button"
          className={pageStyles.btnGhost}
          onClick={() => copy("snippet", snippet)}
        >
          {copied === "snippet" ? "Copied!" : "Copy Claude Desktop config"}
        </button>
      </div>

      <pre className={local.snippet}>{snippet}</pre>

      <p className={pageStyles.muted} style={{ fontSize: 12, marginTop: 10, lineHeight: 1.55 }}>
        Paste this into <span className={pageStyles.code}>~/Library/Application Support/Claude/claude_desktop_config.json</span>{" "}
        (macOS) or <span className={pageStyles.code}>%APPDATA%\Claude\claude_desktop_config.json</span>{" "}
        (Windows){tokenSource === "env"
          ? ", substitute the token,"
          : ""}{" "}
        then restart Claude Desktop. The tools listed below will appear in
        Claude&apos;s tool picker.
      </p>
    </div>
  );
}
