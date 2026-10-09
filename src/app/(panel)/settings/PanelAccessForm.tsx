"use client";

import { useActionState, useState } from "react";
import styles from "@/components/page.module.css";

type Result = { ok: true } | { ok: false; error: string };

export default function PanelAccessForm({
  value,
  clientIp,
  action,
}: {
  value: string;
  clientIp: string;
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
}) {
  const [state, formAction, pending] = useActionState<Result | null, FormData>(action, null);
  const [text, setText] = useState(value);
  const hasMe = text.split(/[\s,]+/).includes(clientIp);

  return (
    <form action={formAction}>
      <textarea
        name="entries"
        value={text}
        onChange={(e) => setText(e.target.value)}
        rows={3}
        className={styles.textarea}
        placeholder={`${clientIp}, 203.0.113.0/24`}
        style={{ width: "100%" }}
      />
      <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap", marginTop: 8 }}>
        {!hasMe && (
          <button
            type="button"
            className={styles.btnGhost}
            onClick={() => setText((t) => (t.trim() ? `${t.trim()}, ${clientIp}` : clientIp))}
          >
            Add my address ({clientIp})
          </button>
        )}
        <button type="submit" className={styles.btn} disabled={pending}>
          {pending ? "Saving..." : "Save"}
        </button>
        {state?.ok === true && (
          <span className={styles.muted}>
            {text.trim() ? "Saved. Only these addresses reach the panel now." : "Saved. Every address can reach the panel."}
          </span>
        )}
      </div>
      {state?.ok === false && <div className={styles.error}>{state.error}</div>}
    </form>
  );
}
