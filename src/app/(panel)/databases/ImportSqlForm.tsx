"use client";

import { useRef, useState } from "react";
import { useRouter } from "next/navigation";
import TaskProgress from "@/components/TaskProgress/TaskProgress";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";
import styles from "@/components/page.module.css";
import type { ActionResult } from "@/server-actions/databases";

/**
 * Client-side preview of what the agent will detect server-side: the
 * database a mariadb-dump / mysqldump file targets, read from its header.
 * Scanning a 64 KB slice is instant and needs no round trip; the agent
 * re-detects authoritatively over the first 256 KB before importing.
 */
const CREATE_RE =
  /^\s*CREATE\s+DATABASE\s+(?:\/\*!\d+\s+IF\s+NOT\s+EXISTS\s*\*\/\s*|IF\s+NOT\s+EXISTS\s+)?`?([^`\r\n;\s]+)`?/im;
const USE_RE = /^\s*USE\s+`?([^`\r\n;\s]+)`?\s*;/im;

/** Mirrors the agent's reserved-schema list so the obvious mistake fails
 *  before a 300 MB upload, not after. */
const RESERVED_DBS = new Set([
  "information_schema",
  "performance_schema",
  "mysql",
  "sys",
  "pdns",
  "mail",
  "panel",
]);

type Phase =
  | { kind: "idle" }
  | { kind: "uploading"; pct: number }
  | { kind: "starting" }
  | { kind: "running"; taskId: string; database: string }
  | { kind: "done"; taskId: string; database: string };

type ImportInput = {
  path: string;
  targetDb?: string;
  createTarget?: boolean;
  dropFirst?: boolean;
};

type Props = {
  dbs: { name: string }[];
  /** Upload limit from Settings; the upload route enforces it too. */
  maxUploadMb: number;
  prepareAction: () => Promise<ActionResult>;
  importAction: (
    input: ImportInput,
  ) => Promise<ActionResult<{ taskId: string; database: string }>>;
};

/** XHR rather than fetch purely for upload.onprogress — fetch still can't
 *  report request-body progress, and a 300 MB upload with no feedback is
 *  indistinguishable from a hung one. Same pattern as the /files browser. */
function putFile(
  url: string,
  file: File,
  onProgress: (fraction: number) => void,
  signal: AbortSignal,
): Promise<{ ok: boolean; status: number; body: string }> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open("POST", url);
    xhr.upload.onprogress = (e) => {
      if (e.lengthComputable) onProgress(e.loaded / e.total);
    };
    xhr.onload = () =>
      resolve({ ok: xhr.status < 400, status: xhr.status, body: xhr.responseText });
    xhr.onerror = () =>
      reject(new Error("network error — the connection dropped mid-upload"));
    xhr.ontimeout = () => reject(new Error("upload timed out"));
    xhr.onabort = () => reject(new DOMException("aborted", "AbortError"));
    signal.addEventListener("abort", () => xhr.abort(), { once: true });
    xhr.send(file);
  });
}

function uploadErrorText(status: number, body: string): string {
  try {
    return (JSON.parse(body) as { error?: string }).error ?? body;
  } catch {
    if (status === 413)
      return "rejected as too large by the web server (nginx client_max_body_size)";
    return body || `upload failed (${status})`;
  }
}

function formatMb(bytes: number): string {
  return `${Math.max(1, Math.round(bytes / (1024 * 1024)))} MB`;
}

/**
 * Upload a `.sql` dump and restore it into MariaDB, end to end:
 * pick file → instant header scan (does the dump name its own database?) →
 * stream the file into Drive's `sql-dumps/` folder → agent task pipes it
 * through the mariadb CLI → live log streams below. Closing the page mid-run
 * is safe — the task keeps going and its log lives on /tasks.
 */
