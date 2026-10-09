"use client";

import { useActionState, useEffect } from "react";
import { useRouter } from "next/navigation";
import styles from "@/components/page.module.css";

type Result = { ok: true } | { ok: false; error: string };

export default function AdminEmailForm({
  currentEmail,
  action,
}: {
  currentEmail: string;
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
}) {
  const [state, formAction, pending] = useActionState<Result | null, FormData>(
    action,
    null,
  );
  const router = useRouter();

  useEffect(() => {
    if (state?.ok === true) {
      // The action revoked our session — the cookies are gone, so any client
      // navigation will land on /login anyway. Push there explicitly so the
      // user gets the "log in with your new email" affordance immediately
      // instead of trying to click around the stale UI.
      router.push("/login");
    }
  }, [state, router]);

  return (
    <form action={formAction}>
      <div className={styles.formRow}>
        <label className={styles.field}>
          Admin email
          <input
            name="email"
            type="email"
            required
            defaultValue={currentEmail}
            className={styles.input}
            autoComplete="email"
          />
        </label>
      </div>
      {state?.ok === false && <div className={styles.error}>{state.error}</div>}
      {state?.ok === true && (
        <div className={styles.notice}>
          Email updated. Redirecting to login — sign in with your new email.
        </div>
      )}
      <button type="submit" className={styles.btn} disabled={pending}>
        {pending ? "..." : "Change email"}
      </button>
    </form>
  );
}
