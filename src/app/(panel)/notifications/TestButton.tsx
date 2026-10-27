"use client";

import { useState, useTransition } from "react";
import styles from "@/components/page.module.css";

type Result = { ok: true } | { ok: false; error: string };

/** Sends a test message through one channel and shows whether it went out.
 *  `sentLabel` replaces "Sent" (email only reaches the local Postfix here).
 *  `children` (the row's other buttons) sit beside it, and the result goes
 *  on its own line below so a long answer does not squeeze them. */
export default function TestButton({
  action,
  sentLabel = "Sent",
  children,
}: {
  action: () => Promise<Result>;
  sentLabel?: string;
  children?: React.ReactNode;
}) {
  const [pending, start] = useTransition();
  const [result, setResult] = useState<Result | null>(null);
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 6, alignItems: "flex-end" }}>
      <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
        <button
          type="button"
          className={styles.btnGhost}
          style={{ whiteSpace: "nowrap" }}
          disabled={pending}
          onClick={() =>
            start(async () => {
              setResult(await action());
            })
          }
        >
          {pending ? "Sending..." : "Send test"}
        </button>
        {children}
      </div>
      {result?.ok === true && (
        <span className={styles.muted} style={{ fontSize: 12, textAlign: "right" }}>
          {sentLabel}
        </span>
      )}
      {result?.ok === false && <span className={styles.error} style={{ fontSize: 12 }}>{result.error}</span>}
    </div>
  );
}
