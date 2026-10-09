"use client";

import { useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import type { TrashRow } from "@/lib/agent";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";
import Table from "@/components/Table/Table";
import pageStyles from "@/components/page.module.css";
import {
  emptyTrashAction,
  purgeTrashAction,
  restoreAction,
} from "@/server-actions/files";

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

function formatTs(iso: string): string {
  return `${iso.slice(0, 10)} ${iso.slice(11, 16)}`;
}

/**
 * Restore puts an item back at its original absolute path and fails loudly if
 * something already occupies it — a restore must never silently overwrite the
 * thing that replaced it.
 */
export default function TrashTable({ rows }: { rows: TrashRow[] }) {
  const router = useRouter();
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [purging, setPurging] = useState<TrashRow | null>(null);
  const [emptying, setEmptying] = useState(false);
  const [pending, startTransition] = useTransition();

  const run = (fn: () => Promise<{ ok: true } | { ok: false; error: string }>, ok: string) =>
    startTransition(async () => {
      const res = await fn();
      if (res.ok) {
        setError(null);
        setNotice(ok);
        setPurging(null);
        setEmptying(false);
        router.refresh();
      } else {
        setError(res.error);
      }
    });

  return (
    <div>
      {error && <div className={pageStyles.error}>{error}</div>}
      {notice && <div className={pageStyles.notice}>{notice}</div>}

      {rows.length > 0 && (
        <div className={pageStyles.actions} style={{ marginBottom: 12 }}>
          <button
            type="button"
            className={pageStyles.btnDanger}
            onClick={() => setEmptying(true)}
            disabled={pending}
          >
            Empty trash ({rows.length})
          </button>
        </div>
      )}

      <Table
        columns={[
          { key: "name", label: "Name" },
          { key: "from", label: "Deleted from" },
          { key: "size", label: "Size", width: "90px" },
          { key: "when", label: "Deleted", width: "140px" },
          { key: "expires", label: "Purges in", width: "100px" },
          { key: "actions", label: "", width: "170px" },
        ]}
        rowKey={(_r, i) => rows[i]?.id ?? i}
        rows={rows.map((r) => ({
          name: (
            <span>
              {r.kind === "dir" ? "▸ " : ""}
              {r.name}
            </span>
          ),
          from: <span className={pageStyles.code}>{r.original_path}</span>,
          size: r.kind === "dir" ? "-" : formatBytes(r.size),
          when: <span className={pageStyles.muted}>{formatTs(r.trashed_at)}</span>,
          expires: (
            <span className={r.days_left <= 3 ? pageStyles.error : pageStyles.muted}>
              {r.days_left} day{r.days_left === 1 ? "" : "s"}
            </span>
          ),
          actions: (
            <span style={{ display: "flex", gap: 6 }}>
              <button
                type="button"
                className={pageStyles.btnGhost}
                onClick={() => run(() => restoreAction(r.id), `Restored to ${r.original_path}.`)}
                disabled={pending}
              >
                Restore
              </button>
              <button
                type="button"
                className={pageStyles.btnDanger}
                onClick={() => setPurging(r)}
                disabled={pending}
              >
                Delete
              </button>
            </span>
          ),
        }))}
        empty="Trash is empty."
        scrollX
      />

      <ConfirmDialog
        open={purging !== null}
        title={`Permanently delete ${purging?.name ?? ""}?`}
        confirmLabel="Delete forever"
        tone="danger"
        pending={pending}
        onConfirm={() =>
          purging &&
          run(() => purgeTrashAction(purging.id), `Permanently deleted ${purging.name}.`)
        }
        onCancel={() => setPurging(null)}
      >
        <div style={{ fontSize: 13 }}>
          This erases it from disk. It came from{" "}
          <span className={pageStyles.code}>{purging?.original_path}</span> and cannot be
          recovered afterwards.
        </div>
      </ConfirmDialog>

      <ConfirmDialog
        open={emptying}
        title={`Permanently delete all ${rows.length} items?`}
        confirmLabel="Empty trash"
        tone="danger"
        pending={pending}
        onConfirm={() => run(() => emptyTrashAction(), "Trash emptied.")}
        onCancel={() => setEmptying(false)}
      >
        <div style={{ fontSize: 13 }}>
          Everything in the trash is erased from disk, including items still inside their
          30-day window. This cannot be undone.
        </div>
      </ConfirmDialog>
    </div>
  );
}
