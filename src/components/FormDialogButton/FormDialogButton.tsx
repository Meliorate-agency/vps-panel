"use client";

import { useState } from "react";
import { createPortal } from "react-dom";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";
import styles from "@/components/page.module.css";

/**
 * A button that opens a form in the panel's dialog instead of keeping that
 * form open on the page (Add panel user, Add Linux user, root's password).
 * The form owns its own submit and messages, so the dialog shows no action
 * row and stays open after a save until it is closed.
 *
 * Portaled to <body>: cards and table wrappers blur their background
 * (backdrop-filter), which would otherwise trap the fixed-position dialog
 * inside the card it was opened from.
 */
export default function FormDialogButton({
  label,
  title,
  children,
  className,
  buttonStyle,
}: {
  label: string;
  title: string;
  children: React.ReactNode;
  className?: string;
  buttonStyle?: React.CSSProperties;
}) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button
        type="button"
        className={className ?? styles.btn}
        style={buttonStyle}
        onClick={() => setOpen(true)}
      >
        {label}
      </button>
      {open &&
        typeof document !== "undefined" &&
        createPortal(
          <ConfirmDialog
            open
            title={title}
            hideActions
            onConfirm={() => setOpen(false)}
            onCancel={() => setOpen(false)}
          >
            {children}
          </ConfirmDialog>,
          document.body,
        )}
    </>
  );
}
