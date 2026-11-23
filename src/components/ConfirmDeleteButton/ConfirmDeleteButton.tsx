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
 * `action` is a (bound) server action — the page passes e.g.
 * `deletePgDatabaseAction.bind(null, name)`. It reports a refusal by
 * returning `{ ok: false, error }`: the message is shown inline in the dialog,
 * which stays open so the operator can read it and retry or cancel. (A thrown
 * error is shown the same way, but Next replaces its message in a production
 * build, so an action returns the text it wants read.) An action that returns
 * nothing is taken as done.
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
  action: () => Promise<void | { ok: true } | { ok: false; error: string }>;
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
        const res = await action();
        if (res && !res.ok) {
          setError(res.error);
          return;
        }
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
