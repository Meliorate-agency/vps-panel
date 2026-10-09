"use client";

import { useState, useTransition } from "react";
import styles from "@/components/page.module.css";

type Result = { ok: true } | { ok: false; error: string };

export default function AcmeEmailForm({
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
    <div style={{ display: "flex", gap: 8, alignItems: "flex-start", flexWrap: "wrap" }}>
      <input
        type="email"
        value={text}
        onChange={(e) => setText(e.target.value)}
        placeholder="you@yourdomain.com"
        className={styles.input}
        style={{ flex: 1, minWidth: 260 }}
      />
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
      {msg?.ok === true && (
        <div className={styles.notice}>Saved. Applies to the next SSL issuance.</div>
      )}
    </div>
  );
}
