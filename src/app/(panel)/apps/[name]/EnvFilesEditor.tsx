"use client";

import { useCallback, useEffect, useState } from "react";
import type { AppEnvFile, AppEnvFilesResp } from "@/lib/agent";
import {
  listAppEnvFilesAction,
  saveAppEnvFileAction,
  deleteAppEnvFileAction,
} from "@/server-actions/apps";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";
import styles from "@/components/page.module.css";

const APPS_BASE_DIR = "/srv/apps";

const MONO = "var(--font-mono, ui-monospace, SFMono-Regular, monospace)";

const cardStyle: React.CSSProperties = {
  border: "1px solid var(--border)",
  borderRadius: 8,
  padding: "12px 14px",
  marginBottom: 12,
  background: "var(--bg-elev-2)",
};

type Props = {
  name: string;
  /** Server-fetched file list (detail page) so there's no load flash. When
   *  omitted (deploy form), the component fetches on mount. */
  initialData?: AppEnvFilesResp | null;
  /** Deploy-form context: the app may not have a systemd unit yet, so a
   *  "restart" can be a no-op. Tweaks the copy accordingly. */
  preDeploy?: boolean;
};

type SaveStatus =
  | { kind: "idle" }
  | { kind: "saving" }
  | { kind: "ok"; msg: string }
  | { kind: "err"; msg: string };

export default function EnvFilesEditor({
  name,
  initialData = null,
  preDeploy = false,
}: Props) {
  const [data, setData] = useState<AppEnvFilesResp | null>(initialData);
  const [loading, setLoading] = useState(initialData === null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    const r = await listAppEnvFilesAction(name);
    if (r.ok) setData(r.data);
    else setError(r.error);
    setLoading(false);
  }, [name]);

  useEffect(() => {
    if (initialData === null) load();
  }, [load, initialData]);

  const sourceDir = data?.source_dir ?? `${APPS_BASE_DIR}/${name}`;

  return (
    <div>
      <div
        style={{
          display: "flex",
          justifyContent: "space-between",
          alignItems: "center",
          marginBottom: 10,
          gap: 12,
        }}
      >
        <p className={styles.muted} style={{ margin: 0, fontSize: 13 }}>
          Files starting with <span className={styles.code}>.env</span> inside{" "}
          <span className={styles.code}>{sourceDir}</span> - the ones your app
          loads at build/run time (e.g.{" "}
          <span className={styles.code}>.env.local</span>).
        </p>
        <button
          type="button"
          className={styles.btnGhost}
          onClick={load}
          disabled={loading}
          style={{ flexShrink: 0 }}
        >
          {loading ? "Scanning..." : "Rescan"}
        </button>
      </div>

      {error && <div className={styles.error}>{error}</div>}

      {data && (
        <>
          {data.files.length === 0 && !loading && (
            <p className={styles.muted} style={{ fontSize: 13 }}>
              No <span className={styles.code}>.env</span> files found yet.
              Create one below.
            </p>
          )}

          {data.truncated && (
            <p className={styles.muted} style={{ fontSize: 12 }}>
              Showing the first {data.files.length} files - more exist on disk.
            </p>
          )}

          {data.files.map((f) => (
            <FileCard
              key={f.path}
              name={name}
              file={f}
              preDeploy={preDeploy}
              onDeleted={load}
            />
          ))}

          <CreateForm
            name={name}
            sourceDir={sourceDir}
            preDeploy={preDeploy}
            onCreated={load}
          />
        </>
      )}
    </div>
  );
}

