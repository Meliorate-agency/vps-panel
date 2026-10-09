"use client";

import { useState, useTransition } from "react";
import styles from "@/components/page.module.css";

type Result = { ok: true } | { ok: false; error: string };

/** Sends a test message through one channel and shows whether it went out. */
export default function TestButton({ action }: { action: () => Promise<Result> }) {
  const [pending, start] = useTransition();
  const [result, setResult] = useState<Result | null>(null);
  return (
    <span style={{ display: "inline-flex", gap: 8, alignItems: "center" }}>
      <button
        type="button"
        className={styles.btnGhost}
        disabled={pending}
        onClick={() =>
          start(async () => {
            setResult(await action());
          })
        }
      >
        {pending ? "Sending..." : "Send test"}
      </button>
      {result?.ok === true && <span className={styles.muted}>Sent</span>}
      {result?.ok === false && (
        <span className={styles.error} title={result.error}>
          {result.error}
        </span>
      )}
    </span>
  );
}
