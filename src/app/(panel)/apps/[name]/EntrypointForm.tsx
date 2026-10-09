"use client";

import { useActionState, useEffect, useState } from "react";
import styles from "@/components/page.module.css";

type Result = { ok: true } | { ok: false; error: string };

/**
 * Inline editor for an app's systemd ExecStart (stored as `apps.entrypoint`).
 * Saving rewrites the unit's ExecStart line and restarts the app, so the
 * button is greyed out until the value actually changes - same UX as the
 * repo-URL editor next to it.
 */
export default function EntrypointForm({
  initialValue,
  action,
}: {
  initialValue: string;
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
}) {
  const [state, formAction, pending] = useActionState<Result | null, FormData>(
    action,
    null,
  );
  const [value, setValue] = useState(initialValue);

  // Re-sync when the server value changes (revalidatePath after save makes the
  // new value the clean baseline).
  useEffect(() => {
    setValue(initialValue);
  }, [initialValue]);

  const dirty = value.trim() !== initialValue.trim();
  const canSave = !pending && dirty && value.trim().length > 0;

  return (
    <form action={formAction}>
      <label className={styles.field} style={{ marginBottom: 8 }}>
        Exec start
        <input
          name="exec_start"
          className={styles.input}
          value={value}
          onChange={(e) => setValue(e.target.value)}
          placeholder="/usr/bin/node dist/main"
          autoComplete="off"
          spellCheck={false}
        />
        <span className={styles.muted} style={{ fontSize: 12, marginTop: 4 }}>
          The command the systemd unit runs (its{" "}
          <span className={styles.code}>ExecStart</span>), from inside the app&apos;s
          source dir. Saving rewrites the unit and{" "}
          <strong>restarts the app</strong>. For a built Node app use{" "}
          <span className={styles.code}>/usr/bin/node dist/main</span>;{" "}
          <span className={styles.code}>/usr/bin/npm start</span> runs the{" "}
          <span className={styles.code}>start</span> script in{" "}
          <span className={styles.code}>package.json</span>.
        </span>
      </label>
      <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
        {state?.ok === false && (
          <span className={styles.error} style={{ marginRight: "auto" }}>
            {state.error}
          </span>
        )}
        {state?.ok === true && !dirty && (
          <span
            style={{
              color: "var(--ok, #4ade80)",
              fontSize: 12,
              fontWeight: 600,
              marginRight: "auto",
            }}
          >
            Saved &amp; restarted
          </span>
        )}
        <button
          type="submit"
          disabled={!canSave}
          className={canSave ? styles.btn : undefined}
          style={
            canSave
              ? { fontSize: 13, padding: "6px 14px" }
              : {
                  fontSize: 13,
                  padding: "6px 14px",
                  background: "var(--bg-elev-2)",
                  border: "1px solid var(--border)",
                  color: "var(--text-dim)",
                  borderRadius: 6,
                  cursor: "not-allowed",
                  fontWeight: 600,
                }
          }
          title={!dirty ? "No changes to save" : "Rewrite ExecStart + restart"}
        >
          {pending ? "Saving…" : "Save & restart"}
        </button>
      </div>
    </form>
  );
}
