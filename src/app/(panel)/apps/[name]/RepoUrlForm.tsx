"use client";

import { useActionState, useEffect, useState } from "react";
import styles from "@/components/page.module.css";

type Result = { ok: true } | { ok: false; error: string };

/**
 * Inline editor for `apps.repo_url`. Greyed-out "Save" button until the
 * operator edits something, just like the panel's other in-place editors.
 * After save the button greys back out and a "Saved" pill shows briefly.
 */
export default function RepoUrlForm({
  initialUrl,
  action,
}: {
  initialUrl: string;
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
}) {
  const [state, formAction, pending] = useActionState<Result | null, FormData>(
    action,
    null,
  );
  const [value, setValue] = useState(initialUrl);

  // Re-sync when the prop changes (revalidatePath after save re-renders
  // the page server-side; we want the new server value to become the
  // new "clean" baseline).
  useEffect(() => {
    setValue(initialUrl);
  }, [initialUrl]);

  const dirty = value.trim() !== initialUrl.trim();
  const canSave = !pending && dirty;

  return (
    <form action={formAction}>
      <label className={styles.field} style={{ marginBottom: 8 }}>
        Repository URL
        <input
          name="repo_url"
          className={styles.input}
          value={value}
          onChange={(e) => setValue(e.target.value)}
          placeholder="git@github_alias:owner/repo.git  or  https://github.com/owner/repo.git"
          autoComplete="off"
        />
        <span className={styles.muted} style={{ fontSize: 12, marginTop: 4 }}>
          Where Rebuild and Pull get the code. The next pull points the
          checkout&apos;s <span className={styles.code}>origin</span> at this URL,
          so changing it switches repos. Empty leaves the checkout&apos;s origin as
          it is.
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
            Saved
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
          title={!dirty ? "No changes to save" : "Save repo URL"}
        >
          {pending ? "Saving…" : "Save"}
        </button>
      </div>
    </form>
  );
}
