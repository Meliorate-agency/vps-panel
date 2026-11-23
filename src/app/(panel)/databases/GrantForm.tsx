"use client";

import { useActionState, useState, useTransition } from "react";
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
  // The select's value is the JSON of [user, host] — immune to any separator
  // character legally appearing in a user or host name. Not an index into
  // `users`: a user created or dropped on this page re-renders the list, and
  // the index then named someone else.
  const [sel, setSel] = useState("");
  const keyOf = (u: DbUser) => JSON.stringify([u.user, u.host]);
  const pair = users.find((u) => keyOf(u) === sel) ?? null;

  // Submitted by hand rather than through <form action>: React resets a form
  // after its action ran, which showed the first user in the User select
  // while the hidden fields still carried the picked one, so the next grant
  // went to a user the form did not show.
  const [, startTransition] = useTransition();
  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    const fd = new FormData(e.currentTarget);
    startTransition(() => formAction(fd));
  }

  return (
    <form onSubmit={onSubmit}>
      <div className={styles.formRow}>
        <label className={styles.field}>
          User
          <select
            className={styles.select}
            value={pair ? sel : ""}
            onChange={(e) => setSel(e.target.value)}
            required
            disabled={users.length === 0}
          >
            <option value="" disabled>
              {users.length === 0 ? "No users yet — create one above" : "Pick a user…"}
            </option>
            {users.map((u) => (
              <option key={keyOf(u)} value={keyOf(u)}>
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
