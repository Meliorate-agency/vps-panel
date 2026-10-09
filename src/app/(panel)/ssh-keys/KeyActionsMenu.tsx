"use client";

import { useEffect, useRef, useState, useTransition } from "react";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";
import { copyText } from "@/lib/clipboard";
import styles from "@/components/page.module.css";

type FetchKeyResult =
  | { ok: true; content: string }
  | { ok: false; error: string };

type SaveResult =
  | { ok: true; fingerprint?: string | null }
  | { ok: false; error: string };

type ModalKind =
  | { kind: "none" }
  | {
      kind: "viewKeypair";
      loading: boolean;
      publicKey?: string;
      publicError?: string;
      privateKey?: string;
      privateError?: string;
    }
  | { kind: "confirmDelete" };

export default function KeyActionsMenu({
  user,
  fingerprint,
  keyType,
  comment,
  keypairName,
  authorized,
  fetchPublic,
  fetchPrivate,
  savePublic,
  savePrivate,
  deleteAction,
}: {
  user: string;
  fingerprint: string;
  keyType: string;
  comment: string;
  /** Null when there's no private-key file on disk for this row (paste-added
   *  keys, externally-added keys, etc.). The modal then shows only the
   *  public half. */
  keypairName: string | null;
  /** False for a keypair in ~/.ssh that is not in authorized_keys: its
   *  public half is read-only here (savePublic edits authorized_keys), and
   *  deleting it means deleting its files. */
  authorized: boolean;
  fetchPublic: (user: string, fp: string) => Promise<FetchKeyResult>;
  fetchPrivate: (user: string, name: string) => Promise<FetchKeyResult>;
  savePublic: (user: string, fp: string, content: string) => Promise<SaveResult>;
  savePrivate: (user: string, name: string, content: string) => Promise<SaveResult>;
  /** Third arg: keypair name to also purge from disk (files + config Host
   *  block). Only ever passed when the row has a `keypairName` and the
   *  operator ticked the checkbox in the delete dialog. */
  deleteAction: (user: string, fp: string, purgeKeypair?: string) => Promise<void>;
}) {
  const [menuOpen, setMenuOpen] = useState(false);
  const [modal, setModal] = useState<ModalKind>({ kind: "none" });
  const [deletePending, startDelete] = useTransition();
  const [deleteError, setDeleteError] = useState<string | null>(null);
  // Opt-in "also delete the keypair files" - without it, removing a
  // generated key leaves ~/.ssh/<name>{,.pub} on disk and the name stays
  // taken for future "Generate keypair" calls.
  const [purgeFiles, setPurgeFiles] = useState(false);
  const [copiedField, setCopiedField] = useState<"public" | "private" | null>(
    null,
  );
  const wrapRef = useRef<HTMLDivElement>(null);

  // Close the menu on outside click / ESC.
  useEffect(() => {
    if (!menuOpen) return;
    const onDown = (e: MouseEvent) => {
      if (wrapRef.current && !wrapRef.current.contains(e.target as Node)) {
        setMenuOpen(false);
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setMenuOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [menuOpen]);

  // Current fingerprint can change after a public-key edit — keep it in
  // state so subsequent fetches / saves use the latest value, not the
  // prop-frozen one.
  const [currentFp, setCurrentFp] = useState(fingerprint);
  useEffect(() => {
    setCurrentFp(fingerprint);
  }, [fingerprint]);

  const onViewKeypair = async () => {
    setMenuOpen(false);
    setCopiedField(null);
    setModal({ kind: "viewKeypair", loading: true });
    const [pubResp, privResp] = await Promise.all([
      fetchPublic(user, currentFp),
      keypairName
        ? fetchPrivate(user, keypairName)
        : Promise.resolve<FetchKeyResult>({
            ok: false,
            error:
              "No private key on disk for this row — only the public half was imported.",
          }),
    ]);
    setModal({
      kind: "viewKeypair",
      loading: false,
      publicKey: pubResp.ok ? pubResp.content : undefined,
      publicError: pubResp.ok ? undefined : pubResp.error,
      privateKey: privResp.ok ? privResp.content : undefined,
      privateError: privResp.ok ? undefined : privResp.error,
    });
  };

  const onAskDelete = () => {
    setMenuOpen(false);
    setDeleteError(null);
    setPurgeFiles(false);
    setModal({ kind: "confirmDelete" });
  };

  const onConfirmDelete = () => {
    setDeleteError(null);
    startDelete(async () => {
      try {
        await deleteAction(
          user,
          fingerprint,
          keypairName && (purgeFiles || !authorized) ? keypairName : undefined,
        );
        setModal({ kind: "none" });
      } catch (e) {
        setDeleteError(e instanceof Error ? e.message : String(e));
      }
    });
  };

  const closeModal = () => {
    if (deletePending) return;
    setModal({ kind: "none" });
    setCopiedField(null);
  };

  const copyToClipboard = async (text: string, which: "public" | "private") => {
    // Not copied: the operator can still select + copy by hand.
    if (!(await copyText(text))) return;
    setCopiedField(which);
    setTimeout(() => setCopiedField(null), 1500);
  };

  return (
    <div ref={wrapRef} style={{ position: "relative", display: "inline-block" }}>
      <button
        type="button"
        aria-label="Key actions"
        aria-haspopup="menu"
        aria-expanded={menuOpen}
        onClick={() => setMenuOpen((v) => !v)}
        style={{
          background: "transparent",
          border: 0,
          padding: "4px 10px",
          cursor: "pointer",
          color: "var(--text)",
          fontSize: 18,
          lineHeight: 1,
          borderRadius: 4,
          letterSpacing: 2,
        }}
        title="Actions"
      >
        ⋯
      </button>
      {menuOpen && (
        <div
          role="menu"
          style={{
            position: "absolute",
            right: 0,
            top: "100%",
            marginTop: 4,
            background: "var(--bg-elev)",
            border: "1px solid var(--border)",
            borderRadius: 6,
            boxShadow: "0 8px 24px rgba(0,0,0,0.4)",
            zIndex: 50,
            minWidth: 160,
            padding: 4,
            display: "flex",
            flexDirection: "column",
          }}
        >
          <MenuItem onClick={onViewKeypair}>View keypair</MenuItem>
          <div
            style={{
              height: 1,
              background: "var(--border)",
              margin: "4px 0",
            }}
          />
          <MenuItem onClick={onAskDelete} danger>
            Delete
          </MenuItem>
        </div>
      )}

      {/* View keypair — single modal showing public + private as editable
          textareas, each with its own Save button. Locked-dismiss so an
          accidental backdrop click or ESC doesn't throw away an
          in-progress edit; only the Close button at the bottom dismisses. */}
      <ConfirmDialog
        open={modal.kind === "viewKeypair"}
        title="Keypair"
        hideActions
        lockDismiss
        onConfirm={closeModal}
        onCancel={closeModal}
      >
        {modal.kind === "viewKeypair" && (
          <KeypairView
            loading={modal.loading}
            user={user}
            keyType={keyType}
            comment={comment}
            publicKey={modal.publicKey}
            publicError={modal.publicError}
            privateKey={modal.privateKey}
            privateError={modal.privateError}
            copiedField={copiedField}
            onCopy={copyToClipboard}
            onClose={closeModal}
            onSavePublic={
              authorized
                ? async (content) => {
                    const r = await savePublic(user, currentFp, content);
                    if (r.ok && r.fingerprint) {
                      // Fingerprint can change when the content changes — track it
                      // so the next fetch/save resolves against the new line.
                      setCurrentFp(r.fingerprint);
                    }
                    return r;
                  }
                : null
            }
            onSavePrivate={
              keypairName
                ? async (content) => savePrivate(user, keypairName, content)
                : null
            }
          />
        )}
      </ConfirmDialog>

      {/* Confirm delete. */}
      <ConfirmDialog
        open={modal.kind === "confirmDelete"}
        title={authorized ? "Remove SSH key?" : "Delete keypair?"}
        confirmLabel={authorized ? "Remove key" : "Delete keypair"}
        cancelLabel={authorized ? "Keep key" : "Keep keypair"}
        tone="danger"
        pending={deletePending}
        onConfirm={onConfirmDelete}
        onCancel={closeModal}
      >
        {authorized ? (
          <p style={{ marginTop: 0 }}>
            This deletes the public key from{" "}
            <span className={styles.code}>
              {user === "root"
                ? "/root/.ssh/authorized_keys"
                : `~${user}/.ssh/authorized_keys`}
            </span>
            . Sessions already authenticated with it stay connected, but new
            SSH attempts using this key will be refused.
          </p>
        ) : (
          <p style={{ marginTop: 0 }}>
            This deletes <span className={styles.code}>~/.ssh/{keypairName}</span> and its{" "}
            <span className={styles.code}>.pub</span>, and the matching{" "}
            <span className={styles.code}>Host</span> block in{" "}
            <span className={styles.code}>~/.ssh/config</span> if there is one. The key
            can&apos;t log in here; the server uses it to reach other hosts, so whatever uses
            it stops working. For a GitHub deploy key, pulls from that repository fail.
          </p>
        )}
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
            <strong>User:</strong> {user}
          </div>
          <div>
            <strong>Type:</strong> {keyType || "—"}
          </div>
          <div>
            <strong>Fingerprint:</strong> {fingerprint}
          </div>
          {comment && (
            <div>
              <strong>Comment:</strong> {comment}
            </div>
          )}
        </div>
        {keypairName && authorized && (
          <div
            style={{
              marginTop: 10,
              padding: "8px 10px",
              background: "rgba(255, 196, 0, 0.08)",
              border: "1px solid rgba(255, 196, 0, 0.28)",
              borderRadius: 6,
              fontSize: 12,
            }}
          >
            <label
              style={{
                display: "flex",
                alignItems: "flex-start",
                gap: 8,
                cursor: "pointer",
              }}
            >
              <input
                type="checkbox"
                checked={purgeFiles}
                onChange={(e) => setPurgeFiles(e.target.checked)}
                disabled={deletePending}
                style={{ marginTop: 2, flexShrink: 0 }}
              />
              <span>
                Also delete the keypair files{" "}
                <span className={styles.code}>~/.ssh/{keypairName}</span> +{" "}
                <span className={styles.code}>.pub</span> and the matching{" "}
                <span className={styles.code}>Host</span> block in{" "}
                <span className={styles.code}>~/.ssh/config</span>.
              </span>
            </label>
            <p style={{ margin: "8px 0 0", opacity: 0.85 }}>
              {purgeFiles
                ? "The private key is deleted for good - outbound git/SSH using this keypair will stop working. The name becomes free to re-create (as a brand-new key)."
                : "Left unchecked, the files stay on disk so outbound git operations keep working - but the name stays taken: generating a new keypair with this name will fail until the files are deleted."}
            </p>
          </div>
        )}
        {deleteError && (
          <div className={styles.error} style={{ marginTop: 10 }}>
            {deleteError}
          </div>
        )}
      </ConfirmDialog>
    </div>
  );
}

function MenuItem({
  children,
  onClick,
  disabled,
  danger,
  title,
}: {
  children: React.ReactNode;
  onClick: () => void;
  disabled?: boolean;
  danger?: boolean;
  title?: string;
}) {
  return (
    <button
      type="button"
      role="menuitem"
      onClick={onClick}
      disabled={disabled}
      title={title}
      style={{
        background: "transparent",
        border: 0,
        padding: "8px 12px",
        textAlign: "left",
        cursor: disabled ? "not-allowed" : "pointer",
        opacity: disabled ? 0.45 : 1,
        color: danger ? "var(--danger, #e0506e)" : "var(--text)",
        fontSize: 13,
        borderRadius: 4,
        font: "inherit",
      }}
      onMouseEnter={(e) => {
        if (!disabled) {
          (e.currentTarget as HTMLButtonElement).style.background =
            "var(--bg-elev-2)";
        }
      }}
      onMouseLeave={(e) => {
        (e.currentTarget as HTMLButtonElement).style.background = "transparent";
      }}
    >
      {children}
    </button>
  );
}

function KeypairView({
  loading,
  user,
  keyType,
  comment,
  publicKey,
  publicError,
  privateKey,
  privateError,
  copiedField,
  onCopy,
  onClose,
  onSavePublic,
  onSavePrivate,
}: {
  loading: boolean;
  user: string;
  keyType: string;
  comment: string;
  publicKey?: string;
  publicError?: string;
  privateKey?: string;
  privateError?: string;
  copiedField: "public" | "private" | null;
  onCopy: (text: string, which: "public" | "private") => void;
  onClose: () => void;
  /** Null for a keypair that is not a login key — section becomes read-only. */
  onSavePublic: ((content: string) => Promise<SaveResult>) | null;
  /** Null when there's no private file on disk — section becomes read-only. */
  onSavePrivate: ((content: string) => Promise<SaveResult>) | null;
}) {
  return (
    // text-align: left so labels, key contents and metadata aren't pulled
    // anywhere weird by an inherited alignment further up the tree.
    // Only the Close button row is explicitly justify-flex-end below.
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        gap: 12,
        textAlign: "left",
        alignItems: "stretch",
      }}
    >
      <div className={styles.muted} style={{ fontSize: 12, textAlign: "left" }}>
        <strong>User:</strong> {user} &middot; <strong>Type:</strong>{" "}
        {keyType || "—"}
        {comment && (
          <>
            {" "}
            &middot; <strong>Comment:</strong> {comment}
          </>
        )}
      </div>

      {loading ? (
        <div className={styles.muted}>Loading keypair…</div>
      ) : (
        <>
          <EditableKeySection
            label="Public key"
            initialContent={publicKey ?? ""}
            error={publicError}
            onCopy={(t) => onCopy(t, "public")}
            copied={copiedField === "public"}
            onSave={onSavePublic}
            rows={3}
          />
          <EditableKeySection
            label="Private key"
            initialContent={privateKey ?? ""}
            error={privateError}
            onCopy={(t) => onCopy(t, "private")}
            copied={copiedField === "private"}
            onSave={onSavePrivate}
            danger
            rows={9}
          />
        </>
      )}

      <div
        style={{
          display: "flex",
          justifyContent: "flex-end",
          marginTop: 4,
        }}
      >
        <button type="button" className={styles.btn} onClick={onClose}>
          Close
        </button>
      </div>
    </div>
  );
}

function EditableKeySection({
  label,
  initialContent,
  error,
  onCopy,
  copied,
  onSave,
  danger,
  rows,
}: {
  label: string;
  initialContent: string;
  error?: string;
  onCopy: (text: string) => void;
  copied: boolean;
  /** Null → render the textarea as readonly (no save button). Used when
   *  there's no underlying file to write back to (paste-imported public,
   *  missing private file). */
  onSave: ((content: string) => Promise<SaveResult>) | null;
  danger?: boolean;
  rows: number;
}) {
  const [value, setValue] = useState(initialContent);
  const [saving, setSaving] = useState(false);
  const [savedAt, setSavedAt] = useState<number | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);

  // Reset local state when a new key is loaded (modal re-opens with
  // different content) so the dirty flag starts fresh.
  useEffect(() => {
    setValue(initialContent);
    setSavedAt(null);
    setSaveError(null);
  }, [initialContent]);

  const dirty = value.trim() !== initialContent.trim() && value.trim().length > 0;
  const canSave = !!onSave && dirty && !saving;

  const onSaveClick = async () => {
    if (!onSave || !dirty) return;
    setSaving(true);
    setSaveError(null);
    setSavedAt(null);
    const r = await onSave(value);
    setSaving(false);
    if (r.ok) {
      // "Reset" the baseline so the button greys back out until the next
      // edit. Use the just-saved value as the new initial.
      setSavedAt(Date.now());
      // Auto-fade the "Saved" indicator after a few seconds.
      setTimeout(() => setSavedAt(null), 3000);
    } else {
      setSaveError(r.error);
    }
  };

  if (error) {
    return (
      <div style={{ textAlign: "left" }}>
        <strong style={{ fontSize: 13 }}>{label}</strong>
        <div
          className={styles.muted}
          style={{
            background: "var(--bg-elev-2)",
            border: "1px dashed var(--border)",
            borderRadius: 6,
            padding: "8px 10px",
            fontSize: 12,
            marginTop: 4,
          }}
        >
          {error}
        </div>
      </div>
    );
  }

  return (
    <div style={{ textAlign: "left" }}>
      <div
        style={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          marginBottom: 4,
          gap: 8,
        }}
      >
        <strong style={{ fontSize: 13 }}>
          {label}
          {danger && (
            <span
              style={{
                marginLeft: 8,
                color: "var(--danger, #e0506e)",
                fontWeight: 500,
                fontSize: 11,
              }}
            >
              (sensitive — don&apos;t paste anywhere persistent)
            </span>
          )}
        </strong>
        {value.trim() && (
          <button
            type="button"
            className={styles.btnGhost}
            style={{ fontSize: 11, padding: "3px 8px", flexShrink: 0 }}
            onClick={() => onCopy(value.trim())}
          >
            {copied ? "Copied!" : "Copy"}
          </button>
        )}
      </div>
      <textarea
        value={value}
        onChange={(e) => setValue(e.target.value)}
        rows={rows}
        spellCheck={false}
        readOnly={!onSave}
        style={{
          width: "100%",
          boxSizing: "border-box",
          background: "var(--bg-elev-2)",
          border: `1px solid ${danger ? "rgba(255, 77, 109, 0.35)" : "var(--border)"}`,
          borderRadius: 6,
          padding: 10,
          fontSize: 11,
          fontFamily: "var(--mono)",
          color: "var(--text)",
          resize: "vertical",
          whiteSpace: "pre",
          overflowWrap: "normal",
          wordBreak: "normal",
          overflow: "auto",
          textAlign: "left",
          opacity: onSave ? 1 : 0.85,
        }}
      />
      {onSave && (
        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "flex-end",
            gap: 10,
            marginTop: 6,
          }}
        >
          {saveError && (
            <span
              style={{
                color: "var(--danger, #e0506e)",
                fontSize: 12,
                marginRight: "auto",
              }}
            >
              {saveError}
            </span>
          )}
          {savedAt && !saveError && (
            <span
              style={{
                color: "var(--ok, #4ade80)",
                fontSize: 12,
                fontWeight: 600,
              }}
            >
              Saved
            </span>
          )}
          <button
            type="button"
            onClick={onSaveClick}
            disabled={!canSave}
            className={canSave ? styles.btn : undefined}
            style={
              canSave
                ? { fontSize: 12, padding: "5px 14px" }
                : {
                    fontSize: 12,
                    padding: "5px 14px",
                    background: "var(--bg-elev-2)",
                    border: "1px solid var(--border)",
                    color: "var(--text-dim)",
                    borderRadius: 6,
                    cursor: "not-allowed",
                    fontWeight: 600,
                  }
            }
            title={
              !onSave
                ? "Read-only — no file on disk to write back to"
                : !dirty
                  ? "No changes to save"
                  : "Save changes"
            }
          >
            {saving ? "Saving…" : "Save"}
          </button>
        </div>
      )}
    </div>
  );
}
