"use client";

import { useState, useTransition } from "react";
import styles from "@/components/page.module.css";

type Result = { ok: true } | { ok: false; error: string };

export default function UploadLimitForm({
  value,
  max,
  action,
}: {
  value: number;
  max: number;
  action: (next: string) => Promise<Result>;
}) {
  const [text, setText] = useState(String(value));
  const [pending, start] = useTransition();
  const [msg, setMsg] = useState<Result | null>(null);
  return (
    <div style={{ display: "flex", gap: 8, alignItems: "flex-end", flexWrap: "wrap" }}>
      <label className={styles.field} style={{ minWidth: 220 }}>
        Largest upload (MB)
        <input
          type="number"
          min={1}
          max={max}
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
      {msg?.ok === true && <div className={styles.notice}>Saved. Applies to the next upload.</div>}
    </div>
  );
}
