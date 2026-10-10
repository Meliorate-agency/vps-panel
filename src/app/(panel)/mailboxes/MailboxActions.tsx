"use client";

import { useActionState, useEffect, useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";
import pageStyles from "@/components/page.module.css";
import local from "./mailboxes.module.css";
import { generatePassword } from "@/lib/passwordGen";
import { copyText } from "@/lib/clipboard";

type UpdateResult =
  | { ok: true; data?: { password?: string } }
  | { ok: false; error: string };

/** Why webmail can't open yet: Roundcube is not installed, or it has no
 *  public domain. null when a domain proxies to it. */
export type WebmailOff = "install" | "domain" | null;

type Props = {
  email: string;
  server: string;
  webmailUrl: string | null;
  webmailOff: WebmailOff;
  currentQuotaMb: number;
  currentUsageMb: number;
  updateAction: (prev: UpdateResult | null, fd: FormData) => Promise<UpdateResult>;
  deleteAction: () => Promise<void>;
};

type View = "menu" | "edit" | "delete";

/**
 * Mobile-only kebab + centered action modal for one mailbox row.
 *
 * Desktop already has all the per-row controls spread across cells (Copy
 * config, Open mailbox, Edit, Delete). On phones those collapse into a
 * single 3-dot button rendered here, which opens a centered modal with
 * the same four actions plus a Close button. Edit and Delete swap the
 * modal body to their own sub-views so the whole flow stays inside one
 * dialog.
 *
 * Visibility is owned by CSS: the wrapper `.mobileKebab` is `display:
 * none` above 768px so this component is invisible on desktop.
 */
export default function MailboxActions({
  email,
  server,
  webmailUrl,
  webmailOff,
  currentQuotaMb,
  currentUsageMb,
  updateAction,
  deleteAction,
}: Props) {
  const router = useRouter();
  const [open, setOpen] = useState(false);
  const [view, setView] = useState<View>("menu");
  const [copied, setCopied] = useState(false);

  // Edit form state (driven by useActionState so the form posts naturally).
  const [editState, editFormAction, editPending] = useActionState<
    UpdateResult | null,
    FormData
  >(updateAction, null);

  // Edit form: controlled password so "Generate" can fill it in.
  const [editPassword, setEditPassword] = useState("");

  // Delete state — typed-confirm requires the user to type the email.
  const [deleteTyped, setDeleteTyped] = useState("");
  const [deletePending, startDelete] = useTransition();

  const minQuota = currentUsageMb + 1;
  const domain = email.split("@")[1] ?? "";
  const mailHost = domain ? `mail.${domain}` : server;

  function closeAll() {
    setOpen(false);
    setView("menu");
    setDeleteTyped("");
    setEditPassword("");
  }

  // Quota-only change succeeded → close the modal + refresh the row.
  // Password change succeeded → keep the dialog open so the operator can
  // copy the generated password; close happens on their explicit click.
  useEffect(() => {
    if (editState?.ok === true && !editState.data?.password) {
      closeAll();
      router.refresh();
    }
  }, [editState, router]);

  async function copyConfig() {
    const text = [
      `Mail server:  ${mailHost}`,
      `IMAP:         ${mailHost}:993 (SSL/TLS)`,
      `SMTP:         ${mailHost}:587 (STARTTLS)`,
      `POP3:         ${mailHost}:995 (SSL/TLS)`,
      `Username:     ${email}`,
    ].join("\n");
    if (!(await copyText(text))) {
      setCopied(false);
      return;
    }
    setCopied(true);
    setTimeout(() => {
      setCopied(false);
      closeAll();
    }, 1100);
  }

  function openMailbox() {
    if (!webmailUrl) return;
    window.open(
      `${webmailUrl}/?_user=${encodeURIComponent(email)}`,
      "_blank",
      "noopener",
    );
    closeAll();
  }

  function onDeleteConfirm() {
    if (deleteTyped !== email || deletePending) return;
    startDelete(async () => {
      await deleteAction();
      closeAll();
      router.refresh();
    });
  }

  return (
    <div className={local.mobileKebab}>
      <button
        type="button"
        onClick={() => setOpen(true)}
        className={local.kebab}
        aria-label={`Actions for ${email}`}
        title="Actions"
      >
        <svg width="18" height="18" viewBox="0 0 18 18" fill="currentColor" aria-hidden="true">
          <circle cx="9" cy="4" r="1.6" />
          <circle cx="9" cy="9" r="1.6" />
          <circle cx="9" cy="14" r="1.6" />
        </svg>
      </button>

      {open && view === "menu" && (
        <ConfirmDialog
          open
          title={email}
          hideActions
          onCancel={closeAll}
          onConfirm={() => {}}
        >
          <div className={local.menuList}>
            <button
              type="button"
              className={local.menuBtn}
              onClick={copyConfig}
            >
              <span className={local.menuLabel}>
                {copied ? "Copied!" : "Copy config"}
              </span>
              <span className={local.menuHint}>IMAP / SMTP / POP3 details</span>
            </button>
            <button
              type="button"
              className={local.menuBtn}
              onClick={openMailbox}
              disabled={!webmailUrl}
            >
              <span className={local.menuLabel}>Open mailbox</span>
              <span className={local.menuHint}>
                {webmailUrl
                  ? "Roundcube webmail"
                  : webmailOff === "install"
                    ? "Off: install Roundcube under Apps first"
                    : "Off: give Roundcube a public domain first"}
              </span>
            </button>
            <button
              type="button"
              className={local.menuBtn}
              onClick={() => setView("edit")}
            >
              <span className={local.menuLabel}>Edit</span>
              <span className={local.menuHint}>Password / quota</span>
            </button>
            <button
              type="button"
              className={`${local.menuBtn} ${local.menuDanger}`}
              onClick={() => setView("delete")}
            >
              <span className={local.menuLabel}>Delete</span>
              <span className={local.menuHint}>Mail moves to Files &gt; Trash for 30 days</span>
            </button>
          </div>
          <div className={local.menuFoot}>
            <button
              type="button"
              className={pageStyles.btnGhost}
              onClick={closeAll}
            >
              Close
            </button>
          </div>
        </ConfirmDialog>
      )}

      {open && view === "edit" && (
        <ConfirmDialog
          open
          title={`Edit ${email}`}
          hideActions
          onCancel={closeAll}
          onConfirm={() => {}}
        >
          <form action={editFormAction}>
            <div className={pageStyles.formRow}>
              <label className={pageStyles.field}>
                New password (optional)
                <div className={local.pwRow}>
                  <input
                    name="password"
                    className={pageStyles.input}
                    type="text"
                    placeholder="leave blank to keep current"
                    autoComplete="new-password"
                    value={editPassword}
                    onChange={(e) => setEditPassword(e.target.value)}
                    spellCheck={false}
                  />
                  <button
                    type="button"
                    className={pageStyles.btnGhost}
                    onClick={() => setEditPassword(generatePassword())}
                    title="Fill in a random 20-character password"
                  >
                    Generate
                  </button>
                </div>
                <span className={local.hint}>
                  Generate fills in a strong random password. It is applied when you save.
                </span>
              </label>
              <label className={pageStyles.field}>
                Quota (MB)
                <input type="hidden" name="current_quota_mb" value={currentQuotaMb} />
                <input
                  name="quota_mb"
                  className={pageStyles.input}
                  type="number"
                  defaultValue={currentQuotaMb}
                  step={1}
                />
                <span className={local.hint}>
                  Current usage: {currentUsageMb} MB. A new quota must be at least {minQuota} MB; leave it as is to change only the password.
                </span>
              </label>
            </div>
            {editState?.ok === false && (
              <div className={pageStyles.error}>{editState.error}</div>
            )}
            {editState?.ok === true && editState.data?.password && (
              <div className={pageStyles.notice}>
                New password (copy it now, it will not be shown again):{" "}
                <code className={pageStyles.code}>{editState.data.password}</code>
              </div>
            )}
            <div className={local.menuFoot}>
              <button
                type="button"
                className={pageStyles.btnGhost}
                onClick={editState?.ok === true && editState.data?.password ? closeAll : () => setView("menu")}
                disabled={editPending}
              >
                {editState?.ok === true && editState.data?.password ? "Close" : "Back"}
              </button>
              <button
                type="submit"
                className={pageStyles.btn}
                disabled={editPending}
              >
                {editPending ? "..." : "Save changes"}
              </button>
            </div>
          </form>
        </ConfirmDialog>
      )}

      {open && view === "delete" && (
        <ConfirmDialog
          open
          title="Delete mailbox?"
          hideActions
          onCancel={closeAll}
          onConfirm={() => {}}
        >
          <div className={local.deleteTarget}>
            <code className={pageStyles.code}>{email}</code>
          </div>
          <p>
            The address stops working immediately. Its stored mail moves to{" "}
            <strong>Files &gt; Trash</strong>, where it can be restored for 30 days
            before it is deleted for good.
          </p>
          <label className={pageStyles.field}>
            Type the mailbox email to confirm
            <input
              className={pageStyles.input}
              value={deleteTyped}
              onChange={(e) => setDeleteTyped(e.target.value)}
              autoComplete="off"
              autoCorrect="off"
              autoCapitalize="off"
              spellCheck={false}
              disabled={deletePending}
              placeholder={email}
            />
          </label>
          <div className={local.menuFoot}>
            <button
              type="button"
              className={pageStyles.btnGhost}
              onClick={() => setView("menu")}
              disabled={deletePending}
            >
              Back
            </button>
            <button
              type="button"
              className={pageStyles.btnDanger}
              onClick={onDeleteConfirm}
              disabled={deleteTyped !== email || deletePending}
              aria-busy={deletePending}
            >
              {deletePending ? "Deleting..." : "Delete"}
            </button>
          </div>
        </ConfirmDialog>
      )}
    </div>
  );
}
