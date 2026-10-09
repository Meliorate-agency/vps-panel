"use client";

import { useEffect, useState, useTransition } from "react";
import { copyText } from "@/lib/clipboard";
import styles from "@/components/page.module.css";
import type { McpScope, McpTokenInfo } from "@/lib/mcpToken";
import type { GenerateResult } from "@/server-actions/mcpToken";

type Result = { ok: true } | { ok: false; error: string };

type Props = {
  /** The active token's metadata; null when MCP is off. Never the token. */
  info: McpTokenInfo | null;
  /** Whether MCP_TOKEN is set in .env (takes over when the panel's is cleared). */
  envFallbackAvailable: boolean;
  generateAction: (scope: McpScope) => Promise<GenerateResult>;
  setScopeAction: (scope: McpScope) => Promise<Result>;
  clearAction: () => Promise<Result>;
};

const SCOPES: { value: McpScope; label: string; help: string }[] = [
  {
    value: "read",
    label: "Read-only",
    help: "Lists and status only. No changes, no .env contents, no task logs.",
  },
  {
    value: "full",
    label: "Full access",
    help: "Everything the MCP tools can do: deploy, restart, env files, DNS, mail, firewall.",
  },
];

/**
 * Settings → MCP token. Only a hash of the token is stored, so the token is
 * shown once, right after it is generated; the form otherwise shows its
 * last four characters and its scope.
 */
export default function McpTokenForm({
  info,
  envFallbackAvailable,
  generateAction,
  setScopeAction,
  clearAction,
}: Props) {
  const [scope, setScope] = useState<McpScope>(info?.scope ?? "read");
  const [fresh, setFresh] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [busy, startBusy] = useTransition();
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => {
    if (info) setScope(info.scope);
  }, [info]);

  const run = (fn: () => Promise<void>) => {
    if (busy) return;
    setError(null);
    setNotice(null);
    startBusy(fn);
  };

  function generate() {
    if (
      info &&
      !window.confirm(
        "Replace the MCP token? Every connected AI client stops working until you paste the new one into its config.",
      )
    ) {
      return;
    }
    run(async () => {
      const r = await generateAction(scope);
      if (!r.ok) return setError(r.error);
      setFresh(r.token);
      setCopied(false);
    });
  }

  function saveScope() {
    run(async () => {
      const r = await setScopeAction(scope);
      if (!r.ok) return setError(r.error);
      setNotice("Scope saved. It applies to the next request.");
    });
  }

  function clear() {
    if (
      !window.confirm(
        envFallbackAvailable
          ? "Remove the panel's MCP token? MCP_TOKEN from .env takes over."
          : "Remove the MCP token? MCP is off until you generate a new one.",
      )
    ) {
      return;
    }
    run(async () => {
      const r = await clearAction();
      if (!r.ok) return setError(r.error);
      setFresh(null);
    });
  }

  function copy() {
    if (!fresh) return;
    void copyText(fresh).then((ok) => {
      // Not copied: the field can still be selected by hand.
      if (ok) setCopied(true);
    });
  }

  const active = info !== null;
  const scopeChanged = info?.source === "db" && scope !== info.scope;

  return (
    <div>
      <div style={{ display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap", marginBottom: 12 }}>
        <span
          style={{
            display: "inline-flex",
            alignItems: "center",
            padding: "3px 9px",
            borderRadius: 999,
            fontSize: 11,
            fontWeight: 600,
            letterSpacing: "0.04em",
            textTransform: "uppercase",
            background: active ? "rgba(84, 209, 140, 0.12)" : "var(--danger-bg)",
            border: `1px solid ${active ? "rgba(84, 209, 140, 0.45)" : "var(--danger-border)"}`,
            color: active ? "var(--ok)" : "var(--danger)",
          }}
        >
          {active ? "Active" : "Off"}
        </span>
        {info?.source === "db" && (
          <span className={styles.muted} style={{ fontSize: 12 }}>
            Token ending in <span className={styles.code}>{info.hint}</span>
            {info.createdAt && <> · created {info.createdAt.slice(0, 10)}</>}
          </span>
        )}
        {info?.source === "env" && (
          <span className={styles.muted} style={{ fontSize: 12 }}>
            Using <span className={styles.code}>MCP_TOKEN</span> from{" "}
            <span className={styles.code}>/etc/vps-panel/.env</span> (full access). Generate a
            token here to replace it with one you can scope.
          </span>
        )}
      </div>

      <fieldset style={{ border: "none", padding: 0, margin: "0 0 12px", display: "grid", gap: 6 }}>
        {SCOPES.map((s) => (
          <label key={s.value} style={{ display: "flex", gap: 8, alignItems: "baseline", fontSize: 13 }}>
            <input
              type="radio"
              name="mcp-scope"
              value={s.value}
              checked={scope === s.value}
              onChange={() => setScope(s.value)}
              disabled={busy}
            />
            <span>
              <strong>{s.label}</strong> <span className={styles.muted}>{s.help}</span>
            </span>
          </label>
        ))}
      </fieldset>

      {fresh && (
        <div className={styles.notice} style={{ marginBottom: 12 }}>
          <div style={{ marginBottom: 6 }}>
            Copy the new token now. Only a hash of it is stored, so it cannot be shown again.
          </div>
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
            <input
              readOnly
              value={fresh}
              className={styles.input}
              style={{ flex: "1 1 320px", minWidth: 0, fontFamily: "var(--mono)", fontSize: 12 }}
              onFocus={(e) => e.target.select()}
            />
            <button type="button" className={styles.btnGhost} onClick={copy}>
              {copied ? "Copied!" : "Copy"}
            </button>
          </div>
        </div>
      )}

      <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
        <button type="button" className={styles.btn} onClick={generate} disabled={busy}>
          {busy ? "..." : active ? `Generate new ${scope === "read" ? "read-only" : "full-access"} token` : "Generate token"}
        </button>
        {scopeChanged && (
          <button type="button" className={styles.btnGhost} onClick={saveScope} disabled={busy}>
            Keep this token, change scope
          </button>
        )}
        {info?.source === "db" && (
          <button type="button" className={styles.btnGhost} onClick={clear} disabled={busy}>
            Remove token
          </button>
        )}
      </div>

      {notice && <div className={styles.notice} style={{ marginTop: 10 }}>{notice}</div>}
      {error && <div className={styles.error} style={{ marginTop: 10 }}>{error}</div>}
    </div>
  );
}
