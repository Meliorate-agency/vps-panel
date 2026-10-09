"use client";

import { useActionState, useEffect } from "react";
import styles from "@/components/page.module.css";
import type { ActionResult } from "@/server-actions/networks";

export default function CreateNetworkForm({
  action,
  disabled,
  onDone,
}: {
  action: (prev: ActionResult | null, fd: FormData) => Promise<ActionResult>;
  disabled: boolean;
  /** Called after the network was created (it shows up below). */
  onDone?: () => void;
}) {
  const [state, formAction, pending] = useActionState<ActionResult | null, FormData>(action, null);
  useEffect(() => {
    if (state?.ok) onDone?.();
  }, [state, onDone]);
  return (
    <form action={formAction}>
      <div className={styles.formRow}>
        <label className={styles.field}>
          Network name
          <input
            name="name"
            required
            maxLength={64}
            placeholder="e.g. Production"
            className={styles.input}
            autoComplete="off"
            autoFocus
            disabled={disabled}
          />
        </label>
      </div>
      {state?.ok === false && <div className={styles.error}>{state.error}</div>}
      <button type="submit" className={styles.btn} disabled={pending || disabled}>
        {pending ? "Checking this server's address..." : "Create network"}
      </button>
    </form>
  );
}
