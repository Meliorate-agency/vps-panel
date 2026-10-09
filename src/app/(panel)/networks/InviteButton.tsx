"use client";

import { useState, useTransition } from "react";
import { copyText } from "@/lib/clipboard";
import styles from "@/components/page.module.css";
import local from "./networks.module.css";

type Result = { ok: true; code: string; expires: number } | { ok: false; error: string };

/** Makes a one-time join code and shows it once. Only a hash of it is kept. */
export default function InviteButton({ action }: { action: () => Promise<Result> }) {
  const [result, setResult] = useState<Result | null>(null);
  const [copied, setCopied] = useState(false);
  const [pending, start] = useTransition();

  if (result?.ok) {
    return (
      <div className={local.invite}>
        <div className={styles.muted} style={{ fontSize: 12 }}>
          Paste this into My Networks on the server that should join. It works once, within{" "}
          {Math.round((result.expires - Date.now()) / 60_000)} min, and is not shown again.
        </div>
        <textarea
          readOnly
          className={`${styles.textarea} ${local.code}`}
          value={result.code}
          onFocus={(e) => e.currentTarget.select()}
        />
        <div className={styles.actions}>
          <button
            type="button"
            className={styles.btnGhost}
            onClick={() => {
              void copyText(result.code).then(setCopied);
            }}
          >
            {copied ? "Copied" : "Copy code"}
          </button>
          <button type="button" className={styles.btnGhost} onClick={() => setResult(null)}>
            Done
          </button>
        </div>
      </div>
    );
  }
  return (
    <span className={local.inline}>
      <button
        type="button"
        className={styles.btn}
        disabled={pending}
        onClick={() =>
          start(async () => {
            setCopied(false);
            setResult(await action());
          })
        }
      >
        {pending ? "..." : "Invite a server"}
      </button>
      {result?.ok === false && <span className={styles.error}>{result.error}</span>}
    </span>
  );
}
