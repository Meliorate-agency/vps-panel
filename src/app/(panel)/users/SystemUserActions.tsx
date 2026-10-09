"use client";

import { useActionState, useEffect, useRef, useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";
import styles from "@/components/page.module.css";

type Result = { ok: true } | { ok: false; error: string };

type Props = {
  name: string;
  home: string;
  /** Bound to `name` on the page side. */
  resetAction: (prev: Result | null, fd: FormData) => Promise<Result>;
  /** Bound to `name` on the page side. Takes a `removeHome` arg the dialog
   *  toggles via a checkbox. */
  deleteAction: (removeHome: boolean) => Promise<void>;
};

/**
 * Per-row controls for a Linux user: opens a Reset-password dialog (sets a
 * new password via the agent's `chpasswd` wrapper) and a Delete dialog
 * (calls `userdel`, optionally `-r` to nuke the home dir). Modelled on the
 * existing PanelUserActions component so the /users page UX is consistent
 * across its panel-users and linux-users tables.
 */
export default function SystemUserActions({
  name,
  home,
  resetAction,
  deleteAction,
}: Props) {
  const router = useRouter();

  // ─── Reset password dialog ─────────────────────────────────────────
  const [resetOpen, setResetOpen] = useState(false);
  const [revealed, setRevealed] = useState(false);
  const formRef = useRef<HTMLFormElement>(null);
  const [resetState, resetFormAction, resetPending] = useActionState<
    Result | null,
    FormData
  >(resetAction, null);

  useEffect(() => {
    if (resetState?.ok === true && resetOpen) {
      const t = setTimeout(() => {
        setResetOpen(false);
        setRevealed(false);
        formRef.current?.reset();
      }, 900);
      return () => clearTimeout(t);
    }
  }, [resetState, resetOpen]);

  // ─── Delete dialog ─────────────────────────────────────────────────
  const [deleteOpen, setDeleteOpen] = useState(false);
  const [removeHome, setRemoveHome] = useState(false);
  const [deletePending, startDelete] = useTransition();
  const [deleteError, setDeleteError] = useState<string | null>(null);

  const onConfirmDelete = () => {
    setDeleteError(null);
    startDelete(async () => {
      try {
        await deleteAction(removeHome);
        router.refresh();
        setDeleteOpen(false);
        setRemoveHome(false);
      } catch (e: unknown) {
        const msg = e instanceof Error ? e.message : String(e);
        setDeleteError(msg);
      }
    });
  };

  return (
    <>
      <div style={{ display: "flex", gap: 6 }}>
        <button
          type="button"
          className={styles.btnGhost}
          onClick={() => {
            setRevealed(false);
            setResetOpen(true);
          }}
          style={{ fontSize: 12, padding: "4px 10px" }}
        >
          Reset password
        </button>
        <button
          type="button"
          className={styles.btnDanger}
          onClick={() => {
            setDeleteError(null);
            setRemoveHome(false);
            setDeleteOpen(true);
          }}
          style={{ fontSize: 12, padding: "4px 10px" }}
        >
          Delete
        </button>
      </div>

      {/* Reset password dialog */}
      <ConfirmDialog
        open={resetOpen}
        title={`Reset password for ${name}`}
        hideActions
        pending={resetPending}
        onConfirm={() => formRef.current?.requestSubmit()}
        onCancel={() => {
          if (resetPending) return;
          setResetOpen(false);
          setRevealed(false);
        }}
      >
        <p style={{ marginTop: 0 }}>
          Pipes <span className={styles.code}>{name}:&lt;pw&gt;</span> into{" "}
          <span className={styles.code}>chpasswd</span> as root. SSH password
          login still depends on{" "}
          <span className={styles.code}>PasswordAuthentication</span> in
          sshd_config (see{" "}
          <a href="/settings#ssh-server" className={styles.code}>
            /settings → SSH server
          </a>
          ).
        </p>
        <form ref={formRef} action={resetFormAction}>
          <div className={styles.formRow}>
            <label className={styles.field}>
              New password
              <input
                name="password"
                type={revealed ? "text" : "password"}
                className={styles.input}
                autoComplete="new-password"
                minLength={12}
                required
              />
            </label>
            <label className={styles.field}>
              Confirm
              <input
                name="confirm"
                type={revealed ? "text" : "password"}
                className={styles.input}
                autoComplete="new-password"
                minLength={12}
                required
              />
            </label>
          </div>
          <label className={styles.checkbox} style={{ marginBottom: 8 }}>
            <input
              type="checkbox"
              checked={revealed}
              onChange={(e) => setRevealed(e.target.checked)}
            />
            Show passwords
          </label>

          {resetState?.ok === false && (
            <div className={styles.error} style={{ marginBottom: 10 }}>
              {resetState.error}
            </div>
          )}
          {resetState?.ok === true && (
            <div className={styles.notice} style={{ marginBottom: 10 }}>
              Password reset.
            </div>
          )}

          <div
            style={{
              display: "flex",
              justifyContent: "flex-end",
              gap: 8,
              marginTop: 12,
            }}
          >
            <button
              type="button"
              className={styles.btnGhost}
              onClick={() => {
                if (resetPending) return;
                setResetOpen(false);
                setRevealed(false);
              }}
              disabled={resetPending}
            >
              Close
            </button>
            <button
              type="submit"
              className={styles.btn}
              disabled={resetPending}
            >
              {resetPending ? "Resetting..." : "Reset password"}
            </button>
          </div>
        </form>
      </ConfirmDialog>

      {/* Delete dialog */}
      <ConfirmDialog
        open={deleteOpen}
        title={`Delete Linux user ${name}?`}
        confirmLabel={deletePending ? "Deleting..." : "Delete user"}
        cancelLabel="Cancel"
        tone="danger"
        pending={deletePending}
        onConfirm={onConfirmDelete}
        onCancel={() => {
          if (deletePending) return;
          setDeleteOpen(false);
          setRemoveHome(false);
        }}
      >
        <p style={{ marginTop: 0 }}>
          Runs <span className={styles.code}>userdel</span> on the host. Any
          processes owned by this user are sent SIGKILL first so userdel
          won&apos;t refuse with &quot;currently logged in&quot;.
        </p>
        <div
          style={{
            background: "var(--bg-elev-2)",
            border: "1px solid var(--border)",
            borderRadius: 6,
            padding: "10px 12px",
            fontSize: 12,
            fontFamily: "var(--mono)",
            marginTop: 10,
            wordBreak: "break-all",
          }}
        >
          <div>
            <strong style={{ fontFamily: "inherit" }}>User:</strong> {name}
          </div>
          <div>
            <strong style={{ fontFamily: "inherit" }}>Home:</strong> {home}
          </div>
        </div>
        <label className={styles.checkbox} style={{ marginTop: 12 }}>
          <input
            type="checkbox"
            checked={removeHome}
            onChange={(e) => setRemoveHome(e.target.checked)}
            disabled={deletePending}
          />
          Also delete <span className={styles.code}>{home}</span> and the mail
          spool (<span className={styles.code}>userdel -r</span>)
        </label>
        {deleteError && (
          <div className={styles.error} style={{ marginTop: 10 }}>
            {deleteError}
          </div>
        )}
      </ConfirmDialog>
    </>
  );
}
