"use client";

import { useState, useTransition } from "react";
import styles from "@/components/page.module.css";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";
import type { PgDatabase } from "@/lib/agent";

type ActionResult = { ok: true } | { ok: false; error: string };

const IDENT_RE = /^[A-Za-z_][A-Za-z0-9_]{0,62}$/;

/**
 * Edit a Postgres database: rename and/or change owner, with Save/Cancel in
 * the centered ConfirmDialog. Only the fields that actually changed are sent
 * to the server action, so leaving the owner on `postgres` (a reserved role
 * the agent would otherwise refuse) is a no-op rather than an error.
 *
 * Owner change is database-level only (ALTER DATABASE … OWNER TO) — the hint
 * spells out that existing tables keep their owner, matching pgAdmin.
 */
export default function PgDbEditButton({
  db,
  roles,
  action,
}: {
  db: PgDatabase;
  roles: string[];
  action: (
    currentName: string,
    changes: { newName?: string; owner?: string },
  ) => Promise<ActionResult>;
}) {
  const [open, setOpen] = useState(false);
  const [name, setName] = useState(db.name);
  const [owner, setOwner] = useState(db.owner);
  const [error, setError] = useState<string | null>(null);
  const [pending, start] = useTransition();

  // The current owner may be a reserved role (e.g. postgres) that isn't in
  // the login-role list — include it so it shows as selected.
  const ownerOptions = Array.from(new Set([db.owner, ...roles]));

  const trimmed = name.trim();
  const nameValid = IDENT_RE.test(trimmed);
  const dirty = trimmed !== db.name || owner !== db.owner;

  const openDialog = () => {
    setName(db.name);
    setOwner(db.owner);
    setError(null);
    setOpen(true);
  };

  const onSave = () => {
    if (pending) return;
    // Nothing changed → treat Save as a plain dismiss.
    if (!dirty) {
      setOpen(false);
      return;
    }
    if (!nameValid) {
      setError(
        "Invalid name — letters, digits, underscores; must start with a letter or underscore.",
      );
      return;
    }
    setError(null);
    start(async () => {
      const res = await action(db.name, {
        newName: trimmed !== db.name ? trimmed : undefined,
        owner: owner !== db.owner ? owner : undefined,
      });
      if (res.ok) {
        setOpen(false);
      } else {
        setError(res.error);
      }
    });
  };

  return (
    <>
      <button
        type="button"
        className={styles.btnGhost}
        onClick={openDialog}
        disabled={pending}
      >
        Edit
      </button>

      <ConfirmDialog
        open={open}
        title={`Edit database ${db.name}`}
        confirmLabel={pending ? "Saving…" : "Save"}
        cancelLabel="Cancel"
        pending={pending}
        onConfirm={onSave}
        onCancel={() => {
          if (pending) return;
          setOpen(false);
        }}
      >
        <div style={{ display: "flex", flexDirection: "column", gap: 14 }}>
          <label className={styles.field}>
            Name
            <input
              className={styles.input}
              value={name}
              onChange={(e) => setName(e.target.value)}
              autoComplete="off"
              pattern="^[A-Za-z_][A-Za-z0-9_]{0,62}$"
              title="letters, digits, underscores; must start with a letter or underscore"
            />
            {trimmed.length > 0 && !nameValid && (
              <span className={styles.muted} style={{ fontSize: 12, marginTop: 4 }}>
                Invalid name — letters, digits, underscores; must start with a
                letter or underscore.
              </span>
            )}
          </label>

          <label className={styles.field}>
            Owner
            <select
              className={styles.select}
              value={owner}
              onChange={(e) => setOwner(e.target.value)}
            >
              {ownerOptions.map((o) => (
                <option key={o} value={o}>
                  {o}
                </option>
              ))}
            </select>
            <span className={styles.muted} style={{ fontSize: 12, marginTop: 4 }}>
              Database-level owner only — existing tables keep their current
              owner. Use the Grant form to give a role access to those.
            </span>
          </label>

          {error && <div className={styles.error}>{error}</div>}
        </div>
      </ConfirmDialog>
    </>
  );
}
