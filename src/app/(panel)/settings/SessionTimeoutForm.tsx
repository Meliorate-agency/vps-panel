"use client";

import { useState, useTransition } from "react";
import styles from "@/components/page.module.css";

type Result = { ok: true } | { ok: false; error: string };

export default function SessionTimeoutForm({
  value,
  action,
}: {
  value: string;
  action: (next: string) => Promise<Result>;
}) {
  const [text, setText] = useState(value);
  const [pending, start] = useTransition();
  const [msg, setMsg] = useState<Result | null>(null);
  return (
    <div style={{ display: "flex", gap: 8, alignItems: "flex-end", flexWrap: "wrap" }}>
      <label className={styles.field} style={{ minWidth: 220 }}>
        Auto-logout after (minutes)
        <input
          type="number"
          min={1}
          max={43200}
          value={text}
          onChange={(e) => setText(e.target.value)}
          className={styles.input}
        />
      </label>
      <button
        className={styles.btn}
        onClick={() =>
          start(async () => {
            const r = await action(text);
            setMsg(r);
          })
        }
        disabled={pending}
      >
        {pending ? "..." : "Save"}
      </button>
      {msg?.ok === false && <div className={styles.error}>{msg.error}</div>}
      {msg?.ok === true && <div className={styles.notice}>Saved. Applies on next sign-in.</div>}
    </div>
  );
}
