"use client";

import { useCallback, useEffect, useRef, useState, useTransition } from "react";
import Link from "next/link";
import type { FileEntry, FileListing } from "@/lib/agent";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";
import pageStyles from "@/components/page.module.css";
import styles from "../files.module.css";
import {
  createFileAction,
  createFolderAction,
  listDirAction,
  moveAction,
  renameAction,
  trashAction,
} from "@/server-actions/files";

type DialogKind = "newFolder" | "newFile" | "rename" | "delete";

/** Join a directory path and a name, both relative to the root. */
function joinPath(dir: string, name: string): string {
  return dir ? `${dir}/${name}` : name;
}

function parentOf(path: string): string {
  const i = path.lastIndexOf("/");
  return i === -1 ? "" : path.slice(0, i);
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let u = 0;
  while (v >= 1024 && u < units.length - 1) {
    v /= 1024;
    u++;
  }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[u]}`;
}

/** "2026-08-15T14:03:07Z" -> "2026-08-15 14:03". Sliced rather than
 *  locale-formatted so the server and client render identical markup. */
function formatTs(iso: string | null): string {
  if (!iso) return "-";
  return `${iso.slice(0, 10)} ${iso.slice(11, 16)}`;
}

function isDirLike(e: FileEntry): boolean {
  return e.kind === "dir" || (e.kind === "symlink" && e.link_is_dir);
}

function extOf(name: string): string {
  const i = name.lastIndexOf(".");
  return i === -1 ? "" : name.slice(i + 1).toLowerCase();
}

const IMAGE_EXT = new Set(["png", "jpg", "jpeg", "gif", "webp", "avif", "bmp", "ico"]);
const TEXT_EXT = new Set([
  "txt", "md", "log", "csv", "xml", "yaml", "yml", "toml", "ini", "conf",
  "sh", "rs", "ts", "tsx", "js", "jsx", "css", "sql", "json", "env",
]);

/** What the preview modal can render. SVG and HTML are excluded on purpose —
 *  the agent refuses to serve them inline (they can carry script, and the
 *  panel's own origin is what would run it), so they'd render as a broken
 *  frame anyway. Download them instead. */
function previewKind(name: string): "image" | "pdf" | "text" | null {
  const ext = extOf(name);
  if (IMAGE_EXT.has(ext)) return "image";
  if (ext === "pdf") return "pdf";
  if (TEXT_EXT.has(ext) || ext === "") return "text";
  return null;
}

function downloadUrl(root: string, path: string, inline = false): string {
  const q = new URLSearchParams({ root, path });
  if (inline) q.set("inline", "1");
  return `/api/files/download?${q.toString()}`;
}

/** Cap on what the text preview pulls down — a 200 MB log shouldn't be
 *  streamed into a <pre> just because it ends in `.log`. */
const TEXT_PREVIEW_LIMIT = 512 * 1024;

type UploadResult = { ok: boolean; status: number; body: string };

/**
 * POST one file, reporting progress as it goes.
 *
 * XHR rather than `fetch` purely for `upload.onprogress` — fetch still can't
 * report request-body progress in any shipping browser, and an upload with no
 * feedback is indistinguishable from a hung one.
 */
function putFile(
  url: string,
  file: File,
  onProgress: (fraction: number) => void,
  signal: AbortSignal,
): Promise<UploadResult> {
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

/** Pull the agent's error text out of a JSON body, falling back to the raw
 *  body — which for a proxy-level rejection is HTML, not JSON. */
function errorText(res: UploadResult): string {
  try {
    return (JSON.parse(res.body) as { error?: string }).error ?? res.body;
  } catch {
    if (res.status === 413) {
      return "rejected as too large by the web server (nginx client_max_body_size)";
    }
    return res.body.slice(0, 200) || `upload failed (${res.status})`;
  }
}

/**
 * The file manager itself.
 *
 * Navigation updates the URL with `history.pushState` and swaps the listing in
 * place, so clicking through a deep tree doesn't cost a full route render.
 * Back/forward still work: those go through the router, which re-renders the
 * server component and remounts this one via its `key`.
 *
 * `maxUploadMb` is the upload limit from Settings; the route enforces it too,
 * this only saves sending a file that would be refused.
 */
export default function FileBrowser({
  initial,
  maxUploadMb,
}: {
  initial: FileListing;
  maxUploadMb: number;
}) {
  const [listing, setListing] = useState<FileListing>(initial);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [dialog, setDialog] = useState<DialogKind | null>(null);
  const [nameInput, setNameInput] = useState("");
  const [pending, startTransition] = useTransition();
  // Which drop target is lit: a directory name, "..", or `crumb:<path>`.
  const [dropTarget, setDropTarget] = useState<string | null>(null);
  // dataTransfer can't be read during dragover, so the payload lives here too.
  const dragPaths = useRef<string[]>([]);
  const nameRef = useRef<HTMLInputElement>(null);
  const fileInput = useRef<HTMLInputElement>(null);
  const uploadAbort = useRef<AbortController | null>(null);
  // Upload progress: which file, how far through the batch, and how far
  // through this file.
  const [uploading, setUploading] = useState<{
    name: string;
    done: number;
    total: number;
    pct: number;
  } | null>(null);
  // True while a drag carrying OS files is over the list.
  const [fileDrag, setFileDrag] = useState(false);
  const [preview, setPreview] = useState<FileEntry | null>(null);
  const [previewText, setPreviewText] = useState<string | null>(null);

  // ConfirmDialog focuses its Confirm button on open, which lands AFTER the
  // input's autoFocus and steals it. Claim focus back once, on open, so a
  // "New folder" dialog is immediately typeable.
  useEffect(() => {
    if (dialog === "newFolder" || dialog === "newFile" || dialog === "rename") {
      const input = nameRef.current;
      if (!input) return;
      input.focus();
      input.select();
    }
  }, [dialog]);

  const root = listing.root;
  const path = listing.path;

  const load = useCallback(
    (next: string, push: boolean) => {
      startTransition(async () => {
        const res = await listDirAction(root.id, next);
        if (!res.ok) {
          setError(res.error);
          return;
        }
        setListing(res.listing);
        setSelected(new Set());
        setError(null);
        setNotice(null);
        if (push) {
          const url = next
            ? `/files/${root.id}?path=${encodeURIComponent(next)}`
            : `/files/${root.id}`;
          window.history.pushState(null, "", url);
        }
      });
    },
    [root.id],
  );

  const reload = useCallback(() => load(path, false), [load, path]);

  const selectedEntries = listing.entries.filter((e) => selected.has(e.name));
  const canActOnSelection =
    selectedEntries.length > 0 && selectedEntries.every((e) => !e.locked) && root.writable;

  const toggle = (name: string) =>
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(name)) next.delete(name);
      else next.add(name);
      return next;
    });

  // --- drag and drop --------------------------------------------------------

  const onDragStart = (e: React.DragEvent, entry: FileEntry) => {
    // Dragging an unselected row drags just that row; dragging a selected one
    // takes the whole selection with it.
    const names = selected.has(entry.name) ? [...selected] : [entry.name];
    dragPaths.current = names.map((n) => joinPath(path, n));
    e.dataTransfer.effectAllowed = "move";
    e.dataTransfer.setData("text/plain", dragPaths.current.join("\n"));
  };

  const dropInto = (dest: string) => {
    setDropTarget(null);
    const paths = dragPaths.current;
    dragPaths.current = [];
    if (paths.length === 0) return;
    // Dropping onto the folder something already lives in is a no-op, not an
    // error worth a banner.
    if (paths.every((p) => parentOf(p) === dest)) return;
    startTransition(async () => {
      const res = await moveAction(root.id, paths, dest);
      if (!res.ok) {
        setError(res.error);
      } else {
        setError(null);
        setNotice(
          `Moved ${paths.length} item${paths.length === 1 ? "" : "s"} to ${dest || root.path}.`,
        );
      }
      const fresh = await listDirAction(root.id, path);
      if (fresh.ok) setListing(fresh.listing);
      // Keep the selection when the action failed so a retry acts on the
      // same items; clearing it made a retried Rename do nothing and a
      // retried Delete trash zero items.
      if (res.ok) setSelected(new Set());
    });
  };

  const allowDrop = (e: React.DragEvent, key: string) => {
    if (!root.writable || dragPaths.current.length === 0) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = "move";
    setDropTarget((t) => (t === key ? t : key));
  };

  /**
   * `dragleave` fires every time the pointer crosses into a CHILD element, so
   * clearing unconditionally makes the highlight strobe as you move across a
   * row. Only clear when the pointer has genuinely left the row's subtree.
   */
  const leaveDrop = (e: React.DragEvent, key: string) => {
    const next = e.relatedTarget as Node | null;
    if (next && e.currentTarget.contains(next)) return;
    setDropTarget((t) => (t === key ? null : t));
  };

  // --- upload / download / preview ------------------------------------------

  /**
   * Uploads run sequentially, not in parallel: the agent aborts an upload that
   * would push the filesystem under its free-space floor, and three concurrent
   * 2 GB streams would each pass that check on the way in and collectively
   * blow through it.
   */
  const uploadFiles = useCallback(
    async (files: File[]) => {
      if (files.length === 0) return;
      setError(null);
      setNotice(null);
      const controller = new AbortController();
      uploadAbort.current = controller;
      const failures: string[] = [];
      let done = 0;

      for (let i = 0; i < files.length; i++) {
        const file = files[i];
        if (file.size > maxUploadMb * 1024 * 1024) {
          failures.push(
            `${file.name}: larger than the ${maxUploadMb} MB upload limit (Settings > Panel > Uploads)`,
          );
          continue;
        }
        setUploading({ name: file.name, done: i, total: files.length, pct: 0 });
        const q = new URLSearchParams({ root: root.id, path, name: file.name });
        try {
          const res = await putFile(
            `/api/files/upload?${q.toString()}`,
            file,
            (fraction) =>
              setUploading({
                name: file.name,
                done: i,
                total: files.length,
                pct: Math.round(fraction * 100),
              }),
            controller.signal,
          );
          if (res.ok) done++;
          else failures.push(`${file.name}: ${errorText(res)}`);
        } catch (e: unknown) {
          if (e instanceof DOMException && e.name === "AbortError") {
            failures.push(`${file.name}: cancelled`);
            break; // don't start the remaining files
          }
          failures.push(`${file.name}: ${e instanceof Error ? e.message : String(e)}`);
        }
      }

      uploadAbort.current = null;
      setUploading(null);
      if (failures.length > 0) setError(failures.join("; "));
      if (done > 0) setNotice(`Uploaded ${done} file${done === 1 ? "" : "s"}.`);
      const fresh = await listDirAction(root.id, path);
      if (fresh.ok) setListing(fresh.listing);
    },
    [root.id, path, maxUploadMb],
  );

  const openPreview = useCallback(
    async (entry: FileEntry) => {
      setPreview(entry);
      setPreviewText(null);
      if (previewKind(entry.name) !== "text") return;
      if (entry.size > TEXT_PREVIEW_LIMIT) {
        setPreviewText(
          `This file is ${formatBytes(entry.size)} — too large to preview. Download it instead.`,
        );
        return;
      }
      try {
        const res = await fetch(downloadUrl(root.id, joinPath(path, entry.name)));
        setPreviewText(res.ok ? await res.text() : `Couldn't read this file (${res.status}).`);
      } catch (e: unknown) {
        setPreviewText(e instanceof Error ? e.message : String(e));
      }
    },
    [root.id, path],
  );

  // --- mutations ------------------------------------------------------------

  const runDialog = () => {
    const name = nameInput.trim();
    startTransition(async () => {
      let res;
      if (dialog === "newFolder") res = await createFolderAction(root.id, path, name);
      else if (dialog === "newFile") res = await createFileAction(root.id, path, name);
      else if (dialog === "rename") {
        const target = selectedEntries[0];
        if (!target) return;
        res = await renameAction(root.id, joinPath(path, target.name), name);
      }
      else if (dialog === "delete")
        res = await trashAction(
          root.id,
          selectedEntries.map((e) => joinPath(path, e.name)),
        );
      else return;

      if (!res.ok) {
        setError(res.error);
      } else {
        setError(null);
        setNotice(
          dialog === "delete"
            ? `Moved ${selectedEntries.length} item${
                selectedEntries.length === 1 ? "" : "s"
              } to Trash. Restorable for 30 days.`
            : null,
        );
        setDialog(null);
        setNameInput("");
      }
      const fresh = await listDirAction(root.id, path);
      if (fresh.ok) setListing(fresh.listing);
      // Keep the selection when the action failed so a retry acts on the
      // same items; clearing it made a retried Rename do nothing and a
      // retried Delete trash zero items.
      if (res.ok) setSelected(new Set());
    });
  };

  const openDialog = (kind: DialogKind) => {
    setError(null);
    setNotice(null);
    setNameInput(kind === "rename" ? selectedEntries[0]?.name ?? "" : "");
    setDialog(kind);
  };

  // --- render ---------------------------------------------------------------

  const segments = path ? path.split("/") : [];

  return (
    <div>
      <Link href="/files" className={pageStyles.backArrow} aria-label="Back to Files">
        ←
      </Link>

      <div className={pageStyles.head}>
        <div>
          <h1 className={pageStyles.title}>{root.label}</h1>
          <div className={pageStyles.subtitle}>
            <span className={pageStyles.code}>{listing.abs_path}</span>
            {!root.writable && " · browse-only"}
          </div>
        </div>
        <div className={pageStyles.actions}>
          <Link href="/files/trash" className={pageStyles.btnGhost}>
            Trash
          </Link>
        </div>
      </div>

      {error && <div className={pageStyles.error}>{error}</div>}
      {notice && <div className={pageStyles.notice}>{notice}</div>}

      <div className={styles.bar}>
        <div className={styles.crumbs}>
          <button
            type="button"
            className={[
              styles.crumb,
              segments.length === 0 ? styles.crumbCurrent : "",
              dropTarget === "crumb:" ? styles.crumbDrop : "",
            ]
              .join(" ")
              .trim()}
            onClick={() => load("", true)}
            disabled={pending || segments.length === 0}
            onDragOver={(e) => allowDrop(e, "crumb:")}
            onDragLeave={(e) => leaveDrop(e, "crumb:")}
            onDrop={() => dropInto("")}
          >
            {root.path}
          </button>
          {segments.map((seg, i) => {
            const target = segments.slice(0, i + 1).join("/");
            const isLast = i === segments.length - 1;
            return (
              <span key={target} className={styles.crumbs}>
                <span className={styles.crumbSep}>/</span>
                <button
                  type="button"
                  className={[
                    styles.crumb,
                    isLast ? styles.crumbCurrent : "",
                    dropTarget === `crumb:${target}` ? styles.crumbDrop : "",
                  ]
                    .join(" ")
                    .trim()}
                  onClick={() => load(target, true)}
                  disabled={pending || isLast}
                  onDragOver={(e) => allowDrop(e, `crumb:${target}`)}
                  onDragLeave={(e) => leaveDrop(e, `crumb:${target}`)}
                  onDrop={() => dropInto(target)}
                >
                  {seg}
                </button>
              </span>
            );
          })}
        </div>

        <div className={styles.toolbar}>
          {listing.writable && (
            <>
              <button
                type="button"
                className={pageStyles.btnGhost}
                onClick={() => openDialog("newFolder")}
                disabled={pending}
              >
                + Folder
              </button>
              <button
                type="button"
                className={pageStyles.btnGhost}
                onClick={() => openDialog("newFile")}
                disabled={pending}
              >
                + File
              </button>
              <button
                type="button"
                className={pageStyles.btnGhost}
                onClick={() => fileInput.current?.click()}
                disabled={pending || uploading !== null}
              >
                Upload
              </button>
              <input
                ref={fileInput}
                type="file"
                multiple
                hidden
                onChange={(ev) => {
                  const files = Array.from(ev.target.files ?? []);
                  // Reset first so re-picking the same file still fires change.
                  ev.target.value = "";
                  void uploadFiles(files);
                }}
              />
            </>
          )}
          {selectedEntries.length === 1 && !isDirLike(selectedEntries[0]) && (
            <a
              className={pageStyles.btnGhost}
              href={downloadUrl(root.id, joinPath(path, selectedEntries[0].name))}
              download={selectedEntries[0].name}
            >
              Download
            </a>
          )}
          <button
            type="button"
            className={pageStyles.btnGhost}
            onClick={() => openDialog("rename")}
            disabled={pending || selectedEntries.length !== 1 || !canActOnSelection}
          >
            Rename
          </button>
          <button
            type="button"
            className={pageStyles.btnDanger}
            onClick={() => openDialog("delete")}
            disabled={pending || !canActOnSelection}
          >
            Delete{selectedEntries.length > 1 ? ` (${selectedEntries.length})` : ""}
          </button>
          <button
            type="button"
            className={pageStyles.btnGhost}
            onClick={reload}
            disabled={pending}
          >
            {pending ? "..." : "Refresh"}
          </button>
        </div>
      </div>

      {root.writable && (
        <div className={styles.hint}>
          Drag rows onto a folder — or onto a breadcrumb above — to move them. Drop files
          from your computer anywhere on the list to upload. Deleted items go to Trash and
          are restorable for 30 days.
        </div>
      )}

      {uploading && (
        <div className={styles.uploadBar}>
          <div className={styles.uploadHead}>
            <span>
              Uploading <strong>{uploading.name}</strong>
              {uploading.total > 1 && ` (${uploading.done + 1} of ${uploading.total})`} —{" "}
              {uploading.pct}%
            </span>
            <button
              type="button"
              className={pageStyles.btnGhost}
              onClick={() => uploadAbort.current?.abort()}
            >
              Cancel
            </button>
          </div>
          <div className={styles.uploadTrack}>
            <div className={styles.uploadFill} style={{ width: `${uploading.pct}%` }} />
          </div>
          {uploading.pct === 100 && (
            <div className={styles.uploadNote}>
              Transferred — waiting for the server to finish writing it to disk.
            </div>
          )}
        </div>
      )}

      <div
        className={[
          styles.list,
          styles.dropRoot,
          fileDrag ? styles.dropRootActive : "",
        ]
          .join(" ")
          .trim()}
        // An OS-file drag advertises the "Files" type; an internal row drag
        // doesn't. Checking that is what keeps "move this row" and "upload
        // from my desktop" from fighting over the same drop.
        onDragOver={(e) => {
          if (!listing.writable || !e.dataTransfer.types.includes("Files")) return;
          e.preventDefault();
          e.dataTransfer.dropEffect = "copy";
          setFileDrag(true);
        }}
        onDragLeave={(e) => {
          const next = e.relatedTarget as Node | null;
          if (next && e.currentTarget.contains(next)) return;
          setFileDrag(false);
        }}
        onDrop={(e) => {
          if (!listing.writable || !e.dataTransfer.types.includes("Files")) return;
          e.preventDefault();
          setFileDrag(false);
          void uploadFiles(Array.from(e.dataTransfer.files));
        }}
      >
        <div className={[styles.row, styles.rowHead].join(" ")}>
          <span />
          <span>Name</span>
          <span>Size</span>
          <span className={styles.hideSmall}>Modified (UTC)</span>
          <span className={styles.hideSmall}>Owner / mode</span>
        </div>

        {path !== "" && (
          <div
            className={[styles.row, dropTarget === ".." ? styles.rowDrop : ""].join(" ").trim()}
            onDragOver={(e) => allowDrop(e, "..")}
            onDragLeave={(e) => leaveDrop(e, "..")}
            onDrop={() => dropInto(parentOf(path))}
          >
            <span />
            <span className={styles.cellName}>
              <span className={[styles.icon, styles.iconDir].join(" ")} aria-hidden="true">
                ↰
              </span>
              <button
                type="button"
                className={styles.nameBtn}
                onClick={() => load(parentOf(path), true)}
                disabled={pending}
              >
                .. (parent folder)
              </button>
            </span>
            <span />
            <span className={styles.hideSmall} />
            <span className={styles.hideSmall} />
          </div>
        )}

        {listing.entries.length === 0 && (
          <div className={styles.row}>
            <span />
            <span className={pageStyles.muted}>This folder is empty.</span>
            <span />
            <span className={styles.hideSmall} />
            <span className={styles.hideSmall} />
          </div>
        )}

        {listing.entries.map((e) => {
          const dir = isDirLike(e);
          const isSelected = selected.has(e.name);
          const isDropTarget = dropTarget === e.name;
          return (
            <div
              key={e.name}
              className={[
                styles.row,
                isSelected ? styles.rowSelected : "",
                isDropTarget ? styles.rowDrop : "",
              ]
                .join(" ")
                .trim()}
              draggable={root.writable && !e.locked}
              onDragStart={(ev) => onDragStart(ev, e)}
              onDragEnd={() => {
                dragPaths.current = [];
                setDropTarget(null);
              }}
              onDragOver={(ev) => {
                // Only folders accept a drop, and never the folder being dragged.
                if (!dir || dragPaths.current.includes(joinPath(path, e.name))) return;
                allowDrop(ev, e.name);
              }}
              onDragLeave={(ev) => leaveDrop(ev, e.name)}
              onDrop={() => {
                if (!dir) return;
                dropInto(joinPath(path, e.name));
              }}
              onClick={() => toggle(e.name)}
            >
              <input
                type="checkbox"
                checked={isSelected}
                onChange={() => toggle(e.name)}
                onClick={(ev) => ev.stopPropagation()}
                aria-label={`Select ${e.name}`}
                disabled={e.locked}
              />
              <span className={styles.cellName}>
                <span
                  className={[styles.icon, dir ? styles.iconDir : ""].join(" ").trim()}
                  aria-hidden="true"
                >
                  {dir ? "▸" : e.kind === "symlink" ? "↳" : "·"}
                </span>
                {dir ? (
                  <button
                    type="button"
                    className={styles.nameBtn}
                    onClick={(ev) => {
                      ev.stopPropagation();
                      load(joinPath(path, e.name), true);
                    }}
                    disabled={pending}
                    title={e.name}
                  >
                    {e.name}
                  </button>
                ) : e.kind === "file" && previewKind(e.name) ? (
                  <button
                    type="button"
                    className={styles.nameBtn}
                    onClick={(ev) => {
                      ev.stopPropagation();
                      void openPreview(e);
                    }}
                    title={`Preview ${e.name}`}
                  >
                    {e.name}
                  </button>
                ) : (
                  <span className={styles.nameStatic} title={e.name}>
                    {e.name}
                  </span>
                )}
                {e.link_target && (
                  <span className={styles.linkTarget} title={e.link_target}>
                    → {e.link_target}
                  </span>
                )}
                {e.locked && (
                  <span className={styles.lockIcon} title="Protected system path — read-only">
                    🔒
                  </span>
                )}
              </span>
              <span className={styles.meta}>{dir ? "-" : formatBytes(e.size)}</span>
              <span className={[styles.meta, styles.hideSmall].join(" ")}>
                {formatTs(e.modified)}
              </span>
              <span className={[styles.meta, styles.hideSmall].join(" ")}>
                {e.owner}:{e.group} {e.mode}
              </span>
            </div>
          );
        })}
      </div>

      <div className={styles.footer}>
        <span>
          {listing.entries.length} item{listing.entries.length === 1 ? "" : "s"}
          {selected.size > 0 && ` · ${selected.size} selected`}
        </span>
        {listing.truncated && (
          <span className={pageStyles.muted}>
            Only the first 5000 entries are shown.
          </span>
        )}
      </div>

      <ConfirmDialog
        open={dialog === "newFolder" || dialog === "newFile" || dialog === "rename"}
        title={
          dialog === "newFolder"
            ? "New folder"
            : dialog === "newFile"
              ? "New file"
              : `Rename ${selectedEntries[0]?.name ?? ""}`
        }
        confirmLabel={dialog === "rename" ? "Rename" : "Create"}
        pending={pending}
        onConfirm={runDialog}
        onCancel={() => {
          setDialog(null);
          setNameInput("");
        }}
      >
        <div className={pageStyles.field}>
          <label htmlFor="files-name-input">
            {dialog === "rename" ? "New name" : "Name"}
          </label>
          <input
            id="files-name-input"
            ref={nameRef}
            className={pageStyles.input}
            value={nameInput}
            onChange={(ev) => setNameInput(ev.target.value)}
            onKeyDown={(ev) => {
              if (ev.key === "Enter" && nameInput.trim()) runDialog();
            }}
          />
        </div>
        <div className={pageStyles.muted} style={{ fontSize: 12, marginTop: 8 }}>
          In <span className={pageStyles.code}>{listing.abs_path}</span>
        </div>
      </ConfirmDialog>

      <ConfirmDialog
        open={dialog === "delete"}
        title={`Move ${selectedEntries.length} item${
          selectedEntries.length === 1 ? "" : "s"
        } to Trash?`}
        confirmLabel="Move to Trash"
        tone="danger"
        pending={pending}
        onConfirm={runDialog}
        onCancel={() => setDialog(null)}
      >
        <div style={{ fontSize: 13 }}>
          These are moved to the panel&apos;s trash, not erased. You can restore them
          from the Trash page for 30 days, after which they&apos;re purged automatically.
        </div>
        <ul className={styles.dialogList}>
          {selectedEntries.map((e) => (
            <li key={e.name}>{e.name}</li>
          ))}
        </ul>
      </ConfirmDialog>

      <ConfirmDialog
        open={preview !== null}
        title={preview?.name ?? ""}
        confirmLabel="Download"
        cancelLabel="Close"
        onConfirm={() => {
          if (!preview) return;
          window.location.href = downloadUrl(root.id, joinPath(path, preview.name));
        }}
        onCancel={() => {
          setPreview(null);
          setPreviewText(null);
        }}
      >
        {preview && (
          <div className={styles.preview}>
            {previewKind(preview.name) === "image" && (
              // eslint-disable-next-line @next/next/no-img-element -- served
              // through the agent proxy, not an optimisable static asset
              <img
                className={styles.previewImage}
                src={downloadUrl(root.id, joinPath(path, preview.name), true)}
                alt={preview.name}
              />
            )}
            {previewKind(preview.name) === "pdf" && (
              <iframe
                className={styles.previewFrame}
                src={downloadUrl(root.id, joinPath(path, preview.name), true)}
                title={preview.name}
              />
            )}
            {previewKind(preview.name) === "text" && (
              <pre className={pageStyles.log}>{previewText ?? "Loading…"}</pre>
            )}
            <div className={styles.previewMeta}>
              {formatBytes(preview.size)} · {preview.owner}:{preview.group} {preview.mode} ·
              modified {formatTs(preview.modified)}
            </div>
          </div>
        )}
      </ConfirmDialog>
    </div>
  );
}
