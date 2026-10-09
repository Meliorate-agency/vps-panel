"use client";

import { useActionState, useState } from "react";
import styles from "@/components/page.module.css";

// Mirror of the agent's RESERVED_USERS list in agent/src/handlers/postgres.rs
// (plus the pg_* prefix rule). Client-side check keeps the round-trip down
// and surfaces inline feedback; the agent re-enforces server-side.
const RESERVED_USERS = ["postgres", "rdsadmin"];

type Result =
  | { ok: true; data?: { password: string; user: string } }
  | { ok: false; error: string };

/**
 * Create a Postgres login role with an optional password (server
 * generates one if omitted) and an optional SUPERUSER flag. No host
 * field — Postgres doesn't tie roles to hosts; access from outside
 * localhost is gated by pg_hba.conf, which the panel doesn't touch.
 */
export default function CreatePgUserForm({
  action,
}: {
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
}) {
  const [state, formAction, pending] = useActionState<Result | null, FormData>(
    action,
    null,
  );
  const [user, setUser] = useState("");

  const lowered = user.trim().toLowerCase();
  const userIsReserved =
    RESERVED_USERS.includes(lowered) || lowered.startsWith("pg_");

  return (
    <form action={formAction}>
      <div className={styles.formRow}>
        <label className={styles.field}>
          Role name
          <input
            name="user"
            className={styles.input}
            required
            placeholder="appuser"
            value={user}
            onChange={(e) => setUser(e.target.value)}
            autoComplete="off"
            pattern="^[A-Za-z_][A-Za-z0-9_]{0,62}$"
            title="letters, digits, underscores; must start with a letter or underscore"
          />
          {userIsReserved && (
            <span className={styles.muted}>
              <code className={styles.code}>{user.trim()}</code> is reserved
              by Postgres — pick a different name.
            </span>
          )}
        </label>
        <label className={styles.field}>
          Password (optional)
          <input
            name="password"
            className={styles.input}
            type="text"
            placeholder="leave blank to generate"
            autoComplete="off"
          />
          <span className={styles.muted} style={{ fontSize: 12, marginTop: 4 }}>
            Stored hashed by Postgres (SCRAM-SHA-256). The panel only sees
            it on creation, then forgets it.
          </span>
        </label>
        <label
          className={styles.checkbox}
          style={{ alignSelf: "flex-end", marginBottom: 6 }}
        >
          <input type="checkbox" name="superuser" />
          Grant SUPERUSER
          <span className={styles.muted} style={{ fontSize: 12, marginLeft: 6 }}>
            (full admin — rarely what an app role wants)
          </span>
        </label>
      </div>
      {state?.ok === false && <div className={styles.error}>{state.error}</div>}
      {state?.ok === true && state.data && (
        <div className={styles.notice}>
          Created <code className={styles.code}>{state.data.user}</code> with
          password <code className={styles.code}>{state.data.password}</code>
        </div>
      )}
      <button
        type="submit"
        className={styles.btn}
        disabled={pending || userIsReserved}
      >
        {pending ? "..." : "Create Postgres role"}
      </button>
    </form>
  );
}
