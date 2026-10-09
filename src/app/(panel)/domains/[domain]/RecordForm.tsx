"use client";

import { useActionState } from "react";
import styles from "@/components/page.module.css";

type Result = { ok: true } | { ok: false; error: string };

export default function RecordForm({
  action,
  zone,
}: {
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
  zone: string;
}) {
  const [state, formAction, pending] = useActionState<Result | null, FormData>(action, null);
  return (
    <form action={formAction}>
      <div className={styles.formRow}>
        <label className={styles.field}>
          Name
          <input name="name" className={styles.input} placeholder={`@ or sub.${zone}`} />
        </label>
        <label className={styles.field}>
          Type
          <select name="type" className={styles.select} defaultValue="A">
            {["A", "AAAA", "CNAME", "MX", "TXT", "NS", "SRV", "CAA", "TLSA"].map((t) => (
              <option key={t}>{t}</option>
            ))}
          </select>
        </label>
        <label className={styles.field}>
          TTL
          <input name="ttl" type="number" defaultValue={3600} className={styles.input} />
        </label>
        <label className={styles.field}>
          Priority (MX/SRV)
          <input name="prio" type="number" defaultValue={0} className={styles.input} />
        </label>
      </div>
      <label className={styles.field} style={{ marginBottom: 8 }}>
        Content
        <input name="content" className={styles.input} required placeholder="203.0.113.42" />
      </label>
      {state?.ok === false && <div className={styles.error}>{state.error}</div>}
      <button type="submit" className={styles.btn} disabled={pending}>
        {pending ? "..." : "Add record"}
      </button>
    </form>
  );
}
