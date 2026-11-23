"use client";

import { useState } from "react";
import styles from "@/components/page.module.css";

/** What the action hands back: nothing, or its `{ ok, error }` result. */
type Outcome = void | { ok: true } | { ok: false; error: string };

/**
 * A <form> around one bound server action that takes no fields (Restart,
 * Adopt, Renew, Unban, ...). The action reports a failure by returning
 * `{ ok: false, error }` and the message is shown next to the button: an
 * error thrown by a server action has its message replaced by Next in a
 * production build, and ends up on the page's error screen.
 *
 * The children are the submit button (PendingButton, ConfirmSubmitButton or a
 * plain one) and whatever else the form showed before.
 */
export default function ActionForm({
  action,
  children,
  className,
  style,
}: {
  action: () => Promise<Outcome>;
  children: React.ReactNode;
  className?: string;
  style?: React.CSSProperties;
}) {
  const [error, setError] = useState<string | null>(null);
  return (
    <form
      className={className}
      style={style}
      action={async () => {
        setError(null);
        const res = await action();
        if (res && !res.ok) setError(res.error);
      }}
    >
      {children}
      {error && (
        <span
          role="alert"
          className={styles.error}
          style={{ display: "inline-block", fontSize: 12, margin: "4px 0 0" }}
        >
          {error}
        </span>
      )}
    </form>
  );
}
