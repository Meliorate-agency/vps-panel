"use client";

import { useRef, useState, useTransition } from "react";
import pageStyles from "@/components/page.module.css";
import styles from "./TypedConfirmButton.module.css";

type Props = {
  /** Text or node on the trigger button (the one users see in the table). */
  trigger: React.ReactNode;
  /** className for the trigger (e.g. styles.btnDanger). */
  triggerClassName?: string;
  /** Modal heading. */
  title: string;
  /** Identifier the user is deleting (shown in a code chip). */
  resourceName: string;
  /** Body copy explaining the consequences. */
  description: React.ReactNode;
  /** Word the user must type, case-sensitive. Defaults to DELETE. */
  phrase?: string;
  /** Confirm button label. */
  confirmLabel?: string;
  /** Pending label for the confirm button. */
  pendingLabel?: string;
  /** The (already-bound) action to run when typed phrase matches. A refusal
   *  comes back as `{ ok: false, error }`: the dialog stays open and shows it. */
  action: () => Promise<void | { ok: true } | { ok: false; error: string }> | void;
};

export default function TypedConfirmButton({
  trigger,
  triggerClassName,
  title,
  resourceName,
  description,
  phrase = "DELETE",
  confirmLabel = "Delete",
  pendingLabel = "Deleting...",
  action,
}: Props) {
  const dialogRef = useRef<HTMLDialogElement>(null);
  const [typed, setTyped] = useState("");
  const [pending, start] = useTransition();
  const [error, setError] = useState<string | null>(null);

  function open() {
    setTyped("");
    setError(null);
    dialogRef.current?.showModal();
  }

  function close() {
    dialogRef.current?.close();
  }

  function onSubmit(e: React.FormEvent) {
    e.preventDefault();
    if (typed !== phrase || pending) return;
    setError(null);
    start(async () => {
      // It used to close whatever happened, so a refused delete looked done
      // (or ended on the error screen). Closed only when the action went
      // through.
      try {
        const res = await action();
        if (res && !res.ok) {
          setError(res.error);
          return;
        }
        close();
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      }
    });
  }

  const ready = typed === phrase;

  return (
    <>
      <button
        type="button"
        className={triggerClassName}
        onClick={open}
        disabled={pending}
        aria-busy={pending}
      >
        {pending ? "..." : trigger}
      </button>
      <dialog
        ref={dialogRef}
        className={styles.dialog}
        onClose={() => setTyped("")}
      >
        <form onSubmit={onSubmit} className={styles.form}>
          <h2 className={styles.title}>{title}</h2>
          <div className={styles.resource}>{resourceName}</div>
          <div className={styles.description}>{description}</div>
          <label className={styles.label}>
            Type{" "}
            <code className={styles.phrase}>{phrase}</code> to confirm
            <input
              className={styles.input}
              value={typed}
              onChange={(e) => setTyped(e.target.value)}
              autoFocus
              autoComplete="off"
              autoCorrect="off"
              autoCapitalize="off"
              spellCheck={false}
              aria-label={`Type ${phrase} to confirm`}
            />
          </label>
          {error && (
            <div role="alert" className={pageStyles.error}>
              {error}
            </div>
          )}
          <div className={styles.actions}>
            <button
              type="button"
              className={pageStyles.btnGhost}
              onClick={close}
            >
              Cancel
            </button>
            <button
              type="submit"
              className={pageStyles.btnDanger}
              disabled={!ready || pending}
              aria-busy={pending}
            >
              {pending ? pendingLabel : confirmLabel}
            </button>
          </div>
        </form>
      </dialog>
    </>
  );
}
