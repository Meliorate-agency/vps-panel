"use client";

import {
  useActionState,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  useTransition,
} from "react";
import { createPortal } from "react-dom";
import styles from "@/components/page.module.css";
import localStyles from "./editRecord.module.css";

type Result = { ok: true } | { ok: false; error: string };

type RecordShape = {
  id: number;
  name: string;
  type: string;
  content: string;
  ttl: number;
  prio: number;
};

// Convert the full FQDN stored in PowerDNS into something nicer to edit:
//   "example.com"              (apex of example.com) → "@"
//   "mail.example.com"         (subdomain of example.com) → "mail"
//   anything else weird        → the raw value (operator can rewrite freely)
// The agent re-normalises whatever the operator types on submit, so worst
// case the round-trip is a no-op.
// Types a record can be edited INTO: the agent's list minus SOA, which only
// the zone itself creates.
const EDITABLE_TYPES = ["A", "AAAA", "CNAME", "MX", "TXT", "NS", "SRV", "CAA", "PTR", "TLSA"];

function labelForEditing(name: string, zone: string): string {
  const n = name.toLowerCase().replace(/\.$/, "");
  const z = zone.toLowerCase().replace(/\.$/, "");
  if (n === z) return "@";
  if (n.endsWith(`.${z}`)) return n.slice(0, n.length - z.length - 1);
  return name;
}

