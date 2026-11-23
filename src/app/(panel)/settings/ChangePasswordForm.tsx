"use client";

import { useEffect, useRef, useState } from "react";
import PasswordInput from "@/components/PasswordInput/PasswordInput";
import styles from "@/components/page.module.css";
import { useFormActionState } from "@/lib/useFormActionState";

type Result = { ok: true } | { ok: false; error: string };

export default function ChangePasswordForm({
  action,
}: {
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
}) {
  const formRef = useRef<HTMLFormElement>(null);
  const [state, formAction, pending, keepRef] = useFormActionState<Result | null>(
    action,
    null,
    formRef,
  );
  const [showCurrent, setShowCurrent] = useState(false);
  const [showNext, setShowNext] = useState(false);

  useEffect(() => {
    if (state?.ok === true) {
      formRef.current?.reset();
      // Reset visibility too so a successful submit doesn't leave the next
      // password sitting plaintext on screen for shoulder-surfers.
      setShowCurrent(false);
      setShowNext(false);
    }
  }, [state]);

  return (
    <form ref={keepRef} action={formAction}>
      <div className={styles.formRow}>
        <label className={styles.field}>
          Current password
          <PasswordInput
            name="current"
            visible={showCurrent}
            onToggle={() => setShowCurrent((v) => !v)}
          />
        </label>
        <label className={styles.field}>
          New password (12+ chars)
          <PasswordInput
            name="next"
            minLength={12}
            visible={showNext}
            onToggle={() => setShowNext((v) => !v)}
          />
        </label>
      </div>
      {state?.ok === false && <div className={styles.error}>{state.error}</div>}
      {state?.ok === true && <div className={styles.notice}>Password updated.</div>}
      <button type="submit" className={styles.btn} disabled={pending}>
        {pending ? "..." : "Change password"}
      </button>
    </form>
  );
}
