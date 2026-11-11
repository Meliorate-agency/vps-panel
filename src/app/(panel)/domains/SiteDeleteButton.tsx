"use client";

import { useState, useTransition } from "react";
import { createPortal } from "react-dom";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";
import styles from "@/components/page.module.css";

/**
 * Removes one site's nginx vhost (Delete, Remove from agent, Remove HTTP
 * routing) after a confirm dialog that says what goes and what stays. The
 * dialog is portaled to <body>: inside the Domains table card, whose
 * backdrop-filter is the containing block of its fixed backdrop, it would be
 * placed and clipped by the card instead of the viewport.
 */
export default function SiteDeleteButton({
  domain,
  action,
  label = "Delete",
  pendingLabel = "Deleting...",
  title,
  className,
}: {
  domain: string;
  action: (domain: string) => Promise<void>;
  label?: string;
  pendingLabel?: string;
  /** Dialog heading; "Delete <domain>?" by default. */
  title?: string;
  className?: string;
}) {
  const [open, setOpen] = useState(false);
  const [pending, start] = useTransition();

  const onConfirm = () =>
    start(async () => {
      await action(domain);
      setOpen(false);
    });

  return (
    <>
      <button
        type="button"
        className={className ?? styles.btnDanger}
        onClick={() => setOpen(true)}
        disabled={pending}
      >
        {pending ? pendingLabel : label}
      </button>
      {open &&
        createPortal(
          <ConfirmDialog
            open
            title={title ?? `Delete ${domain}?`}
            confirmLabel={label}
            tone="danger"
            pending={pending}
            onConfirm={onConfirm}
            onCancel={() => {
              if (!pending) setOpen(false);
            }}
          >
            <p>This removes:</p>
            <ul>
              <li>
                The nginx site for <span className={styles.code}>{domain}</span>: it stops
                answering on HTTP and HTTPS.
              </li>
              <li>Its redirect rules.</li>
              <li>
                The A/AAAA records the panel added for it in its parent zone, if there are
                any.
              </li>
            </ul>
            <p>
              Website files and the SSL certificate stay on disk, so adding the domain again
              picks them up.
            </p>
          </ConfirmDialog>,
          document.body,
        )}
    </>
  );
}
