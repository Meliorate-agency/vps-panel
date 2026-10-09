"use client";

import { useEffect, useState } from "react";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";
import styles from "@/components/page.module.css";

type FetchResult = { ok: true; content: string } | { ok: false; error: string };
type SaveResult = { ok: true } | { ok: false; error: string };

/**
 * Inline "view + edit raw file" button. Used on the SSH keys page to give
 * per-user access to `~/.ssh/authorized_keys` and `~/.ssh/config`. Opens a
 * centered editable dialog (lockDismiss so click-outside / ESC don't drop
 * in-progress edits) with Save + Close at the bottom. Save flips disabled
 * → active when the content differs from what was loaded; flips back to
 * disabled + shows "Saved" after a successful write.
 */
export default function SshFileButton({
  user,
  filePath,
  label,
  fetchAction,
  saveAction,
}: {
  user: string;
  /** Display-only path string, e.g. "~/.ssh/authorized_keys". */
  filePath: string;
  /** Button label. */
  label: string;
  fetchAction: (user: string) => Promise<FetchResult>;
  saveAction: (user: string, content: string) => Promise<SaveResult>;
}) {
  const [open, setOpen] = useState(false);
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [initial, setInitial] = useState("");
  const [value, setValue] = useState("");
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [savedAt, setSavedAt] = useState<number | null>(null);

  useEffect(() => {
    if (!open) return;
    setLoading(true);
    setLoadError(null);
    setSaveError(null);
    setSavedAt(null);
    fetchAction(user).then((r) => {
      setLoading(false);
      if (r.ok) {
        setInitial(r.content);
        setValue(r.content);
      } else {
        setLoadError(r.error);
      }
    });
  }, [open, user, fetchAction]);

  const dirty = value !== initial;
  const canSave = !saving && dirty;

  const onSaveClick = async () => {
    if (!canSave) return;
    setSaving(true);
    setSaveError(null);
    setSavedAt(null);
    const r = await saveAction(user, value);
    setSaving(false);
    if (r.ok) {
      setInitial(value);
      setSavedAt(Date.now());
      setTimeout(() => setSavedAt(null), 3000);
    } else {
      setSaveError(r.error);
    }
  };

  const onCloseClick = () => {
    if (saving) return;
    setOpen(false);
  };

  return (
    <>
      <button
        type="button"
        className={styles.btnGhost}
        onClick={() => setOpen(true)}
        style={{ fontSize: 12, padding: "5px 10px" }}
      >
        {label}
      </button>

      <ConfirmDialog
        open={open}
        title={`${user} · ${filePath}`}
        hideActions
        lockDismiss
        onConfirm={onCloseClick}
        onCancel={onCloseClick}
      >
        <div
          style={{
            display: "flex",
            flexDirection: "column",
            gap: 10,
            textAlign: "left",
          }}
        >
          {loading ? (
            <div className={styles.muted}>Loading…</div>
          ) : loadError ? (
            <div className={styles.error}>{loadError}</div>
          ) : (
            <>
              <textarea
                value={value}
                onChange={(e) => setValue(e.target.value)}
                spellCheck={false}
                rows={16}
                style={{
                  width: "100%",
                  boxSizing: "border-box",
                  background: "var(--bg-elev-2)",
                  border: "1px solid var(--border)",
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
                  minHeight: "30vh",
                }}
              />
              <div
                className={styles.muted}
                style={{ fontSize: 12, textAlign: "left" }}
              >
                Whole-file overwrite. File is chmod 0600 and chown to{" "}
                <span className={styles.code}>{user}</span> on save.
              </div>
            </>
          )}

          {saveError && (
            <div className={styles.error}>{saveError}</div>
          )}

          <div
            style={{
              display: "flex",
              alignItems: "center",
              gap: 10,
              justifyContent: "flex-end",
              marginTop: 4,
            }}
          >
            {savedAt && !saveError && (
              <span
                style={{
                  color: "var(--ok, #4ade80)",
                  fontSize: 12,
                  fontWeight: 600,
                  marginRight: "auto",
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
                  ? { fontSize: 12, padding: "6px 14px" }
                  : {
                      fontSize: 12,
                      padding: "6px 14px",
                      background: "var(--bg-elev-2)",
                      border: "1px solid var(--border)",
                      color: "var(--text-dim)",
                      borderRadius: 6,
                      cursor: "not-allowed",
                      fontWeight: 600,
                    }
              }
              title={!dirty ? "No changes to save" : "Save file"}
            >
              {saving ? "Saving…" : "Save"}
            </button>
            <button
              type="button"
              className={styles.btnGhost}
              onClick={onCloseClick}
              disabled={saving}
              style={{ fontSize: 12, padding: "6px 14px" }}
            >
              Close
            </button>
          </div>
        </div>
      </ConfirmDialog>
    </>
  );
}
