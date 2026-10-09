"use client";

import { useActionState, useEffect, useState } from "react";
import type { ActionResult } from "@/server-actions/auth";
import styles from "./page.module.css";

type StartResult = { secret: string; qrDataUrl: string; uri: string } | { error: string };

type Props = {
  mode: "enroll" | "verify";
  startEnrollAction: () => Promise<StartResult>;
  confirmEnrollAction: (prev: ActionResult | null, fd: FormData) => Promise<ActionResult>;
  verifyAction: (prev: ActionResult | null, fd: FormData) => Promise<ActionResult>;
};

export default function TotpClient({
  mode,
  startEnrollAction,
  confirmEnrollAction,
  verifyAction,
}: Props) {
  const [enroll, setEnroll] = useState<StartResult | null>(null);
  const [confirmState, confirmFormAction, confirmPending] = useActionState<ActionResult | null, FormData>(
    confirmEnrollAction,
    null,
  );
  const [verifyState, verifyFormAction, verifyPending] = useActionState<ActionResult | null, FormData>(
    verifyAction,
    null,
  );

  useEffect(() => {
    if (mode === "enroll" && !enroll) {
      startEnrollAction().then(setEnroll);
    }
  }, [mode, enroll, startEnrollAction]);

  if (mode === "enroll") {
    if (!enroll) return <div className={styles.subtitle}>Generating secret...</div>;
    if ("error" in enroll) return <div className={styles.error}>{enroll.error}</div>;
    return (
      <>
        <div className={styles.qrBlock}>
          <img src={enroll.qrDataUrl} alt="TOTP QR code" className={styles.qr} />
          <div className={styles.secret}>
            <span className={styles.secretLabel}>Or enter manually:</span>
            <code className={styles.secretValue}>{enroll.secret}</code>
          </div>
        </div>
        <form action={confirmFormAction} className={styles.form}>
          <label className={styles.label}>
            6-digit code
            <input
              name="code"
              inputMode="numeric"
              pattern="[0-9]{6}"
              maxLength={6}
              required
              autoFocus
              className={styles.input}
            />
          </label>
          {confirmState && confirmState.ok === false && (
            <div className={styles.error}>{confirmState.error}</div>
          )}
          {confirmState && confirmState.ok === true && (
            <div className={styles.notice}>
              TOTP enabled. Reload the page to sign in with the second factor.
            </div>
          )}
          <button type="submit" className={styles.submit} disabled={confirmPending}>
            {confirmPending ? "Verifying..." : "Confirm"}
          </button>
        </form>
      </>
    );
  }

  return (
    <form action={verifyFormAction} className={styles.form}>
      <label className={styles.label}>
        6-digit code
        <input
          name="code"
          inputMode="numeric"
          pattern="[0-9]{6}"
          maxLength={6}
          required
          autoFocus
          className={styles.input}
        />
      </label>
      {verifyState && verifyState.ok === false && (
        <div className={styles.error}>{verifyState.error}</div>
      )}
      <button type="submit" className={styles.submit} disabled={verifyPending}>
        {verifyPending ? "Verifying..." : "Verify"}
      </button>
    </form>
  );
}
