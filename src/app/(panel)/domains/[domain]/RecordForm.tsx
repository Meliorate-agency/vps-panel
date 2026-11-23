"use client";

import { useActionState, useEffect, useRef, useTransition } from "react";
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
  const formRef = useRef<HTMLFormElement>(null);
  // Submitted by hand: through <form action> React reset the form after a
  // refused record too, wiping a long TXT/DKIM value along with the error.
  // Cleared only once the record is added.
  const [, startTransition] = useTransition();
  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    const fd = new FormData(e.currentTarget);
    startTransition(() => formAction(fd));
  }
  useEffect(() => {
    if (state?.ok) formRef.current?.reset();
  }, [state]);
  return (
    <form onSubmit={onSubmit} ref={formRef}>
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
          {/* Required: an emptied box reached the action as Number("") = 0
              and the record was served with TTL 0. */}
          <input
            name="ttl"
            type="number"
            defaultValue={3600}
            min={1}
            required
            className={styles.input}
          />
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
