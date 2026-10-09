"use client";

import { useState, useTransition } from "react";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";
import styles from "@/components/page.module.css";
import type { DbGrantedUser } from "@/lib/agent";
import type { ActionResult } from "@/server-actions/databases";

type Props = {
  database: string;
  users: DbGrantedUser[];
  /** revokeDbGrantAction — (user, host, database) → ActionResult. */
  revokeAction: (
    user: string,
    host: string,
    database: string,
  ) => Promise<ActionResult>;
};

const GLOBAL_GRANTS_NOTE =
  "Users with db-level grants on this database. Users granted globally (ON *.*, e.g. via phpMyAdmin's global privileges) hold access too but are not listed per-database.";

/**
 * The "Granted users" cell of the MariaDB databases table: one chip per
 * user@host with db-level grants, each with a × that revokes after a danger
 * confirm. The revoke action revalidates /databases, so a successful revoke
 * re-renders the row without the chip — no local list state to maintain.
 */
export default function GrantedUsers({ database, users, revokeAction }: Props) {
  const [target, setTarget] = useState<DbGrantedUser | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pending, startTransition] = useTransition();

  if (users.length === 0) {
    return (
      <span className={styles.muted} title={GLOBAL_GRANTS_NOTE}>
        —
      </span>
    );
  }

  const confirm = () => {
    if (!target) return;
    setError(null);
    startTransition(async () => {
      const res = await revokeAction(target.user, target.host, database);
      if (res.ok) {
        setTarget(null);
      } else {
        setError(res.error);
      }
    });
  };

  return (
    <div
      style={{ display: "flex", flexWrap: "wrap", gap: 6 }}
      title={GLOBAL_GRANTS_NOTE}
    >
      {users.map((u) => (
        <span
          key={`${u.user}@${u.host}`}
          className={styles.code}
          style={{
            display: "inline-flex",
            alignItems: "center",
            gap: 4,
            whiteSpace: "nowrap",
          }}
        >
          {u.user}@{u.host}
          <button
            type="button"
            aria-label={`Revoke ${u.user}@${u.host} on ${database}`}
            title={`Revoke ${u.user}@${u.host}'s access to ${database}`}
            onClick={() => {
              setError(null);
              setTarget(u);
            }}
            style={{
              border: "none",
              background: "none",
              cursor: "pointer",
              color: "inherit",
              opacity: 0.55,
              padding: 0,
              fontSize: 12,
              lineHeight: 1,
            }}
          >
            ✕
          </button>
        </span>
      ))}

      <ConfirmDialog
        open={target !== null}
        tone="danger"
        title={`Revoke access to ${database}?`}
        confirmLabel="Revoke"
        cancelLabel="Cancel"
        pending={pending}
        lockDismiss={pending}
        onConfirm={confirm}
        onCancel={() => {
          if (pending) return;
          setTarget(null);
          setError(null);
        }}
      >
        <p style={{ marginTop: 0 }}>
          This removes all of{" "}
          <span className={styles.code}>
            {target?.user}@{target?.host}
          </span>
          &apos;s privileges on{" "}
          <span className={styles.code}>{database}</span>. An app connecting
          with this user will lose access to the database immediately. The
          user itself is kept.
        </p>
        {error && <div className={styles.error}>{error}</div>}
      </ConfirmDialog>
    </div>
  );
}
