"use client";

import { useActionState } from "react";
import styles from "@/components/page.module.css";
import type { PgRole } from "@/lib/agent";

type Result = { ok: true } | { ok: false; error: string };

/**
 * Create a Postgres database. Optionally assigns ownership to an
 * existing role at creation time — recommended, because a database
 * owned by `postgres` requires an extra schema grant before the
 * application role can create tables. The owner dropdown is pre-filled
 * with every login role the panel currently sees.
 */
export default function CreatePgDbForm({
  action,
  roles,
}: {
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
  roles: PgRole[];
}) {
  const [state, formAction, pending] = useActionState<Result | null, FormData>(
    action,
    null,
  );
  return (
    <form
      action={formAction}
      style={{ display: "flex", gap: 8, alignItems: "flex-end", flexWrap: "wrap" }}
    >
      <label className={styles.field} style={{ flex: "1 1 200px" }}>
        Name
        <input
          name="name"
          className={styles.input}
          required
          placeholder="myapp_prod"
          autoComplete="off"
          pattern="^[A-Za-z_][A-Za-z0-9_]{0,62}$"
          title="lowercase letters, digits, underscores; must start with a letter"
        />
      </label>
      <label className={styles.field} style={{ flex: "1 1 180px" }}>
        Owner role
        <select name="owner" className={styles.select} defaultValue="">
          <option value="">postgres (default — needs schema grant later)</option>
          {roles.map((r) => (
            <option key={r.user} value={r.user}>
              {r.user}
              {r.superuser ? " · superuser" : ""}
            </option>
          ))}
        </select>
      </label>
      <button type="submit" className={styles.btn} disabled={pending}>
        {pending ? "..." : "Create Postgres database"}
      </button>
      {state?.ok === false && <div className={styles.error}>{state.error}</div>}
    </form>
  );
}
