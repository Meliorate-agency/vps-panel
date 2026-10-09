"use client";

import { useActionState, useEffect, useRef, useState } from "react";
import styles from "@/components/page.module.css";

type Result = { ok: true } | { ok: false; error: string };

export default function RootPasswordForm({
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

  // Clear the inputs after a successful save so a follow-up shoulder-surf
  // doesn't catch the password sitting in the field. Server action returned
  // ok=true => the new password is already live on the host.
  useEffect(() => {
    if (state?.ok === true) {
      formRef.current?.reset();
    }
  }, [state]);

  return (
    <form ref={formRef} action={formAction}>
      <div className={styles.formRow}>
        <label className={styles.field}>
          New password
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
        Minimum 12 characters. The agent runs{" "}
        <span className={styles.code}>chpasswd</span> as root and writes the
        new hash to <span className={styles.code}>/etc/shadow</span>. The
        password is never stored by the panel.
      </div>

      {state?.ok === false && (
        <div className={styles.error} style={{ marginBottom: 12 }}>
          {state.error}
        </div>
      )}
      {state?.ok === true && (
        <div className={styles.notice} style={{ marginBottom: 12 }}>
          Root password updated. The Hetzner / OVH web console will accept it
          immediately — no reboot needed.
        </div>
      )}

      <button type="submit" className={styles.btn} disabled={pending}>
        {pending ? "Saving..." : "Set root password"}
      </button>
    </form>
  );
}
