"use client";

import { useState, useTransition } from "react";
import styles from "@/components/page.module.css";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";

type Result =
  | { ok: true; data?: { password: string } }
  | { ok: false; error: string };

/**
 * Reset the password of a MariaDB user or a Postgres role from a popup.
 * Engine-agnostic: the page binds the target identity into `action`, so this
 * component only deals with the password string. Leave the field blank to let
 * the server generate one; on success the new password is shown once (the
 * panel never stores it), with a reminder to update the app's DB config.
 */
export default function ChangePasswordButton({
  identity,
  action,
}: {
  identity: string;
  action: (password: string) => Promise<Result>;
}) {
  const [open, setOpen] = useState(false);
  const [password, setPassword] = useState("");
  const [newPassword, setNewPassword] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pending, start] = useTransition();

  const openDialog = () => {
    setPassword("");
    setNewPassword(null);
    setError(null);
    setOpen(true);
  };

  const onConfirm = () => {
    if (pending) return;
    // Second click once the new password is shown just closes the dialog.
    if (newPassword) {
      setOpen(false);
      return;
    }
    setError(null);
    start(async () => {
      const res = await action(password.trim());
      if (res.ok) {
        setNewPassword(res.data?.password ?? password.trim());
      } else {
        setError(res.error);
      }
    });
  };

  return (
    <>
      <button
        type="button"
        className={styles.btnGhost}
        onClick={openDialog}
        disabled={pending}
      >
        Password
      </button>

      <ConfirmDialog
        open={open}
        title={`Change password for ${identity}`}
        confirmLabel={
          pending ? "Saving…" : newPassword ? "Done" : "Set password"
        }
        cancelLabel={newPassword ? "Close" : "Cancel"}
        pending={pending}
        onConfirm={onConfirm}
        onCancel={() => {
          if (pending) return;
          setOpen(false);
        }}
      >
        {newPassword ? (
          <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
            <p style={{ margin: 0 }}>
              New password for <span className={styles.code}>{identity}</span>:
            </p>
            <code
              className={styles.code}
              style={{ fontSize: 14, userSelect: "all", wordBreak: "break-all" }}
            >
              {newPassword}
            </code>
            <span className={styles.muted} style={{ fontSize: 12 }}>
              Copy it now — the panel won&apos;t show it again. Put this exact
              value in your app&apos;s database config and restart the app.
            </span>
          </div>
        ) : (
          <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
            <label className={styles.field}>
              New password
              <input
                className={styles.input}
                type="text"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                placeholder="leave blank to generate"
                autoComplete="off"
              />
            </label>
            <span className={styles.muted} style={{ fontSize: 12 }}>
              Any app still using the old password will stop connecting until you
              update its config.
            </span>
            {error && <div className={styles.error}>{error}</div>}
          </div>
        )}
      </ConfirmDialog>
    </>
  );
}
