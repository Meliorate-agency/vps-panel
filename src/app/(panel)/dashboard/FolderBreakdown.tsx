"use client";

import { useEffect, useState, useTransition } from "react";
import type { FolderKind, FolderSize, FolderSizes } from "@/lib/agent";
import StatusBadge from "@/components/StatusBadge/StatusBadge";
import Table from "@/components/Table/Table";
import styles from "@/components/page.module.css";
import grid from "./grid.module.css";
import { folderSizesAction } from "./folders";

/** Folders smaller than this are left out of the list (still counted). */
const MIN_SHOWN = 1024 * 1024;

const KIND_BADGE: Partial<Record<FolderKind, { text: string; title: string }>> = {
  cache: { text: "cache", title: "Downloaded again when needed: safe to clear." },
  leftover: {
    text: "leftover",
    title: "Left behind by a build or an update: usually safe to delete.",
  },
};

/**
 * "What uses the space on /": the key folders' sizes, measured by the agent
 * with du. Loaded after the page renders, because a first measurement reads
 * the whole disk and can take a minute; the agent then serves it for 10
 * minutes.
 */
export default function FolderBreakdown() {
  const [sizes, setSizes] = useState<FolderSizes | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pending, start] = useTransition();

  const load = (refresh: boolean) =>
    start(async () => {
      const res = await folderSizesAction(refresh).catch((e: unknown) => ({
        ok: false as const,
        error: e instanceof Error ? e.message : String(e),
      }));
      if (res.ok) {
        setSizes(res.sizes);
        setError(null);
      } else {
        setError(res.error);
      }
    });

  useEffect(() => {
    load(false);
    // Once, on mount.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div className={styles.section}>
      <div className={grid.sectionHead}>
        <h2 className={styles.sectionTitle}>What uses the space on /</h2>
        <div className={grid.sectionMeta}>
          {sizes && !pending && (
            <span className={styles.muted}>
              measured {ago(sizes.measured_at)}
              {sizes.took_ms >= 1000 && ` in ${Math.round(sizes.took_ms / 1000)} s`}
            </span>
          )}
          {sizes && (
            <button
              type="button"
              className={styles.btnGhost}
              onClick={() => load(true)}
              disabled={pending}
            >
              {pending ? "Measuring..." : "Measure again"}
            </button>
          )}
        </div>
      </div>
      {error && <div className={styles.error}>Could not measure folders: {error}</div>}
      {!sizes && !error && (
        <div className={styles.muted} style={{ fontSize: 13 }}>
          Measuring folder sizes... the first time reads the whole disk, so it can take a
          minute.
        </div>
      )}
      {sizes && (
        <Table
          layout="fixed"
          scrollX
          columns={[
            { key: "folder", label: "Folder" },
            { key: "size", label: "Size", width: "200px" },
            { key: "tag", label: "", width: "90px" },
          ]}
          rows={buildRows(sizes).map((r) => ({
            folder: (
              <div
                className={grid.folderCell}
                style={r.child ? { paddingLeft: 22 } : undefined}
              >
                <span className={grid.folderPath}>
                  {r.child && <span className={grid.folderConnector}>└ </span>}
                  {r.path ? <span className={styles.code}>{r.path}</span> : <strong>{r.label}</strong>}
                </span>
                {r.path && <span className={grid.folderLabel}>{r.label}</span>}
              </div>
            ),
            size: renderSize(r.bytes, sizes.total_bytes),
            tag: r.kind && KIND_BADGE[r.kind] ? (
              <span title={KIND_BADGE[r.kind]!.title}>
                <StatusBadge text={KIND_BADGE[r.kind]!.text} tone="warn" />
              </span>
            ) : (
              // Not null: the Table shows "-" for a missing cell.
              ""
            ),
          }))}
        />
      )}
    </div>
  );
}

type Row = {
  path: string | null;
  label: string;
  kind: FolderKind | null;
  bytes: number | null;
  child: boolean;
};

/** Biggest first, each folder followed by the listed folders inside it, then
 *  what no listed folder covers and the filesystem's reserve, so the rows add
 *  up to the Used figure of the Disks table. */
function buildRows(s: FolderSizes): Row[] {
  const bySize = (a: FolderSize, b: FolderSize) => (b.bytes ?? -1) - (a.bytes ?? -1);
  const shown = (f: FolderSize) => f.bytes === null || f.bytes >= MIN_SHOWN;
  const top = s.folders.filter((f) => f.parent === null).sort(bySize);
  const rows: Row[] = [];
  for (const f of top.filter(shown)) {
    rows.push({ ...f, child: false });
    for (const c of s.folders.filter((c) => c.parent === f.path && shown(c)).sort(bySize)) {
      rows.push({ ...c, child: true });
    }
  }
  const listed = top.reduce((sum, f) => sum + (f.bytes ?? 0), 0);
  const rest = s.used_bytes - listed;
  if (rest >= MIN_SHOWN) {
    rows.push({ path: null, label: "Everything else", kind: null, bytes: rest, child: false });
  }
  const reserved = s.total_bytes - s.used_bytes - s.avail_bytes;
  if (reserved >= MIN_SHOWN) {
    rows.push({
      path: null,
      label: "Reserved for root by the filesystem",
      kind: null,
      bytes: reserved,
      child: false,
    });
  }
  return rows;
}

function renderSize(bytes: number | null, total: number) {
  if (bytes === null) {
    return (
      <span className={styles.muted} title="du did not finish in time">
        not measured
      </span>
    );
  }
  const pct = total > 0 ? (bytes / total) * 100 : 0;
  return (
    <div className={grid.folderSize}>
      <span>
        {formatBytes(bytes)}{" "}
        <span className={styles.muted}>({pct < 1 ? "<1" : Math.round(pct)}%)</span>
      </span>
      <div className={grid.folderBar}>
        <div style={{ width: `${Math.min(100, pct)}%` }} />
      </div>
    </div>
  );
}

function formatBytes(bytes: number): string {
  const gb = bytes / 1024 ** 3;
  if (gb >= 1) return `${gb.toFixed(1)} GB`;
  return `${Math.round(bytes / 1024 ** 2)} MB`;
}

function ago(iso: string): string {
  const mins = Math.floor((Date.now() - new Date(iso).getTime()) / 60000);
  if (!Number.isFinite(mins) || mins < 1) return "just now";
  if (mins < 60) return `${mins} min ago`;
  return `${Math.floor(mins / 60)} h ago`;
}
