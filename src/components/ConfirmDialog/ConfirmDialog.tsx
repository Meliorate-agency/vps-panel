"use client";

import { useEffect, useRef, useSyncExternalStore } from "react";
import { createPortal } from "react-dom";
import pageStyles from "@/components/page.module.css";
import styles from "./ConfirmDialog.module.css";

const noSubscribe = () => () => {};

type Props = {
  open: boolean;
  title: string;
  children: React.ReactNode;
  confirmLabel?: string;
  cancelLabel?: string;
  tone?: "default" | "danger";
  pending?: boolean;
  /**
   * Suppress the built-in Cancel/Confirm action row. Use when the dialog is
   * displaying a streaming task or some other in-progress UI that needs to
   * own its own controls (e.g. a "Close" button only enabled after the task
   * settles).
   */
  hideActions?: boolean;
  /**
   * Block ESC and backdrop-click dismissal. Set true while a destructive
   * background task is mid-flight so the user can't accidentally close the
   * dialog before the task finishes.
   */
  lockDismiss?: boolean;
  /** 760px instead of 560px, for dialogs that hold a whole screen's worth
   *  (the Tailscale dialog). */
  wide?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
};

export default function ConfirmDialog({
  open,
  title,
  children,
  confirmLabel = "Confirm",
  cancelLabel = "Cancel",
  tone = "default",
  pending = false,
  hideActions = false,
  lockDismiss = false,
  wide = false,
  onConfirm,
  onCancel,
}: Props) {
  const confirmBtn = useRef<HTMLButtonElement>(null);
  // Latest onCancel via a ref so the key/scroll-lock effect below doesn't list
  // it as a dependency. Callers pass an inline arrow (new reference every
  // render), and re-running that effect on every render also re-ran the focus
  // call — which stole focus from inputs in the dialog on each keystroke.
  const onCancelRef = useRef(onCancel);
  onCancelRef.current = onCancel;

  // Focus the confirm button when the dialog OPENS — not on every render.
  useEffect(() => {
    if (open && !hideActions) confirmBtn.current?.focus();
  }, [open, hideActions]);

  // ESC-to-dismiss + body scroll lock while open. Depends only on values that
  // genuinely change the behavior; onCancel is read through its ref.
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !pending && !lockDismiss) onCancelRef.current();
    };
    window.addEventListener("keydown", onKey);
    const prevOverflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => {
      window.removeEventListener("keydown", onKey);
      document.body.style.overflow = prevOverflow;
    };
  }, [open, pending, lockDismiss]);

  // Rendered into <body>. Cards, table wrappers and the quick-install cards
  // blur their background (backdrop-filter), which makes them the containing
  // block of a fixed-position descendant: a dialog opened from a table row
  // or a card was sized and clipped to that card instead of the viewport.
  // False on the server and while hydrating, so the markup still matches.
  const canPortal = useSyncExternalStore(
    noSubscribe,
    () => true,
    () => false,
  );

  if (!open) return null;

  const dialog = (
    <div
      className={styles.backdrop}
      onClick={() => {
        if (!pending && !lockDismiss) onCancel();
      }}
      role="dialog"
      aria-modal="true"
      aria-labelledby="confirm-dialog-title"
    >
      <div
        className={`${styles.dialog} ${wide ? styles.wide : ""}`}
        onClick={(e) => e.stopPropagation()}
      >
        {/* Safety-net ✕ in the top-right. On mobile (especially iOS
         *  Safari) the dialog can fill the visible viewport when the
         *  address bar is expanded, leaving no backdrop area to tap for
         *  dismissal. This button is always reachable inside the
         *  dialog itself. lockDismiss honours the same gate the
         *  backdrop click does. */}
        {!lockDismiss && (
          <button
            type="button"
            className={styles.closeBtn}
            onClick={onCancel}
            disabled={pending}
            aria-label="Close dialog"
            title="Close"
          >
            <svg width="14" height="14" viewBox="0 0 16 16" fill="none" aria-hidden="true">
              <path d="M3 3L13 13M13 3L3 13" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" />
            </svg>
          </button>
        )}
        <h3 id="confirm-dialog-title" className={styles.title}>
          {title}
        </h3>
        <div className={styles.body}>{children}</div>
        {!hideActions && (
          <div className={styles.actions}>
            <button
              type="button"
              className={pageStyles.btnGhost}
              onClick={onCancel}
              disabled={pending}
            >
              {cancelLabel}
            </button>
            <button
              ref={confirmBtn}
              type="button"
              className={tone === "danger" ? pageStyles.btnDanger : pageStyles.btn}
              onClick={onConfirm}
              disabled={pending}
              aria-busy={pending}
            >
              {pending ? "..." : confirmLabel}
            </button>
          </div>
        )}
      </div>
    </div>
  );
  return canPortal ? createPortal(dialog, document.body) : dialog;
}
