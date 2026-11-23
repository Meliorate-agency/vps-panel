import { redirect } from "next/navigation";
import { getSession } from "@/lib/auth";
import { db } from "@/lib/db";
import {
  startTotpEnrollAction,
  confirmTotpEnrollAction,
  verifyTotpAction,
} from "@/server-actions/auth";
import TotpClient from "./TotpClient";
import styles from "./page.module.css";

export const metadata = { title: "Two-factor" };

type Row = { totp_enabled: number; totp_secret: string | null };

export default async function TotpPage() {
  const s = await getSession();
  if (!s) redirect("/login");

  const row = db()
    .prepare(`SELECT totp_enabled, totp_secret FROM users WHERE id = ?`)
    .get(s.userId) as Row | undefined;

  // A session that still owes its code, on an account whose two-factor has
  // been switched off since (the agent's password reset clears it): there is
  // no code to verify, and enrolling takes a full session, so this page could
  // not be completed. Sign in again; /login does not look at the old cookie,
  // and signing in replaces it.
  if (s.needsTotp && !row?.totp_enabled) redirect("/login");

  const mode = row?.totp_enabled ? "verify" : "enroll";

  return (
    <div className={styles.shell}>
      <div className={styles.card}>
        <h1 className={styles.title}>Two-factor authentication</h1>
        <p className={styles.subtitle}>
          {mode === "enroll"
            ? "Scan the code with an authenticator app, then enter the 6-digit code."
            : "Enter the 6-digit code from your authenticator app."}
        </p>
        <TotpClient
          mode={mode}
          startEnrollAction={startTotpEnrollAction}
          confirmEnrollAction={confirmTotpEnrollAction}
          verifyAction={verifyTotpAction}
        />
      </div>
    </div>
  );
}
