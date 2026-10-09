"use client";

import { useState, useTransition } from "react";
import pageStyles from "@/components/page.module.css";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";

/**
 * Reusable styled "delete with confirmation" button. Renders a danger
 * button that opens the centered ConfirmDialog instead of firing a bare
 * <form> submit, so destructive actions (drop database / drop user) always
 * pass through an explicit, dismissable confirmation.
 *
 * `action` is a (bound) server action returning Promise<void> — the page
 * passes e.g. `deletePgDatabaseAction.bind(null, name)`. Errors the action
 * throws are caught and shown inline in the dialog rather than bubbling to
 * an error boundary, and the dialog stays open so the operator can read the
 * message and retry or cancel.
 *
 * Mirrors the DomainDeleteButton pattern but is engine/resource agnostic:
 * the caller supplies the title, body copy, and labels.
 */
export default function ConfirmDeleteButton({
  action,
  title,
  body,
  confirmLabel = "Delete",
  buttonLabel = "Drop",
  buttonClassName,
}: {
  action: () => Promise<void>;
  title: string;
  body: React.ReactNode;
  confirmLabel?: string;
  buttonLabel?: string;
  buttonClassName?: string;
}) {
  const [open, setOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [pending, start] = useTransition();

  const onConfirm = () => {
    setError(null);
    start(async () => {
      try {
        await action();
        setOpen(false);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      }
    });
  };

  return (
    <>
      <button
        type="button"
        className={buttonClassName ?? pageStyles.btnDanger}
        onClick={() => {
          setError(null);
          setOpen(true);
        }}
        disabled={pending}
      >
        {pending ? "…" : buttonLabel}
      </button>

      <ConfirmDialog
        open={open}
        title={title}
        confirmLabel={confirmLabel}
        cancelLabel="Cancel"
        tone="danger"
        pending={pending}
        onConfirm={onConfirm}
        onCancel={() => {
          if (pending) return;
          setOpen(false);
          setError(null);
        }}
      >
        {body}
        {error && (
          <div className={pageStyles.error} style={{ marginTop: 12 }}>
            {error}
          </div>
        )}
      </ConfirmDialog>
    </>
  );
}
