"use client";

import { useActionState } from "react";
import styles from "@/components/page.module.css";
import type { PgDatabase, PgRole } from "@/lib/agent";

type Result = { ok: true } | { ok: false; error: string };

/**
 * Grant Postgres privileges on a database to a role. The agent's grant
 * handler actually issues a small bundle of statements (database-level
 * grant + schema grant + table/sequence grants + default-privilege
 * grants) because Postgres' permission model is layered — granting on
 * the database alone doesn't let the role create or query tables.
 * Operators just see the top-level privilege choice; the cascade is
 * implicit.
 */
export default function PgGrantForm({
  action,
  roles,
  databases,
}: {
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
  roles: PgRole[];
  databases: PgDatabase[];
}) {
  const [state, formAction, pending] = useActionState<Result | null, FormData>(
    action,
    null,
  );

  return (
    <form action={formAction}>
      <div className={styles.formRow}>
        <label className={styles.field}>
          Role
          {roles.length > 0 ? (
            <select name="user" className={styles.select} required defaultValue="">
              <option value="" disabled>
                Pick a role…
              </option>
              {roles.map((r) => (
                <option key={r.user} value={r.user}>
                  {r.user}
                </option>
              ))}
            </select>
          ) : (
            <input
              name="user"
              className={styles.input}
              required
              placeholder="appuser"
            />
          )}
        </label>
        <label className={styles.field}>
          Database
          {databases.length > 0 ? (
            <select name="database" className={styles.select} required defaultValue="">
              <option value="" disabled>
                Pick a database…
              </option>
              {databases.map((d) => (
                <option key={d.name} value={d.name}>
                  {d.name}
                </option>
              ))}
            </select>
          ) : (
            <input
              name="database"
              className={styles.input}
              required
              placeholder="myapp_prod"
            />
          )}
        </label>
        <label className={styles.field}>
          Privileges
          <select
            name="privileges"
            className={styles.select}
            defaultValue="ALL PRIVILEGES"
          >
            <option>ALL PRIVILEGES</option>
            <option>SELECT, INSERT, UPDATE, DELETE</option>
            <option>SELECT</option>
            <option>CONNECT</option>
          </select>
          <span className={styles.muted} style={{ fontSize: 12, marginTop: 4 }}>
            ALL PRIVILEGES also cascades into the public schema and existing
            tables/sequences so the role can actually use the database.
          </span>
        </label>
      </div>
      {state?.ok === false && <div className={styles.error}>{state.error}</div>}
      {state?.ok === true && (
        <div className={styles.notice}>Privileges granted.</div>
      )}
      <button type="submit" className={styles.btn} disabled={pending}>
        {pending ? "..." : "Grant"}
      </button>
    </form>
  );
}
