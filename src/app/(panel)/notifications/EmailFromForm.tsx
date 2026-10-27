"use client";

import { useActionState } from "react";
import styles from "@/components/page.module.css";

type Result = { ok: true } | { ok: false; error: string };

/**
 * Which mailbox notification email is sent as. A mailbox on a mail domain
 * here gets the domain's SPF and DKIM, and bounces land in it; the server
 * default (root@<hostname>) has neither.
 */
export default function EmailFromForm({
  action,
  current,
  mailboxes,
  server,
}: {
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
  current: string;
  mailboxes: string[];
  server: string;
}) {
  const [state, formAction, pending] = useActionState<Result | null, FormData>(action, null);
  // A saved mailbox that has since been deleted still shows, so saving
  // again is a choice rather than a silent switch.
  const options = current && !mailboxes.includes(current) ? [current, ...mailboxes] : mailboxes;
  return (
    <form action={formAction}>
      <div style={{ display: "flex", flexWrap: "wrap", gap: 12, alignItems: "flex-end", marginBottom: 8 }}>
        <label className={styles.field} style={{ flex: "1 1 260px", maxWidth: 480 }}>
          Send email from
          {/* Keyed on the saved value: React's form reset after Save would
              otherwise snap it back to the value it mounted with. */}
          <select key={current} name="email_from" className={styles.select} defaultValue={current}>
            <option value="">Server default (root@{server || "this server"})</option>
            {options.map((m) => (
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
      <div className={styles.muted} style={{ fontSize: 12 }}>
        {mailboxes.length === 0
          ? "No mailboxes yet. Create one on Mailboxes (e.g. alerts@ your domain) and pick it here: mail from the server default has no SPF or DKIM, so Gmail and others may refuse it."
          : "Pick a mailbox: its domain's SPF and DKIM then cover the message, and if a receiving server refuses it, the bounce lands in that mailbox."}
      </div>
      {state?.ok === false && <div className={styles.error}>{state.error}</div>}
      {state?.ok === true && <div className={styles.muted} style={{ fontSize: 12 }}>Saved.</div>}
    </form>
  );
}
