"use client";

import { useActionState, useEffect, useRef, useState } from "react";
import styles from "@/components/page.module.css";

type Result = { ok: true } | { ok: false; error: string };

/**
 * "+ New folder" button that expands into an inline name field when
 * clicked. Keeps the /apps top bar uncluttered when no operator is in
 * the middle of creating a folder.
 */
export default function NewFolderButton({
  action,
}: {
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
}) {
  const [open, setOpen] = useState(false);
  const [state, formAction, pending] = useActionState<Result | null, FormData>(
    action,
    null,
  );
  const formRef = useRef<HTMLFormElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  // On success, collapse and clear so a second folder add starts fresh.
  useEffect(() => {
    if (state?.ok === true) {
      formRef.current?.reset();
      setOpen(false);
    }
  }, [state]);

  useEffect(() => {
    if (open) inputRef.current?.focus();
  }, [open]);

  if (!open) {
    return (
      <button
        type="button"
        className={styles.btnGhost}
        onClick={() => setOpen(true)}
        title="Create a new folder for organising apps"
      >
        + New folder
      </button>
    );
  }

  return (
    <form
      ref={formRef}
      action={formAction}
      style={{ display: "flex", gap: 6, alignItems: "center" }}
    >
      <input
        ref={inputRef}
        name="name"
        className={styles.input}
        placeholder="folder name"
        maxLength={60}
        required
        style={{ height: 30, padding: "2px 10px", fontSize: 13, width: 180 }}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.preventDefault();
            setOpen(false);
          }
        }}
      />
      <button
        type="submit"
        className={styles.btn}
        disabled={pending}
        style={{ height: 30, padding: "0 12px", fontSize: 12 }}
      >
        {pending ? "..." : "Create"}
      </button>
      <button
        type="button"
        className={styles.btnGhost}
        onClick={() => setOpen(false)}
        disabled={pending}
        style={{ height: 30, padding: "0 10px", fontSize: 12 }}
      >
        Cancel
      </button>
      {state?.ok === false && (
        <span className={styles.error} style={{ fontSize: 12 }}>
          {state.error}
        </span>
      )}
    </form>
  );
}
