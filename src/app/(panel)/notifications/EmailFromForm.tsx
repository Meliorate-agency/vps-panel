"use client";

import Link from "next/link";
import { useActionState } from "react";
import styles from "@/components/page.module.css";

type Result = { ok: true } | { ok: false; error: string };

/**
 * Which mailbox notification email is sent as. Only an existing mailbox can
 * be picked: its domain's SPF and DKIM cover the message and bounces land in
 * it. Until one is saved the agent falls back to the server's own name,
 * which has neither, so the form says so.
 */
export default function EmailFromForm({
  action,
  current,
  mailboxes,
}: {
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
  current: string;
  /** null when the agent could not list them. */
  mailboxes: string[] | null;
}) {
  const [state, formAction, pending] = useActionState<Result | null, FormData>(action, null);
  if (mailboxes === null) {
    return <div className={styles.error}>Could not read the mailbox list from the agent.</div>;
  }
  if (mailboxes.length === 0) {
    return (
      <div className={styles.muted} style={{ fontSize: 13 }}>
        No mailboxes yet. Create one on <Link href="/mailboxes">Mailboxes</Link> (for example
        alerts@ your domain), then pick it here.
      </div>
    );
  }
  const chosen = mailboxes.includes(current) ? current : "";
  return (
    <form action={formAction}>
      <div style={{ display: "flex", flexWrap: "wrap", gap: 12, alignItems: "flex-end", marginBottom: 8 }}>
        <label className={styles.field} style={{ flex: "1 1 260px", maxWidth: 480 }}>
          Send email from
          {/* Keyed on the saved value: React's form reset after Save would
              otherwise snap it back to the value it mounted with. */}
          <select key={chosen} name="email_from" className={styles.select} defaultValue={chosen} required>
            {!chosen && (
              <option value="" disabled>
                Choose a mailbox
              </option>
            )}
            {mailboxes.map((m) => (
              <option key={m} value={m}>
                {m}
              </option>
            ))}
          </select>
        </label>
        <button type="submit" className={styles.btn} disabled={pending}>
          {pending ? "..." : "Save"}
        </button>
      </div>
      <div className={chosen ? styles.muted : styles.error} style={{ fontSize: 12 }}>
        {chosen
          ? "Its domain's SPF and DKIM cover the message, and if a receiving server refuses it, the bounce lands in this mailbox."
          : current
            ? `${current} no longer exists. Pick another mailbox: until then notification email may be refused.`
            : "No sender picked yet: until you pick a mailbox, notification email goes out under the server's own name and Gmail and others may refuse it."}
      </div>
      {state?.ok === false && <div className={styles.error}>{state.error}</div>}
      {state?.ok === true && <div className={styles.muted} style={{ fontSize: 12 }}>Saved.</div>}
    </form>
  );
}
