"use client";

import { useEffect, useRef, useState } from "react";
import styles from "@/components/page.module.css";
import local from "./networks.module.css";
import type { ActionResult } from "@/server-actions/networks";

type SaveState = "idle" | "saving" | "saved" | { error: string };

const SAVE_AFTER_MS = 700;

/**
 * The name the other servers see. Saves by itself a moment after typing
 * stops, and when the field loses focus. Empty goes back to the host name.
 */
export default function NodeNameForm({
  current,
  fallback,
  save,
}: {
  current: string;
  fallback: string;
  save: (name: string) => Promise<ActionResult>;
}) {
  const [value, setValue] = useState(current);
  const [state, setState] = useState<SaveState>("idle");
  const saved = useRef(current.trim());
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(
    () => () => {
      if (timer.current) clearTimeout(timer.current);
    },
    [],
  );

  const flush = async (v: string) => {
    if (timer.current) {
      clearTimeout(timer.current);
      timer.current = null;
    }
    if (v.trim() === saved.current) return;
    setState("saving");
    const r = await save(v);
    if (r.ok) {
      saved.current = v.trim();
      setState("saved");
    } else {
      setState({ error: r.error });
    }
  };

  return (
    <label className={`${styles.field} ${local.nameField}`}>
      <span className={local.nameLabel}>
        Name the other servers see
        {state === "saving" && <span className={styles.muted}>Saving…</span>}
        {state === "saved" && <span className={local.savedNote}>Saved</span>}
      </span>
      <input
        value={value}
        onChange={(e) => {
          const v = e.target.value;
          setValue(v);
          setState("idle");
          if (timer.current) clearTimeout(timer.current);
          timer.current = setTimeout(() => void flush(v), SAVE_AFTER_MS);
        }}
        onBlur={() => void flush(value)}
        placeholder={fallback}
        maxLength={64}
        className={styles.input}
        autoComplete="off"
        aria-invalid={typeof state === "object"}
      />
      {typeof state === "object" && <span className={local.problem}>{state.error}</span>}
    </label>
  );
}
