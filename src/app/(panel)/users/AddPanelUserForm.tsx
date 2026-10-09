"use client";

import { useActionState, useEffect, useRef, useState } from "react";
import styles from "@/components/page.module.css";

type Result = { ok: true } | { ok: false; error: string };

/**
 * Inline form for creating a new panel-login user. Mirrors RootPasswordForm
 * styling — email + password + confirm + role select + reveal toggle.
 * On success, the form resets so the operator can immediately enroll
 * another account without shoulder-surf risk from a lingering password
 * value in the field.
 */
export default function AddPanelUserForm({
  action,
}: {
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
}) {
  const [state, formAction, pending] = useActionState<Result | null, FormData>(
    action,
    null,
  );
  const [revealed, setRevealed] = useState(false);
  const formRef = useRef<HTMLFormElement>(null);

  useEffect(() => {
    if (state?.ok === true) {
      formRef.current?.reset();
      setRevealed(false);
    }
  }, [state]);

  return (
    <form ref={formRef} action={formAction}>
      <div className={styles.formRow}>
        <label className={styles.field}>
          Email
          <input
            name="email"
            type="email"
            className={styles.input}
            placeholder="alice@example.com"
            autoComplete="off"
            required
          />
        </label>
        <label className={styles.field}>
          Role
          <select name="role" className={styles.select} defaultValue="admin">
            <option value="admin">admin</option>
            <option value="user">user</option>
          </select>
        </label>
      </div>
      <div className={styles.formRow}>
        <label className={styles.field}>
          Password
          <input
            name="password"
            type={revealed ? "text" : "password"}
            className={styles.input}
            autoComplete="new-password"
            minLength={12}
            required
          />
        </label>
        <label className={styles.field}>
          Confirm
          <input
            name="confirm"
            type={revealed ? "text" : "password"}
            className={styles.input}
            autoComplete="new-password"
            minLength={12}
            required
          />
        </label>
      </div>
      <label className={styles.checkbox} style={{ marginBottom: 12 }}>
        <input
          type="checkbox"
          checked={revealed}
          onChange={(e) => setRevealed(e.target.checked)}
        />
        Show passwords
      </label>
      <div className={styles.muted} style={{ fontSize: 12, marginBottom: 12 }}>
        Minimum 12 characters. The password is hashed (argon2id) before being
        written to <span className={styles.code}>panel.db</span>. The new user
        can sign in immediately at <span className={styles.code}>/login</span>.
      </div>

      {state?.ok === false && (
        <div className={styles.error} style={{ marginBottom: 12 }}>
          {state.error}
        </div>
      )}
      {state?.ok === true && (
        <div className={styles.notice} style={{ marginBottom: 12 }}>
          User created.
        </div>
      )}

      <button type="submit" className={styles.btn} disabled={pending}>
        {pending ? "Creating..." : "Create user"}
      </button>
    </form>
  );
}
