"use client";

import { useActionState } from "react";
import styles from "@/components/page.module.css";

type Result = { ok: true } | { ok: false; error: string };

type Props = {
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
  /** Linux users (from /etc/passwd) the agent considers cron-eligible.
   *  Used as a dropdown of valid user values. Always pre-pended with the
   *  default "root" if the list came back empty (e.g. agent-down case). */
  users: { name: string; uid: number; home: string }[];
};

export default function CronForm({ action, users }: Props) {
  const [state, formAction, pending] = useActionState<Result | null, FormData>(
    action,
    null,
  );

  // Fallback so the dropdown is always usable even if the agent's
  // /system/users call failed during page render.
  const choices = users.length > 0 ? users : [{ name: "root", uid: 0, home: "/root" }];

  return (
    <form action={formAction}>
      <div className={styles.formRow}>
        <label className={styles.field}>
          Schedule
          <input
            name="schedule"
            className={styles.input}
            required
            placeholder="*/15 * * * *"
          />
        </label>
        <label className={styles.field}>
          User
          <select name="user" className={styles.select} defaultValue="root">
            {choices.map((u) => (
              <option key={u.name} value={u.name}>
                {u.name}
                {u.uid === 0 ? " (root)" : u.home ? ` — ${u.home}` : ""}
              </option>
            ))}
          </select>
          <span className={styles.muted} style={{ fontSize: 12, marginTop: 4 }}>
            The Linux user the cron daemon executes the command as. Pick one
            that exists on this host — the agent verifies existence against{" "}
            <span className={styles.code}>/etc/passwd</span> before writing.
          </span>
        </label>
        <label className={styles.field}>
          Label
          <input name="label" className={styles.input} placeholder="cleanup" />
        </label>
      </div>
      <label className={styles.field} style={{ marginBottom: 8 }}>
        Command
        <input
          name="command"
          className={styles.input}
          required
          placeholder="/usr/local/bin/cleanup.sh"
        />
        <span className={styles.muted} style={{ fontSize: 12, marginTop: 4 }}>
          Single-line shell command. Runs under{" "}
          <span className={styles.code}>SHELL=/bin/sh</span> with{" "}
          <span className={styles.code}>PATH</span> set to the standard
          system + admin dirs. For multi-step commands, chain with{" "}
          <span className={styles.code}>&amp;&amp;</span> or write a script
          and call it.
        </span>
      </label>
      {state?.ok === false && <div className={styles.error}>{state.error}</div>}
      <button type="submit" className={styles.btn} disabled={pending}>
        {pending ? "..." : "Add cron job"}
      </button>
    </form>
  );
}
