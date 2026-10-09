"use client"; // Error boundaries must be Client Components

import Link from "next/link";
import { useEffect } from "react";
import styles from "@/components/page.module.css";

/**
 * Fallback for any error thrown while rendering a panel page or by a server
 * action that returns nothing (Delete, Restart, Adopt, Renew, ...). Without
 * this boundary one failed action replaced the whole panel with Next's error
 * screen. In production the server hides the real message and sends only a
 * digest, so the page says where to find it.
 */
export default function PanelError({
  error,
  unstable_retry,
}: {
  error: Error & { digest?: string };
  unstable_retry: () => void;
}) {
  useEffect(() => {
    console.error(error);
  }, [error]);

  return (
    <div className={styles.section} role="alert">
      <h2 className={styles.sectionTitle}>That did not work</h2>
      <p className={styles.error} style={{ marginTop: 8 }}>
        {error.digest
          ? "The action failed on the server."
          : error.message || "Something went wrong."}
      </p>
      {error.digest && (
        <p className={styles.muted} style={{ marginTop: 8 }}>
          The details are in the panel log. On the server run{" "}
          <code>journalctl -u vps-panel | grep {error.digest}</code>. Tasks the agent ran
          are listed under <Link href="/tasks">Tasks</Link>, and every change under{" "}
          <Link href="/audit">Audit</Link>.
        </p>
      )}
      <div style={{ display: "flex", gap: 8, marginTop: 12 }}>
        <button className={styles.btn} onClick={() => unstable_retry()}>
          Try again
        </button>
        <Link className={styles.btnGhost} href="/dashboard">
          Dashboard
        </Link>
      </div>
    </div>
  );
}
