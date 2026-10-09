"use client";

import { useState, useTransition } from "react";
import styles from "@/components/page.module.css";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";
import StatusBadge from "@/components/StatusBadge/StatusBadge";

type ActionResult = { ok: true } | { ok: false; error: string };

/**
 * Per-database pgvector control shown in the Postgres databases table.
 *
 * Three states:
 *  - package not installed on the server (`available === false`) → disabled
 *    Enable button pointing at the install card; the extension can't be
 *    created until the apt package lands.
 *  - installed but not enabled on this DB → Enable button (CREATE EXTENSION).
 *  - enabled → green badge + Disable (DROP EXTENSION, behind a confirm because
 *    it can fail when vector columns still depend on it).
 */
export default function PgVectorButton({
  database,
  enabled,
  available,
  enableAction,
  disableAction,
}: {
  database: string;
  enabled: boolean;
  available: boolean;
  enableAction: (database: string) => Promise<ActionResult>;
  disableAction: (database: string) => Promise<ActionResult>;
}) {
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [pending, start] = useTransition();

  const onEnable = () => {
    if (pending) return;
    setError(null);
    start(async () => {
      const res = await enableAction(database);
      if (!res.ok) setError(res.error);
    });
  };

  const onDisable = () => {
    if (pending) return;
    setError(null);
    start(async () => {
      const res = await disableAction(database);
      if (res.ok) {
        setConfirmOpen(false);
      } else {
        setError(res.error);
      }
    });
  };

  // Package not installed yet — nothing to enable. The install card in the
  // strip above is the way to get the package; spell that out in the tooltip.
  if (!available) {
    return (
      <button
        type="button"
        className={styles.btnGhost}
        disabled
        title="Install the pgvector package from the strip above first."
      >
        Enable
      </button>
    );
  }

  if (!enabled) {
    return (
      <div
        style={{
          display: "flex",
          flexDirection: "column",
          gap: 4,
          alignItems: "flex-start",
        }}
      >
        <button
          type="button"
          className={styles.btnGhost}
          onClick={onEnable}
          disabled={pending}
        >
          {pending ? "Enabling…" : "Enable"}
        </button>
        {error && (
          <span className={styles.error} style={{ fontSize: 12 }}>
            {error}
          </span>
        )}
      </div>
    );
  }

  return (
    <>
      <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
        <StatusBadge text="enabled" tone="ok" />
        <button
          type="button"
          className={styles.btnGhost}
          onClick={() => {
            setError(null);
            setConfirmOpen(true);
          }}
          disabled={pending}
        >
          Disable
        </button>
      </div>

      <ConfirmDialog
        open={confirmOpen}
        title={`Disable pgvector on ${database}`}
        confirmLabel={pending ? "Disabling…" : "Disable"}
        cancelLabel="Cancel"
        pending={pending}
        onConfirm={onDisable}
        onCancel={() => {
          if (pending) return;
          setConfirmOpen(false);
        }}
      >
        <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
          <p style={{ margin: 0 }}>
            This runs <code className={styles.code}>DROP EXTENSION vector</code>{" "}
            on <span className={styles.code}>{database}</span>. It fails if
            tables still use <code className={styles.code}>vector</code> columns
            — drop those columns first. The pgvector package stays installed and
            can be re-enabled anytime.
          </p>
          {error && <div className={styles.error}>{error}</div>}
        </div>
      </ConfirmDialog>
    </>
  );
}
