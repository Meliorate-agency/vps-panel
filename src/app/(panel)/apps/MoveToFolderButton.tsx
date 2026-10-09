"use client";

import { useEffect, useLayoutEffect, useRef, useState, useTransition } from "react";
import { createPortal } from "react-dom";
import { useRouter } from "next/navigation";
import styles from "@/components/page.module.css";

type Props = {
  appName: string;
  currentFolderId: number | null;
  folders: { id: number; name: string }[];
  /** Bound action that takes (folderId | null) and updates apps.folder_id. */
  moveAction: (folderId: number | null) => Promise<void>;
};

/**
 * Compact per-row "move to folder" picker. Renders as a tiny folder-icon
 * button; clicking opens a popover with a radio-style list of every
 * folder (plus a "No folder" item). Picking an item calls the bound
 * server action and refreshes the route so the row jumps into its new
 * bucket.
 *
 * Intentionally small so it fits alongside the existing Open link in the
 * actions cell without bloating the row height.
 */
export default function MoveToFolderButton({
  appName,
  currentFolderId,
  folders,
  moveAction,
}: Props) {
  const router = useRouter();
  const [open, setOpen] = useState(false);
  const [pending, startTransition] = useTransition();
  const wrapRef = useRef<HTMLDivElement>(null);
  const popRef = useRef<HTMLDivElement>(null);
  // See GitStatusBadge for the rationale on portal + position:fixed —
  // Table.wrap's backdrop-filter creates a containing block that traps
  // even fixed-positioned descendants, and clipping via overflow:auto
  // hides the popover when scrolling inside the card.
  const [pos, setPos] = useState<{ top: number; right: number } | null>(null);

  useLayoutEffect(() => {
    if (!open || !wrapRef.current) return;
    function place() {
      const r = wrapRef.current!.getBoundingClientRect();
      setPos({
        top: r.bottom + 6,
        right: Math.max(8, window.innerWidth - r.right),
      });
    }
    place();
    window.addEventListener("resize", place);
    return () => window.removeEventListener("resize", place);
  }, [open]);

  // Close on outside click (trigger or portaled body) + on any scroll.
  useEffect(() => {
    if (!open) return;
    function onClick(e: MouseEvent) {
      const target = e.target as Node;
      if (wrapRef.current?.contains(target)) return;
      if (popRef.current?.contains(target)) return;
      setOpen(false);
    }
    function onScroll() {
      setOpen(false);
    }
    document.addEventListener("mousedown", onClick);
    window.addEventListener("scroll", onScroll, true);
    return () => {
      document.removeEventListener("mousedown", onClick);
      window.removeEventListener("scroll", onScroll, true);
    };
  }, [open]);

  const pick = (folderId: number | null) => {
    startTransition(async () => {
      try {
        await moveAction(folderId);
        router.refresh();
        setOpen(false);
      } catch {
        // Server-side audit captures the error; UI just keeps the popover
        // open so the operator can try a different bucket.
      }
    });
  };

  return (
    <div style={{ position: "relative", display: "inline-block" }} ref={wrapRef}>
      <button
        type="button"
        className={styles.btnGhost}
        onClick={() => setOpen((v) => !v)}
        disabled={pending}
        title={
          currentFolderId === null
            ? "Move into a folder"
            : "Change folder / remove from folder"
        }
        aria-label={`Folder of ${appName}`}
        style={{ padding: "4px 8px", fontSize: 12 }}
      >
        <svg
          width="14"
          height="14"
          viewBox="0 0 16 16"
          fill="none"
          aria-hidden="true"
          style={{ verticalAlign: "-2px" }}
        >
          <path
            d="M2 4.5A1.5 1.5 0 0 1 3.5 3h2.69a1.5 1.5 0 0 1 1.06.44l.78.78a1.5 1.5 0 0 0 1.06.44H12.5A1.5 1.5 0 0 1 14 6.16V11.5A1.5 1.5 0 0 1 12.5 13h-9A1.5 1.5 0 0 1 2 11.5v-7Z"
            stroke="currentColor"
            strokeWidth="1.4"
          />
        </svg>
      </button>

      {open && pos && typeof document !== "undefined" && createPortal(
        <div
          ref={popRef}
          role="menu"
          style={{
            position: "fixed",
            top: pos.top,
            right: pos.right,
            minWidth: 180,
            background: "var(--bg-elev-solid)",
            border: "1px solid var(--border-strong)",
            borderRadius: "var(--radius)",
            padding: 6,
            boxShadow: "0 12px 32px rgba(0, 0, 0, 0.55)",
            zIndex: 9999,
            fontSize: 12,
          }}
        >
          <div
            style={{
              padding: "4px 8px",
              color: "var(--text-dim)",
              fontSize: 11,
              textTransform: "uppercase",
              letterSpacing: "0.06em",
            }}
          >
            Move {appName}
          </div>
          <FolderItem
            label="(no folder)"
            active={currentFolderId === null}
            pending={pending}
            onClick={() => pick(null)}
          />
          {folders.map((f) => (
            <FolderItem
              key={f.id}
              label={f.name}
              active={currentFolderId === f.id}
              pending={pending}
              onClick={() => pick(f.id)}
            />
          ))}
          {folders.length === 0 && (
            <div
              style={{
                padding: "6px 8px",
                color: "var(--text-dim)",
                fontSize: 11,
              }}
            >
              No folders yet. Use <strong>+ New folder</strong> on /apps.
            </div>
          )}
        </div>,
        document.body,
      )}
    </div>
  );
}

function FolderItem({
  label,
  active,
  pending,
  onClick,
}: {
  label: string;
  active: boolean;
  pending: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      role="menuitem"
      onClick={onClick}
      disabled={pending}
      style={{
        display: "flex",
        alignItems: "center",
        gap: 8,
        width: "100%",
        padding: "6px 8px",
        background: active ? "var(--bg-elev-2)" : "transparent",
        border: 0,
        color: active ? "var(--accent-glow)" : "var(--text)",
        borderRadius: 4,
        cursor: pending ? "wait" : "pointer",
        textAlign: "left",
        fontSize: 12,
        fontWeight: active ? 600 : 400,
      }}
      onMouseEnter={(e) => {
        if (!active) e.currentTarget.style.background = "var(--bg-elev-2)";
      }}
      onMouseLeave={(e) => {
        if (!active) e.currentTarget.style.background = "transparent";
      }}
    >
      <span style={{ width: 14, fontSize: 11, color: "var(--text-dim)" }}>
        {active ? "●" : "○"}
      </span>
      {label}
    </button>
  );
}
