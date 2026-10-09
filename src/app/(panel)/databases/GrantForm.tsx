"use client";

import { useActionState, useState } from "react";
import styles from "@/components/page.module.css";
import type { DbUser } from "@/lib/agent";

type Result = { ok: true } | { ok: false; error: string };

/**
 * Grant MariaDB privileges — mirrors PgGrantForm: both the user and the
 * database are dropdowns fed from the live lists, because granting only
 * makes sense against things that exist (modern MariaDB errors rather than
 * auto-creating a user on GRANT). A MariaDB "user" is a user@host pair, so
 * picking one row carries the host too — that's what replaced the old
 * free-text user + host-preset fields. The selection feeds hidden inputs so
 * the server action's FormData contract (user / host / database /
 * privileges) is unchanged.
 */
export default function GrantForm({
  action,
  users,
  databases,
}: {
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
  users: DbUser[];
  databases: { name: string }[];
}) {
  const [state, formAction, pending] = useActionState<Result | null, FormData>(action, null);
  // The select's value is an index into `users` — immune to any separator
  // character legally appearing in a user or host name.
  const [sel, setSel] = useState("");
  const pair = sel === "" ? null : users[Number(sel)];

  return (
    <form action={formAction}>
      <div className={styles.formRow}>
        <label className={styles.field}>
          User
          <select
            className={styles.select}
            value={sel}
            onChange={(e) => setSel(e.target.value)}
            required
            disabled={users.length === 0}
          >
            <option value="" disabled>
              {users.length === 0 ? "No users yet — create one above" : "Pick a user…"}
            </option>
            {users.map((u, i) => (
              <option key={`${u.user}@${u.host}`} value={String(i)}>
                {u.user}@{u.host}
              </option>
            ))}
          </select>
          <input type="hidden" name="user" value={pair?.user ?? ""} />
          <input type="hidden" name="host" value={pair?.host ?? ""} />
        </label>
        <label className={styles.field}>
          Database
          <select
            name="database"
            className={styles.select}
            required
            defaultValue=""
            disabled={databases.length === 0}
          >
            <option value="" disabled>
              {databases.length === 0
                ? "No databases yet — create or import one above"
                : "Pick a database…"}
            </option>
            {databases.map((d) => (
              <option key={d.name} value={d.name}>
                {d.name}
              </option>
            ))}
          </select>
        </label>
        <label className={styles.field}>
          Privileges
          <select name="privileges" className={styles.select} defaultValue="ALL PRIVILEGES">
            <option>ALL PRIVILEGES</option>
            <option>SELECT, INSERT, UPDATE, DELETE</option>
            <option>SELECT</option>
          </select>
        </label>
      </div>
      {state?.ok === false && <div className={styles.error}>{state.error}</div>}
      {state?.ok === true && <div className={styles.notice}>Privileges granted.</div>}
      <button
        type="submit"
        className={styles.btn}
        disabled={pending || !pair || databases.length === 0}
      >
        {pending ? "..." : "Grant"}
      </button>
    </form>
  );
}
