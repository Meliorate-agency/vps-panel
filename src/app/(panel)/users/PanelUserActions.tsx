"use client";

import { useActionState, useEffect, useRef, useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";
import styles from "@/components/page.module.css";

type Result = { ok: true } | { ok: false; error: string };

type Props = {
  userId: number;
  email: string;
  role: string;
  /** Bound to a specific userId on the page side. */
  resetAction: (
    prev: Result | null,
    fd: FormData,
  ) => Promise<Result>;
  /** Bound to a specific userId on the page side. */
  deleteAction: () => Promise<void>;
};

/**
 * Renders the "Reset password" + "Delete" buttons for one panel-user row.
 * Each button opens its own ConfirmDialog modal — the reset dialog embeds a
 * fresh password form so admins can set the new value without leaving the
 * page; the delete dialog uses the existing danger tone with a typed-
 * confirm-style explanation but a single click to confirm (consistent with
 * the rest of the panel's delete UX).
 */
export default function PanelUserActions({
  userId,
  email,
  role,
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
      // Auto-close after a beat so the operator sees the success notice.
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
  const [deletePending, startDelete] = useTransition();
  const [deleteError, setDeleteError] = useState<string | null>(null);

  const onConfirmDelete = () => {
    setDeleteError(null);
    startDelete(async () => {
      try {
        await deleteAction();
        // Server action calls revalidatePath; tell the router to swap in the
        // fresh server-rendered tree so the row disappears immediately.
        router.refresh();
        setDeleteOpen(false);
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
        title={`Reset password for ${email}`}
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
          Sets a new password for this user (argon2id) and revokes any open
          sessions so they have to sign in again. The user&apos;s email and
          role are unchanged.
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
              Password reset. Existing sessions revoked.
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
        title={`Delete ${email}?`}
        confirmLabel={deletePending ? "Deleting..." : "Delete user"}
        cancelLabel="Cancel"
        tone="danger"
        pending={deletePending}
        onConfirm={onConfirmDelete}
        onCancel={() => {
          if (deletePending) return;
          setDeleteOpen(false);
        }}
      >
        <p style={{ marginTop: 0 }}>
          Permanently removes the panel-login account and revokes every active
          session for this user.
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
            <strong style={{ fontFamily: "inherit" }}>Email:</strong> {email}
          </div>
          <div>
            <strong style={{ fontFamily: "inherit" }}>Role:</strong> {role}
          </div>
        </div>
        <div className={styles.muted} style={{ fontSize: 12, marginTop: 12 }}>
          The last admin can&apos;t be deleted — if this is the only admin
          account, the server action will refuse and you&apos;ll see an error
          below.
        </div>
        {deleteError && (
          <div className={styles.error} style={{ marginTop: 10 }}>
            {deleteError}
          </div>
        )}
      </ConfirmDialog>
    </>
  );
}
