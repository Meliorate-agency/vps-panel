"use client";

import { useActionState, useEffect, useRef, useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import Table from "@/components/Table/Table";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";
import styles from "@/components/page.module.css";
import sectionStyles from "./AppFolderSection.module.css";

type Result = { ok: true } | { ok: false; error: string };

type Props = {
  /** Null for the virtual "Ungrouped" / "External" buckets — those have
   *  no folder row and no rename / delete / collapse-persist controls. */
  folder: { id: number; name: string; collapsed: number } | null;
  /** Override the bucket label when folder is null (e.g. "Ungrouped",
   *  "External apps"). Ignored when folder is non-null. */
  fallbackLabel?: string;
  /** Pre-built row data for the Table component. The page server
   *  component builds these to share the cell-rendering logic across
   *  buckets. */
  rows: Record<string, React.ReactNode>[];
  /** Table column metadata. */
  columns: { key: string; label: string; width?: string }[];
  /** Shown when this section has no apps. */
  empty: string;
  /** Bound action: rename this folder. Null when folder is null. */
  renameAction?: (prev: Result | null, fd: FormData) => Promise<Result>;
  /** Bound action: delete this folder. Null when folder is null. */
  deleteAction?: () => Promise<void>;
  /** Bound action: persist collapsed flag. Null when folder is null. */
  toggleCollapsedAction?: () => Promise<void>;
};

/**
 * Collapsible section for one bucket of apps (a named folder, or the
 * virtual "Ungrouped" / "External" buckets). For named folders the header
 * exposes inline rename + a styled delete confirmation dialog. Collapse
 * state is persisted via toggleCollapsedAction so it survives page
 * refresh.
 *
 * Virtual buckets (folder=null) only render a static header without the
 * mutation controls — collapse is local-only client state in that case.
 */
export default function AppFolderSection({
  folder,
  fallbackLabel,
  rows,
  columns,
  empty,
  renameAction,
  deleteAction,
  toggleCollapsedAction,
}: Props) {
  const router = useRouter();
  const isVirtual = folder === null;
  const label = folder?.name ?? fallbackLabel ?? "Ungrouped";

  // Persisted collapse for named folders; local-only for virtual buckets.
  const persistedCollapsed = folder ? folder.collapsed === 1 : false;
  const [localCollapsed, setLocalCollapsed] = useState(persistedCollapsed);
  // Keep in sync if a server revalidate changes the persisted value (e.g.
  // another tab toggled it).
  useEffect(() => {
    setLocalCollapsed(persistedCollapsed);
  }, [persistedCollapsed]);

  const onToggle = () => {
    setLocalCollapsed((v) => !v);
    if (toggleCollapsedAction) {
      // Fire-and-forget. revalidatePath inside the action will eventually
      // re-render with the authoritative value.
      toggleCollapsedAction().catch(() => {
        // Roll back the optimistic flip if the server rejects.
        setLocalCollapsed(persistedCollapsed);
      });
    }
  };

  // ─── Rename inline editor ─────────────────────────────────────────
  const [editing, setEditing] = useState(false);
  const [name, setName] = useState(label);
  useEffect(() => setName(label), [label]);
  const formRef = useRef<HTMLFormElement>(null);
  const [renameState, renameFormAction, renamePending] = useActionState<
    Result | null,
    FormData
  >(
    renameAction ??
      (async () => ({ ok: false, error: "rename not supported" })),
    null,
  );
  useEffect(() => {
    if (renameState?.ok === true) {
      setEditing(false);
    }
  }, [renameState]);

  // ─── Delete dialog ────────────────────────────────────────────────
  const [deleteOpen, setDeleteOpen] = useState(false);
  const [deletePending, startDelete] = useTransition();
  const [deleteError, setDeleteError] = useState<string | null>(null);
  const onConfirmDelete = () => {
    if (!deleteAction) return;
    setDeleteError(null);
    startDelete(async () => {
      try {
        await deleteAction();
        router.refresh();
        setDeleteOpen(false);
      } catch (e: unknown) {
        setDeleteError(e instanceof Error ? e.message : String(e));
      }
    });
  };

  return (
    <div className={sectionStyles.wrap}>
      <div className={sectionStyles.header}>
        <button
          type="button"
          className={sectionStyles.toggle}
          onClick={onToggle}
          aria-expanded={!localCollapsed}
          aria-label={localCollapsed ? "Expand" : "Collapse"}
          title={localCollapsed ? "Expand folder" : "Collapse folder"}
        >
          <svg
            width="12"
            height="12"
            viewBox="0 0 12 12"
            fill="none"
            aria-hidden="true"
            style={{
              transform: localCollapsed ? "rotate(-90deg)" : "rotate(0deg)",
              transition: "transform 0.15s",
            }}
          >
            <path
              d="M2 4 L6 8 L10 4"
              stroke="currentColor"
              strokeWidth="1.8"
              strokeLinecap="round"
              strokeLinejoin="round"
            />
          </svg>
        </button>

        {editing && folder ? (
          <form
            ref={formRef}
            action={renameFormAction}
            style={{ display: "flex", gap: 6, alignItems: "center", flex: 1 }}
          >
            <input
              name="name"
              className={styles.input}
              defaultValue={name}
              autoFocus
              maxLength={60}
              style={{ height: 28, padding: "2px 8px", fontSize: 13 }}
              onKeyDown={(e) => {
                if (e.key === "Escape") {
                  e.preventDefault();
                  setEditing(false);
                }
              }}
            />
            <button
              type="submit"
              className={styles.btn}
              disabled={renamePending}
              style={{ height: 28, padding: "0 12px", fontSize: 12 }}
            >
              {renamePending ? "..." : "Save"}
            </button>
            <button
              type="button"
              className={styles.btnGhost}
              onClick={() => setEditing(false)}
              disabled={renamePending}
              style={{ height: 28, padding: "0 10px", fontSize: 12 }}
            >
              Cancel
            </button>
            {renameState?.ok === false && (
              <span className={styles.error} style={{ fontSize: 12 }}>
                {renameState.error}
              </span>
            )}
          </form>
        ) : (
          <>
            <button
              type="button"
              className={sectionStyles.title}
              onClick={onToggle}
            >
              {label}
            </button>
            <span className={sectionStyles.count}>
              {rows.length} {rows.length === 1 ? "app" : "apps"}
            </span>
            {folder && (
              <div className={sectionStyles.actions}>
                <button
                  type="button"
                  className={styles.btnGhost}
                  style={{ fontSize: 11, padding: "3px 9px" }}
                  onClick={() => {
                    setName(label);
                    setEditing(true);
                  }}
                  title="Rename folder"
                >
                  Rename
                </button>
                <button
                  type="button"
                  className={styles.btnDanger}
                  style={{ fontSize: 11, padding: "3px 9px" }}
                  onClick={() => {
                    setDeleteError(null);
                    setDeleteOpen(true);
                  }}
                  title="Delete folder (apps inside go back to Ungrouped)"
                >
                  Delete
                </button>
              </div>
            )}
          </>
        )}
      </div>

      {!localCollapsed && (
        <div className={sectionStyles.body}>
          <Table scrollX columns={columns} rows={rows} empty={empty} />
        </div>
      )}

      {folder && (
        <ConfirmDialog
          open={deleteOpen}
          title={`Delete folder "${label}"?`}
          confirmLabel={deletePending ? "Deleting..." : "Delete folder"}
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
            Removes the folder from <span className={styles.code}>panel.db</span>.
            Apps currently inside this folder go back to <strong>Ungrouped</strong>{" "}
            — none of them are deleted, no systemd units are touched, no code on
            disk changes.
          </p>
          <div className={styles.muted} style={{ fontSize: 12, marginTop: 8 }}>
            <strong>{rows.length}</strong>{" "}
            {rows.length === 1 ? "app" : "apps"} currently in this folder.
          </div>
          {deleteError && (
            <div className={styles.error} style={{ marginTop: 10 }}>
              {deleteError}
            </div>
          )}
        </ConfirmDialog>
      )}
    </div>
  );
}