export default function ImportSqlForm({ dbs, maxUploadMb, prepareAction, importAction }: Props) {
  const router = useRouter();
  const [file, setFile] = useState<File | null>(null);
  // null = no header found (headerless dump); undefined = nothing scanned yet
  const [detected, setDetected] = useState<string | null | undefined>(undefined);
  const [targetDb, setTargetDb] = useState("");
  const [newDbName, setNewDbName] = useState("");
  const [dropFirst, setDropFirst] = useState(false);
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });
  const [error, setError] = useState<string | null>(null);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const abortRef = useRef<AbortController | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

  const busy = phase.kind === "uploading" || phase.kind === "starting";
  const creatingNew = targetDb === "__new__";
  const effectiveDb = detected ?? (creatingNew ? newDbName.trim() : targetDb);

  const onPickFile = async (f: File | null) => {
    setFile(f);
    setDetected(undefined);
    setError(null);
    setPhase({ kind: "idle" });
    if (!f) return;
    if (!/\.sql$/i.test(f.name)) {
      setError("Pick a .sql file (plain SQL dump — decompress .gz/.zip first).");
      setFile(null);
      return;
    }
    if (f.size > maxUploadMb * 1024 * 1024) {
      setError(
        `This dump is ${formatMb(f.size)}, larger than the ${maxUploadMb} MB upload limit (Settings > Panel > Uploads).`,
      );
      setFile(null);
      return;
    }
    try {
      const head = await f.slice(0, 64 * 1024).text();
      const m = CREATE_RE.exec(head) ?? USE_RE.exec(head);
      setDetected(m ? m[1] : null);
    } catch {
      // Preview only — the agent re-scans server-side either way.
      setDetected(null);
    }
  };

  const validate = (): string | null => {
    if (!file) return "Pick a .sql file first.";
    if (!effectiveDb) return "Choose a target database for this dump.";
    if (RESERVED_DBS.has(effectiveDb.toLowerCase()))
      return `\`${effectiveDb}\` is a reserved system database — it can't be an import target.`;
    return null;
  };

  const start = async () => {
    const v = validate();
    if (v || !file) {
      setError(v);
      return;
    }
    setError(null);

    // 1. Ensure drive:sql-dumps/ exists (409 = already there = fine).
    setPhase({ kind: "uploading", pct: 0 });
    const prep = await prepareAction();
    if (!prep.ok) {
      setError(prep.error);
      setPhase({ kind: "idle" });
      return;
    }

    // 2. Stream the dump into Drive through the existing upload route (the
    //    one nginx location with the multi-GB body carve-out). Overwrite so
    //    re-importing the same filename replaces the stale copy.
    const controller = new AbortController();
    abortRef.current = controller;
    const q = new URLSearchParams({
      root: "drive",
      path: "sql-dumps",
      name: file.name,
      overwrite: "1",
    });
    try {
      const res = await putFile(
        `/api/files/upload?${q.toString()}`,
        file,
        (fraction) =>
          setPhase({ kind: "uploading", pct: Math.round(fraction * 100) }),
        controller.signal,
      );
      if (!res.ok) {
        setError(uploadErrorText(res.status, res.body));
        setPhase({ kind: "idle" });
        return;
      }
    } catch (e: unknown) {
      if (e instanceof DOMException && e.name === "AbortError") {
        setError("Upload cancelled.");
      } else {
        setError(e instanceof Error ? e.message : String(e));
      }
      setPhase({ kind: "idle" });
      return;
    } finally {
      abortRef.current = null;
    }

    // 3. Hand off to the agent task and stream its log.
    setPhase({ kind: "starting" });
    const res = await importAction({
      path: `sql-dumps/${file.name}`,
      targetDb: detected ? undefined : effectiveDb,
      createTarget: !detected && creatingNew,
      dropFirst,
    });
    if (!res.ok) {
      setError(res.error);
      setPhase({ kind: "idle" });
      return;
    }
    setPhase({
      kind: "running",
      taskId: res.data!.taskId,
      database: res.data!.database,
    });
  };

  const submit = () => {
    const v = validate();
    if (v) {
      setError(v);
      return;
    }
    // Dropping a database first is the one destructive path — confirm it.
    if (dropFirst) setConfirmOpen(true);
    else void start();
  };

  const reset = () => {
    setFile(null);
    setDetected(undefined);
    setTargetDb("");
    setNewDbName("");
    setDropFirst(false);
    setError(null);
    setPhase({ kind: "idle" });
    if (fileInputRef.current) fileInputRef.current.value = "";
  };

  if (phase.kind === "running" || phase.kind === "done") {
    return (
      <div>
        <div className={styles.notice} style={{ marginBottom: 10 }}>
          {phase.kind === "running" ? (
            <>
              Importing into <span className={styles.code}>{phase.database}</span>.
              Leaving this page is safe — the import keeps running and its log
              lives on the Tasks page.
            </>
          ) : (
            <>
              Import into <span className={styles.code}>{phase.database}</span>{" "}
              finished. The dump file stays in Drive under{" "}
              <span className={styles.code}>sql-dumps/</span> — delete it there
              if you don&apos;t want it kept (it&apos;s included in backups).
            </>
          )}
        </div>
        {/* Stays mounted after completion so the final summary line
            ("Import complete: N tables ...") remains readable. */}
        <TaskProgress
          taskId={phase.taskId}
          onComplete={(s) => {
            if (s === "succeeded") {
              setPhase({
                kind: "done",
                taskId: phase.taskId,
                database: phase.database,
              });
              router.refresh();
            }
            // On failure keep the log visible; the error lines are in it.
          }}
        />
        <div style={{ marginTop: 10 }}>
          <button type="button" className={styles.btnGhost} onClick={reset}>
            Import another dump
          </button>
        </div>
      </div>
    );
  }

  return (
    <div>
      <div style={{ display: "flex", gap: 8, alignItems: "flex-end", flexWrap: "wrap" }}>
        <label className={styles.field} style={{ flex: "2 1 260px" }}>
          SQL dump file
          <input
            ref={fileInputRef}
            type="file"
            accept=".sql"
            className={styles.input}
            disabled={busy}
            onChange={(e) => void onPickFile(e.target.files?.[0] ?? null)}
          />
        </label>

        {file && detected === null && (
          <label className={styles.field} style={{ flex: "1 1 180px" }}>
            Target database
            <select
              className={styles.input}
              value={targetDb}
              disabled={busy}
              onChange={(e) => setTargetDb(e.target.value)}
            >
              <option value="">Choose…</option>
              {dbs.map((d) => (
                <option key={d.name} value={d.name}>
                  {d.name}
                </option>
              ))}
              <option value="__new__">New database…</option>
            </select>
          </label>
        )}

        {file && detected === null && creatingNew && (
          <label className={styles.field} style={{ flex: "1 1 160px" }}>
            New database name
            <input
              className={styles.input}
              value={newDbName}
              disabled={busy}
              placeholder="myapp_prod"
              onChange={(e) => setNewDbName(e.target.value)}
            />
          </label>
        )}

        <button
          type="button"
          className={styles.btn}
          disabled={busy || !file}
          onClick={submit}
        >
          {phase.kind === "uploading"
            ? `Uploading… ${phase.pct}%`
            : phase.kind === "starting"
              ? "Starting…"
              : "Upload & import"}
        </button>

        {phase.kind === "uploading" && (
          <button
            type="button"
            className={styles.btnGhost}
            onClick={() => abortRef.current?.abort()}
          >
            Cancel
          </button>
        )}
      </div>

      {file && detected !== undefined && (
        <div className={styles.muted} style={{ marginTop: 8, fontSize: 12 }}>
          {detected ? (
            <>
              This dump creates and imports into{" "}
              <span className={styles.code}>{detected}</span> (
              {formatMb(file.size)}) — no target needed.
            </>
          ) : (
            <>
              No <span className={styles.code}>CREATE DATABASE</span> /{" "}
              <span className={styles.code}>USE</span> header found (
              {formatMb(file.size)}) — everything runs against the target you
              pick.
            </>
          )}
        </div>
      )}

      {file && (
        <label
          className={styles.checkbox}
          style={{ marginTop: 8, display: "inline-flex", gap: 6 }}
        >
          <input
            type="checkbox"
            checked={dropFirst}
            disabled={busy}
            onChange={(e) => setDropFirst(e.target.checked)}
          />
          Drop and recreate the database first (clean re-import — deletes all
          current data in it)
        </label>
      )}

      {phase.kind === "uploading" && (
        <div
          style={{
            marginTop: 10,
            height: 6,
            borderRadius: 3,
            background: "var(--border)",
            overflow: "hidden",
            maxWidth: 480,
          }}
        >
          <div
            style={{
              width: `${phase.pct}%`,
              height: "100%",
              background: "var(--accent)",
              transition: "width .2s",
            }}
          />
        </div>
      )}

      {error && (
        <div className={styles.error} style={{ marginTop: 10 }}>
          {error}
        </div>
      )}

      <ConfirmDialog
        open={confirmOpen}
        tone="danger"
        title={`Drop ${effectiveDb || "the database"} before importing?`}
        confirmLabel="Drop & import"
        cancelLabel="Cancel"
        onConfirm={() => {
          setConfirmOpen(false);
          void start();
        }}
        onCancel={() => setConfirmOpen(false)}
      >
        <p style={{ marginTop: 0 }}>
          This permanently deletes the current{" "}
          <span className={styles.code}>{effectiveDb}</span> database —{" "}
          <strong>all tables and data in it</strong> — and then restores it
          from the dump. This cannot be undone.
        </p>
      </ConfirmDialog>
    </div>
  );
}
