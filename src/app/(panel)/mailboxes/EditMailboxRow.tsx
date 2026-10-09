"use client";

import { useActionState, useState } from "react";
import styles from "@/components/page.module.css";
import localStyles from "./mailboxes.module.css";
import { generatePassword } from "@/lib/passwordGen";

type Result =
  | { ok: true; data?: { password?: string } }
  | { ok: false; error: string };

export default function EditMailboxRow({
  email,
  currentQuotaMb,
  currentUsageMb,
  action,
}: {
  email: string;
  currentQuotaMb: number;
  currentUsageMb: number;
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
}) {
  const [state, formAction, pending] = useActionState<Result | null, FormData>(action, null);
  // Controlled so "Generate" can fill it. Shown as plain text on purpose:
  // the operator is setting a password to hand to someone, not typing their
  // own, and needs to read it.
  const [password, setPassword] = useState("");
  const minQuota = currentUsageMb + 1;

  return (
    <details className={localStyles.editDetails} name="mailbox-edit">
      <summary className={localStyles.editSummary}>Edit</summary>
      <div className={localStyles.editPanel}>
        <div className={localStyles.editTitle}>
          Edit <span className={styles.code}>{email}</span>
        </div>
        <form action={formAction}>
          <div className={styles.formRow}>
            <label className={styles.field}>
              New password (optional)
              <div className={localStyles.pwRow}>
                <input
                  name="password"
                  className={styles.input}
                  type="text"
                  placeholder="leave blank to keep current"
                  autoComplete="new-password"
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                  spellCheck={false}
                />
                <button
                  type="button"
                  className={styles.btnGhost}
                  onClick={() => setPassword(generatePassword())}
                  title="Fill in a random 20-character password"
                >
                  Generate
                </button>
              </div>
              <span className={localStyles.hint}>
                Generate fills in a strong random password. It is applied when you save.
              </span>
            </label>
            <label className={styles.field}>
              Quota (MB)
              <input type="hidden" name="current_quota_mb" value={currentQuotaMb} />
              <input
                name="quota_mb"
                className={styles.input}
                type="number"
                defaultValue={currentQuotaMb}
                step={1}
              />
              <span className={localStyles.hint}>
                Current usage: {currentUsageMb} MB. A new quota must be at least {minQuota} MB; leave it as is to change only the password.
              </span>
            </label>
          </div>
          {state?.ok === false && <div className={styles.error}>{state.error}</div>}
          {state?.ok === true && state.data?.password && (
            <div className={styles.notice}>
              New password (copy it now, it will not be shown again):{" "}
              <code className={styles.code}>{state.data.password}</code>
            </div>
          )}
          {state?.ok === true && !state.data?.password && (
            <div className={styles.notice}>Saved.</div>
          )}
          <button type="submit" className={styles.btn} disabled={pending}>
            {pending ? "..." : "Save changes"}
          </button>
        </form>
      </div>
    </details>
  );
}