function Chevron({ open }: { open: boolean }) {
  return (
    <svg
      width="14"
      height="14"
      viewBox="0 0 16 16"
      fill="none"
      aria-hidden="true"
      style={{
        flexShrink: 0,
        color: "var(--text-dim)",
        transition: "transform 0.15s ease",
        transform: open ? "rotate(90deg)" : "none",
      }}
    >
      <path
        d="M6 4l4 4-4 4"
        stroke="currentColor"
        strokeWidth="1.8"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function TrashIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 16 16" fill="none" aria-hidden="true">
      <path
        d="M2.5 4h11M6.5 4V2.75A.75.75 0 0 1 7.25 2h1.5a.75.75 0 0 1 .75.75V4m2.25 0v9.25a.75.75 0 0 1-.75.75h-7a.75.75 0 0 1-.75-.75V4M6.5 7.25v3.5M9.5 7.25v3.5"
        stroke="currentColor"
        strokeWidth="1.3"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function FileCard({
  name,
  file,
  preDeploy,
  onDeleted,
}: {
  name: string;
  file: AppEnvFile;
  preDeploy: boolean;
  onDeleted: () => Promise<void>;
}) {
  const [open, setOpen] = useState(false);
  const [content, setContent] = useState(file.content);
  const [status, setStatus] = useState<SaveStatus>({ kind: "idle" });
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [delErr, setDelErr] = useState<string | null>(null);

  const dirty = !file.too_large && content !== file.content;

  const save = async (restart: boolean) => {
    setStatus({ kind: "saving" });
    const r = await saveAppEnvFileAction(name, file.path, content, restart);
    if (r.ok) {
      setStatus({ kind: "ok", msg: savedMessage(restart, r.restarted, preDeploy) });
    } else {
      setStatus({ kind: "err", msg: r.error });
    }
  };

  const onDelete = async () => {
    setDeleting(true);
    setDelErr(null);
    const r = await deleteAppEnvFileAction(name, file.path, false);
    setDeleting(false);
    if (r.ok) {
      setConfirmOpen(false);
      await onDeleted();
    } else {
      setDelErr(r.error);
    }
  };

  const rows = Math.min(24, Math.max(6, content.split("\n").length + 1));
  const saving = status.kind === "saving";

  return (
    <div style={cardStyle}>
      {/* Header row: the toggle button (left) + a delete button (right). */}
      <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
        <button
          type="button"
          onClick={() => setOpen((o) => !o)}
          aria-expanded={open}
          style={{
            display: "flex",
            alignItems: "center",
            gap: 10,
            flex: 1,
            minWidth: 0,
            padding: 0,
            margin: 0,
            background: "transparent",
            border: "none",
            cursor: "pointer",
            textAlign: "left",
            color: "inherit",
            font: "inherit",
          }}
        >
          <Chevron open={open} />
          <span style={{ flex: 1, minWidth: 0 }}>
            <span className={styles.code} style={{ fontSize: 13, fontWeight: 600 }}>
              {file.path}
            </span>
            <span
              className={styles.muted}
              style={{
                display: "block",
                fontSize: 12,
                marginTop: 3,
                fontFamily: MONO,
                wordBreak: "break-all",
              }}
            >
              {file.abs_path}
            </span>
          </span>
          {dirty && !open && (
            <span
              title="Unsaved changes"
              style={{
                flexShrink: 0,
                width: 7,
                height: 7,
                borderRadius: "50%",
                background: "var(--accent)",
              }}
            />
          )}
          {file.too_large && (
            <span className={styles.muted} style={{ flexShrink: 0, fontSize: 12 }}>
              {(file.size / 1024).toFixed(0)} KB
            </span>
          )}
        </button>
        <button
          type="button"
          aria-label={`Delete ${file.path}`}
          title="Delete file"
          disabled={deleting}
          onClick={() => {
            setDelErr(null);
            setConfirmOpen(true);
          }}
          style={{
            flexShrink: 0,
            display: "inline-flex",
            alignItems: "center",
            justifyContent: "center",
            width: 32,
            height: 32,
            padding: 0,
            background: "transparent",
            border: "1px solid var(--border)",
            borderRadius: 6,
            color: "var(--text-dim)",
            cursor: deleting ? "not-allowed" : "pointer",
          }}
        >
          <TrashIcon />
        </button>
      </div>

      {open && (
        <div style={{ marginTop: 12 }}>
          {file.too_large ? (
            <p className={styles.muted} style={{ fontSize: 13, margin: 0 }}>
              {(file.size / 1024).toFixed(0)} KB - too large to edit here.
            </p>
          ) : (
            <>
              <textarea
                className={styles.textarea}
                rows={rows}
                value={content}
                spellCheck={false}
                onChange={(e) => setContent(e.target.value)}
                style={{
                  width: "100%",
                  boxSizing: "border-box",
                  display: "block",
                  resize: "vertical",
                  fontFamily: MONO,
                  fontSize: 13,
                }}
              />
              {status.kind === "err" && (
                <div className={styles.error} style={{ marginTop: 8 }}>
                  {status.msg}
                </div>
              )}
              {status.kind === "ok" && (
                <div className={styles.notice} style={{ marginTop: 8 }}>
                  {status.msg}
                </div>
              )}
              <div
                style={{
                  display: "flex",
                  gap: 8,
                  justifyContent: "flex-end",
                  marginTop: 10,
                }}
              >
                <button
                  type="button"
                  className={styles.btnGhost}
                  disabled={saving}
                  onClick={() => save(false)}
                >
                  {saving ? "Saving..." : "Save"}
                </button>
                <button
                  type="button"
                  className={styles.btn}
                  disabled={saving}
                  onClick={() => save(true)}
                >
                  {saving ? "Saving..." : "Save & restart"}
                </button>
              </div>
            </>
          )}
        </div>
      )}

      <ConfirmDialog
        open={confirmOpen}
        title={`Delete ${file.path}?`}
        confirmLabel="Delete"
        cancelLabel="Cancel"
        tone="danger"
        pending={deleting}
        onConfirm={onDelete}
        onCancel={() => {
          if (deleting) return;
          setConfirmOpen(false);
        }}
      >
        <p style={{ marginTop: 0 }}>
          Permanently deletes this file from disk. It does not restart the app —
          use the restart control if the running app needs to drop it.
        </p>
        <div
          style={{
            background: "var(--bg-elev-2)",
            border: "1px solid var(--border)",
            borderRadius: 6,
            padding: "10px 12px",
            fontSize: 12,
            fontFamily: MONO,
            marginTop: 10,
            wordBreak: "break-all",
          }}
        >
          {file.abs_path}
        </div>
        {delErr && (
          <div className={styles.error} style={{ marginTop: 10 }}>
            {delErr}
          </div>
        )}
      </ConfirmDialog>
    </div>
  );
}

function CreateForm({
  name,
  sourceDir,
  preDeploy,
  onCreated,
}: {
  name: string;
  sourceDir: string;
  preDeploy: boolean;
  onCreated: () => Promise<void>;
}) {
  const [path, setPath] = useState(".env");
  const [content, setContent] = useState("");
  const [status, setStatus] = useState<SaveStatus>({ kind: "idle" });

  const create = async (restart: boolean) => {
    const trimmed = path.trim();
    const base = trimmed.split("/").pop() ?? "";
    if (!base.startsWith(".env")) {
      setStatus({
        kind: "err",
        msg: "File name must start with .env (e.g. .env, .env.local, .env.production).",
      });
      return;
    }
    setStatus({ kind: "saving" });
    const r = await saveAppEnvFileAction(name, trimmed, content, restart);
    if (r.ok) {
      setStatus({ kind: "ok", msg: savedMessage(restart, r.restarted, preDeploy) });
      setContent("");
      setPath(".env");
      await onCreated();
    } else {
      setStatus({ kind: "err", msg: r.error });
    }
  };

  const saving = status.kind === "saving";

  return (
    <div
      style={{
        ...cardStyle,
        borderStyle: "dashed",
        marginTop: 4,
      }}
    >
      <div style={{ marginBottom: 8, fontSize: 13, fontWeight: 600 }}>
        Create env file
      </div>
      <p className={styles.muted} style={{ fontSize: 12, marginTop: 0, marginBottom: 8 }}>
        Defaults to <span className={styles.code}>.env</span> at the folder
        root. Edit the path for a different name or a subfolder (e.g.{" "}
        <span className={styles.code}>.env.production</span> or{" "}
        <span className={styles.code}>apps/web/.env.local</span>).
      </p>

      {/* Fixed source-dir prefix + editable relative path, mirroring the
          deploy form's source-folder control. */}
      <div
        className={styles.input}
        style={{
          display: "flex",
          alignItems: "center",
          gap: 0,
          padding: 0,
          overflow: "hidden",
          marginBottom: 8,
        }}
      >
        <span
          className={styles.muted}
          style={{
            padding: "8px 4px 8px 10px",
            background: "rgba(255,255,255,0.04)",
            userSelect: "none",
            fontFamily: MONO,
            fontSize: 13,
            whiteSpace: "nowrap",
          }}
        >
          {sourceDir}/
        </span>
        <input
          value={path}
          onChange={(e) => setPath(e.target.value)}
          spellCheck={false}
          autoComplete="off"
          aria-label="Env file path relative to the source folder"
          placeholder=".env"
          style={{
            flex: 1,
            minWidth: 0,
            padding: "8px 10px",
            border: "none",
            outline: "none",
            background: "transparent",
            color: "inherit",
            fontFamily: MONO,
            fontSize: 13,
          }}
        />
      </div>

      <textarea
        className={styles.textarea}
        rows={6}
        value={content}
        spellCheck={false}
        placeholder={"DATABASE_URL=postgres://...\nNEXT_PUBLIC_API=https://..."}
        onChange={(e) => setContent(e.target.value)}
        style={{
          width: "100%",
          boxSizing: "border-box",
          display: "block",
          resize: "vertical",
          fontFamily: MONO,
          fontSize: 13,
        }}
      />

      {status.kind === "err" && (
        <div className={styles.error} style={{ marginTop: 8 }}>
          {status.msg}
        </div>
      )}
      {status.kind === "ok" && (
        <div className={styles.notice} style={{ marginTop: 8 }}>
          {status.msg}
        </div>
      )}

      <div
        style={{
          display: "flex",
          gap: 8,
          justifyContent: "flex-end",
          marginTop: 10,
        }}
      >
        <button
          type="button"
          className={styles.btnGhost}
          disabled={saving}
          onClick={() => create(false)}
        >
          {saving ? "Creating..." : "Create"}
        </button>
        <button
          type="button"
          className={styles.btn}
          disabled={saving}
          onClick={() => create(true)}
        >
          {saving ? "Creating..." : "Create & restart"}
        </button>
      </div>
    </div>
  );
}

function savedMessage(
  restart: boolean,
  restarted: string | null,
  preDeploy: boolean,
): string {
  if (!restart) return "Saved.";
  if (restarted) return `Saved. Restarted ${restarted}.`;
  return preDeploy
    ? "Saved. No service to restart yet - it'll pick this up on Deploy."
    : "Saved. No running service found to restart.";
}