export default function EditRecordRow({
  record,
  zone,
  updateAction,
  deleteAction,
}: {
  record: RecordShape;
  zone: string;
  updateAction: (prev: Result | null, fd: FormData) => Promise<Result>;
  deleteAction: () => Promise<void>;
}) {
  const [state, formAction, pending] = useActionState<Result | null, FormData>(
    updateAction,
    null,
  );
  const [deletePending, startDelete] = useTransition();
  const isSoa = record.type.toUpperCase() === "SOA";
  const typeOptions = isSoa
    ? ["SOA"]
    : EDITABLE_TYPES.includes(record.type)
      ? EDITABLE_TYPES
      : [...EDITABLE_TYPES, record.type];
  const [open, setOpen] = useState(false);
  const [isMobile, setIsMobile] = useState(false);
  const [pos, setPos] = useState<{
    top?: number;
    bottom?: number;
    right: number;
    maxHeight: number;
  } | null>(null);
  const [, setResized] = useState(0);
  const wrapRef = useRef<HTMLDivElement>(null);
  const popRef = useRef<HTMLDivElement>(null);

  // Phone vs desktop. Phone = centered modal so the popup is reachable
  // when the row is inside a horizontally-scrolled table; the trigger
  // could be scrolled off-screen and anchoring would put the popup
  // anywhere. Desktop = small popover anchored under the Edit button.
  useEffect(() => {
    if (typeof window === "undefined" || !window.matchMedia) return;
    const mql = window.matchMedia("(max-width: 768px)");
    setIsMobile(mql.matches);
    const onChange = (e: MediaQueryListEvent) => setIsMobile(e.matches);
    mql.addEventListener("change", onChange);
    return () => mql.removeEventListener("change", onChange);
  }, []);

  // Anchor the desktop popover to the trigger row's bottom-right corner.
  // We portal the popover to <body> and use position:fixed so we get
  // viewport coords, not parent-relative — Table.wrap's backdrop-filter
  // makes it a containing block that traps even fixed descendants if
  // they're mounted inside, AND scrollX adds overflow:auto which would
  // clip an absolute popover anyway.
  //
  // It opens above the row instead when it would run past the bottom of the
  // window and there is more room above, and it never grows past the window
  // edge on its side: when neither side has room for all of it, it scrolls
  // inside. Scrolling the page closes it, so otherwise Save on a lower row
  // was out of reach. Runs after every render while open, so an error line
  // re-places it; the state only changes when the placement does. Resizes
  // re-render to re-place it too.
  useLayoutEffect(() => {
    if (!open || isMobile || !wrapRef.current) return;
    const r = wrapRef.current.getBoundingClientRect();
    // scrollHeight: the full height even while it is capped (+2 border).
    const height = (popRef.current?.scrollHeight ?? 0) + 2;
    const right = Math.max(8, Math.round(window.innerWidth - r.right));
    const roomBelow = Math.floor(window.innerHeight - r.bottom - 6 - 8);
    const roomAbove = Math.floor(r.top - 6 - 8);
    const next =
      height > roomBelow && roomAbove > roomBelow
        ? { bottom: Math.round(window.innerHeight - r.top + 6), right, maxHeight: roomAbove }
        : { top: Math.round(r.bottom + 6), right, maxHeight: roomBelow };
    setPos((p) =>
      p &&
      p.top === next.top &&
      p.bottom === next.bottom &&
      p.right === next.right &&
      p.maxHeight === next.maxHeight
        ? p
        : next,
    );
  });
  useEffect(() => {
    if (!open || isMobile) return;
    const onResize = () => setResized((n) => n + 1);
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, [open, isMobile]);

  // Close on outside click + ESC. Desktop additionally closes on any
  // scroll because the anchored popover can't reliably track its trigger
  // through the backdrop-filter container. Mobile is a modal so body
  // scroll is locked instead.
  useEffect(() => {
    if (!open) return;
    function onClick(e: MouseEvent) {
      const target = e.target as Node;
      if (wrapRef.current?.contains(target)) return;
      if (popRef.current?.contains(target)) return;
      setOpen(false);
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") setOpen(false);
    }
    document.addEventListener("mousedown", onClick);
    window.addEventListener("keydown", onKey);
    if (!isMobile) {
      const onScroll = (e: Event) => {
        // A capped popover scrolls inside; that is not the page.
        if (popRef.current?.contains(e.target as Node)) return;
        setOpen(false);
      };
      window.addEventListener("scroll", onScroll, true);
      return () => {
        document.removeEventListener("mousedown", onClick);
        window.removeEventListener("keydown", onKey);
        window.removeEventListener("scroll", onScroll, true);
      };
    }
    return () => {
      document.removeEventListener("mousedown", onClick);
      window.removeEventListener("keydown", onKey);
    };
  }, [open, isMobile]);

  // Lock body scroll while the mobile modal is open so the operator
  // can't accidentally scroll the records list behind the backdrop.
  useEffect(() => {
    if (!open || !isMobile || typeof document === "undefined") return;
    const prev = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => {
      document.body.style.overflow = prev;
    };
  }, [open, isMobile]);

  // Close the popover as soon as save succeeds. The page revalidation
  // triggered by updateRecordAction refreshes the row in place, so
  // leaving the panel open with stale defaultValues is just visual noise.
  useEffect(() => {
    if (state?.ok === true) {
      setOpen(false);
    }
  }, [state]);

  function handleDeleteClick() {
    const ok = window.confirm(
      `Delete DNS record?\n\n${record.name}  ${record.type}  ${record.content}\n\nThis cannot be undone.`,
    );
    if (!ok) return;
    startDelete(async () => {
      await deleteAction();
    });
  }

  const panelBaseStyle: React.CSSProperties = {
    background: "var(--bg-elev-solid)",
    border: "1px solid var(--border-strong)",
    borderRadius: "var(--radius)",
    padding: "14px 16px",
    boxShadow: "0 12px 32px rgba(0, 0, 0, 0.5)",
    zIndex: 9999,
  };

  return (
    <div className={localStyles.row} ref={wrapRef}>
      <button
        type="button"
        className={localStyles.editSummary}
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
      >
        Edit
      </button>
      <button
        type="button"
        className={`${styles.btnDanger} ${localStyles.iconBtn}`}
        aria-label="Delete record"
        title="Delete record"
        disabled={deletePending}
        onClick={handleDeleteClick}
      >
        <svg
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2"
          strokeLinecap="round"
          strokeLinejoin="round"
          aria-hidden="true"
        >
          <path d="M3 6h18" />
          <path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" />
          <path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6" />
          <path d="M10 11v6" />
          <path d="M14 11v6" />
        </svg>
      </button>

      {open && (isMobile || pos) && typeof document !== "undefined" &&
        createPortal(
          <>
            {isMobile && (
              <div
                onClick={() => setOpen(false)}
                aria-hidden="true"
                style={{
                  position: "fixed",
                  inset: 0,
                  background: "var(--scrim)",
                  backdropFilter: "blur(6px) saturate(120%)",
                  WebkitBackdropFilter: "blur(6px) saturate(120%)",
                  zIndex: 9998,
                }}
              />
            )}
            <div
              ref={popRef}
              role="dialog"
              aria-modal={isMobile ? "true" : undefined}
              style={
                isMobile
                  ? {
                      ...panelBaseStyle,
                      position: "fixed",
                      top: "50%",
                      left: "50%",
                      transform: "translate(-50%, -50%)",
                      width: "calc(100vw - 24px)",
                      maxWidth: 420,
                      maxHeight: "calc(100dvh - 24px)",
                      overflowY: "auto",
                    }
                  : {
                      ...panelBaseStyle,
                      position: "fixed",
                      top: pos!.top ?? "auto",
                      bottom: pos!.bottom ?? "auto",
                      right: pos!.right,
                      width: 380,
                      maxWidth: "calc(100vw - 16px)",
                      maxHeight: pos!.maxHeight,
                      overflowY: "auto",
                    }
              }
            >
              <div className={localStyles.editTitle}>
                Edit{" "}
                <span className={styles.code}>
                  {record.name} {record.type}
                </span>
              </div>
              <form action={formAction}>
                <input type="hidden" name="original_type" value={record.type} />
                <label className={styles.field} style={{ marginBottom: 8 }}>
                  Name
                  <input
                    name="name"
                    className={styles.input}
                    defaultValue={labelForEditing(record.name, zone)}
                    readOnly={isSoa}
                    placeholder="@ or sub"
                    autoComplete="off"
                    required
                  />
                  <span
                    className={styles.muted}
                    style={{ fontSize: 12, marginTop: 4 }}
                  >
                    Use <span className={styles.code}>@</span> for the zone apex,
                    or a label like <span className={styles.code}>mail</span> to
                    store{" "}
                    <span className={styles.code}>mail.{zone}</span>. Typing the
                    full FQDN works too — the agent normalises it on save.
                  </span>
                </label>
                <label className={styles.field} style={{ marginBottom: 8 }}>
                  Type
                  {/* Disabled for SOA, so no type is submitted and it can never
                      be retyped. The record's own type is always an option:
                      a type missing from the list made the browser pick the
                      first option and saved the record as an A. */}
                  <select
                    name="type"
                    className={styles.select}
                    defaultValue={record.type}
                    disabled={isSoa}
                  >
                    {typeOptions.map((t) => (
                      <option key={t} value={t}>
                        {t}
                      </option>
                    ))}
                  </select>
                  <span
                    className={styles.muted}
                    style={{ fontSize: 12, marginTop: 4 }}
                  >
                    {isSoa
                      ? "The SOA record always stays an SOA at the zone apex. Only its content and TTL can change."
                      : "Changing type also requires the Content field to match the new type (e.g. an IP for A, a hostname for CNAME)."}
                  </span>
                </label>
                <label className={styles.field} style={{ marginBottom: 8 }}>
                  Content
                  <input
                    name="content"
                    className={styles.input}
                    defaultValue={record.content}
                    required
                  />
                </label>
                <label className={styles.field} style={{ marginBottom: 8 }}>
                  TTL
                  <input
                    name="ttl"
                    type="number"
                    className={styles.input}
                    defaultValue={record.ttl}
                    min={1}
                    step={1}
                  />
                </label>
                <label className={styles.field} style={{ marginBottom: 8 }}>
                  Priority
                  <input
                    name="prio"
                    type="number"
                    className={styles.input}
                    defaultValue={record.prio}
                    min={0}
                    step={1}
                    placeholder="0"
                  />
                  <span
                    className={styles.muted}
                    style={{ fontSize: 12, marginTop: 4 }}
                  >
                    Used by MX and SRV records. Leave at 0 for other types.
                  </span>
                </label>
                {state?.ok === false && (
                  <div className={styles.error}>{state.error}</div>
                )}
                <div
                  style={{
                    display: "flex",
                    gap: 8,
                    justifyContent: "flex-end",
                    marginTop: 4,
                  }}
                >
                  <button
                    type="button"
                    className={styles.btnGhost}
                    onClick={() => setOpen(false)}
                    disabled={pending}
                  >
                    Cancel
                  </button>
                  <button
                    type="submit"
                    className={styles.btn}
                    disabled={pending}
                  >
                    {pending ? "..." : "Save changes"}
                  </button>
                </div>
              </form>
            </div>
          </>,
          document.body,
        )}
    </div>
  );
}
