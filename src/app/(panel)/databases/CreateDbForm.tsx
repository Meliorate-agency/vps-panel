"use client";

import { useActionState } from "react";
import styles from "@/components/page.module.css";

type Result = { ok: true } | { ok: false; error: string };

export default function CreateDbForm({
  action,
}: {
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
}) {
  const [state, formAction, pending] = useActionState<Result | null, FormData>(action, null);
  return (
    <form action={formAction} style={{ display: "flex", gap: 8, alignItems: "flex-end" }}>
      <label className={styles.field} style={{ flex: 1 }}>
        Name
        <input name="name" className={styles.input} required placeholder="myapp_prod" />
      </label>
      <button type="submit" className={styles.btn} disabled={pending}>
        {pending ? "..." : "Create database"}
      </button>
      {state?.ok === false && <div className={styles.error}>{state.error}</div>}
    </form>
  );
}
