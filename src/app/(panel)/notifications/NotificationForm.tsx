"use client";

import { useActionState } from "react";
import styles from "@/components/page.module.css";
import { NOTIFICATION_EVENTS } from "@/lib/notificationEvents";

type Result = { ok: true } | { ok: false; error: string };

export default function NotificationForm({
  action,
}: {
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
}) {
  const [state, formAction, pending] = useActionState<Result | null, FormData>(action, null);
  return (
    <form action={formAction}>
      <div className={styles.formRow}>
        <label className={styles.field}>
          Channel
          <select name="channel" className={styles.select} defaultValue="email">
            <option value="email">email</option>
            <option value="webhook">webhook</option>
          </select>
        </label>
        <label className={styles.field}>
          Target
          <input
            name="target"
            className={styles.input}
            required
            placeholder="ops@example.com or https://hooks.slack.com/..."
          />
        </label>
      </div>
      <fieldset style={{ border: "none", padding: 0, margin: "0 0 12px" }}>
        <legend className={styles.muted} style={{ fontSize: 12, marginBottom: 6 }}>
          Events (none selected = all)
        </legend>
        <div style={{ display: "flex", flexWrap: "wrap", gap: "6px 16px" }}>
          {NOTIFICATION_EVENTS.map((e) => (
            <label key={e.id} style={{ display: "flex", gap: 6, alignItems: "center", fontSize: 13 }}>
              <input type="checkbox" name="events" value={e.id} />
              {e.label}
            </label>
          ))}
        </div>
      </fieldset>
      {state?.ok === false && <div className={styles.error}>{state.error}</div>}
      <button type="submit" className={styles.btn} disabled={pending}>
        {pending ? "..." : "Add"}
      </button>
    </form>
  );
}
