"use client";

import { useEffect, useLayoutEffect, useRef, useState, useTransition } from "react";
import { createPortal } from "react-dom";
import { useRouter } from "next/navigation";
import StatusBadge from "@/components/StatusBadge/StatusBadge";
import topbarStyles from "@/components/Topbar/Topbar.module.css";
import pageStyles from "@/components/page.module.css";
import type { GitStatusResp } from "@/lib/agent";

type UpdateResult =
  | { ok: true; taskId: string }
  | { ok: false; error: string };

type RefreshResult =
  | { ok: true; status: GitStatusResp }
  | { ok: false; error: string };

type Props = {
  appName: string;
  status: GitStatusResp | undefined;
  isExternal: boolean;
  /** Server action that triggers pull + reinstall + restart on the agent. */
  updateAction: (name: string) => Promise<UpdateResult>;
  /** Server action that runs this app's git fetch comparison again. */
  refreshAction: (name: string) => Promise<RefreshResult>;
};

/** Desktop popover width, the same whatever it shows: a failed fetch's
 *  error text used to stretch it across the whole screen. */
const POP_WIDTH = 300;

function Spinner() {
  return (
    <svg
      className={topbarStyles.spinner}
      width="12"
      height="12"
      viewBox="0 0 24 24"
      fill="none"
      aria-hidden="true"
    >
      <circle
        cx="12"
        cy="12"
        r="9"
        stroke="currentColor"
        strokeWidth="2.5"
        strokeLinecap="round"
        strokeDasharray="42 60"
      />
    </svg>
  );
}

/** Nothing to act on: no popover, the badge is inert. */
function isInert(s: GitStatusResp): boolean {
  return !s.has_repo || (!s.error && s.behind === 0);
}

/**
 * Renders the per-app git-status badge (used in the Git column on /apps).
 * For actionable states (behind / diverged / error) the badge becomes a
 * button that opens a small popover mirroring the panel's UpdateButton —
 * details on the left, an "Update + restart" primary on the right. Clicking
 * the primary fires `updateAction` which spawns an agent task; on success
 * the operator is redirected to /tasks/<id> to watch the live log. After a
 * failed fetch the primary is "Retry fetch" instead, which re-runs the check
 * for this app only and shows the new result in place.
 *
 * For non-actionable states (up-to-date / no git / external / unknown) the
 * badge stays inert, matching the previous read-only behaviour.
 */
export default function GitStatusBadge({
  appName,
  status,
  isExternal,
  updateAction,
  refreshAction,
}: Props) {
  const router = useRouter();
  const [open, setOpen] = useState(false);
  const [pending, startTransition] = useTransition();
  const [retrying, startRetry] = useTransition();
  const [error, setError] = useState<string | null>(null);
  const [retryNote, setRetryNote] = useState<string | null>(null);
  // A retried status, kept only while the page still shows the status it
  // replaced: a page refresh brings a newer one from the server.
  const [retried, setRetried] = useState<{ base: GitStatusResp | undefined; status: GitStatusResp } | null>(null);
  const live = retried && retried.base === status ? retried.status : status;
  const wrapRef = useRef<HTMLDivElement>(null);
  const popRef = useRef<HTMLDivElement>(null);
  // Viewport-anchored position computed from the trigger's bounding rect.
  // We use `position: fixed` on the portaled popover; that combined with
  // body-level mounting escapes the Table.wrap stacking context (which
  // backdrop-filter creates as a containing block, trapping anything
  // positioned inside it even with high z-index).
  const [pos, setPos] = useState<{ top?: number; bottom?: number; right: number } | null>(null);
  const [, setResized] = useState(0);
  // Phone-vs-desktop mode for the popover. On phone we ignore the
  // anchored position and render a fully centered modal with a backdrop
  // so the popup is reachable and fully visible — the trigger button is
  // often at the bottom of the screen / inside a horizontally-scrolled
  // table where anchoring would put the popup off-screen.
  const [isMobile, setIsMobile] = useState(false);

  // Mobile detection. Tracks the same 768px breakpoint everything else
  // in the panel uses. Listens for changes so a desktop → phone resize
  // (DevTools) reconfigures the open popup correctly.
  useEffect(() => {
    if (typeof window === "undefined" || !window.matchMedia) return;
    const mql = window.matchMedia("(max-width: 768px)");
    setIsMobile(mql.matches);
    const onChange = (e: MediaQueryListEvent) => setIsMobile(e.matches);
    mql.addEventListener("change", onChange);
    return () => mql.removeEventListener("change", onChange);
  }, []);

  // Anchor the popover under the badge (desktop only — mobile is
  // viewport-centered), or above it when it would run past the bottom of
  // the window and there is more room above: a failed fetch's error makes
  // it tall, and scrolling the page to reach it closes it. Runs after every
  // render while open, so new content (a retry) re-places it; the state only
  // changes when the position does. Resizes re-render to re-place it too.
  useLayoutEffect(() => {
    if (!open || isMobile || !wrapRef.current) return;
    const r = wrapRef.current.getBoundingClientRect();
    const height = popRef.current?.offsetHeight ?? 0;
    const right = Math.max(8, Math.round(window.innerWidth - r.right));
    const roomBelow = window.innerHeight - r.bottom;
    const next =
      height + 14 > roomBelow && r.top > roomBelow
        ? { bottom: Math.round(window.innerHeight - r.top + 6), right }
        : { top: Math.round(r.bottom + 6), right };
    setPos((p) =>
      p && p.top === next.top && p.bottom === next.bottom && p.right === next.right ? p : next,
    );
  });
  useEffect(() => {
    if (!open || isMobile) return;
    const onResize = () => setResized((n) => n + 1);
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, [open, isMobile]);

  // Close popover on outside click (trigger OR portaled body). Desktop
  // additionally closes on any scroll because the popover is anchored
  // to the trigger and tracking through a backdrop-filter'd container
  // is fragile. Mobile is a centered modal that owns the screen — body
  // scroll is locked instead so there's nothing to close on.
  useEffect(() => {
    if (!open) return;
    function onClick(e: MouseEvent) {
      const target = e.target as Node;
      if (wrapRef.current?.contains(target)) return;
      if (popRef.current?.contains(target)) return;
      setOpen(false);
    }
    document.addEventListener("mousedown", onClick);
    if (!isMobile) {
      const onScroll = (e: Event) => {
        // The error text scrolls inside the popover; that is not the page.
        if (popRef.current?.contains(e.target as Node)) return;
        setOpen(false);
      };
      window.addEventListener("scroll", onScroll, true);
      return () => {
        document.removeEventListener("mousedown", onClick);
        window.removeEventListener("scroll", onScroll, true);
      };
    }
    return () => document.removeEventListener("mousedown", onClick);
  }, [open, isMobile]);

  // Mobile: lock background scroll while the centered modal is open so
  // the operator can't accidentally scroll the apps list behind the
  // backdrop. Desktop popover is anchored + small enough that the page
  // can stay interactive — no lock there.
  useEffect(() => {
    if (!open || !isMobile || typeof document === "undefined") return;
    const prev = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => {
      document.body.style.overflow = prev;
    };
  }, [open, isMobile]);

  // ESC closes on both modes — keyboard users on desktop expect it, and
  // hardware-keyboard tablets on mobile do too.
  useEffect(() => {
    if (!open) return;
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") setOpen(false);
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open]);

  const runUpdate = () => {
    setError(null);
    startTransition(async () => {
      const r = await updateAction(appName);
      if (!r.ok) {
        setError(r.error);
        return;
      }
      setOpen(false);
      router.push(`/tasks/${encodeURIComponent(r.taskId)}`);
    });
  };

  const retryFetch = () => {
    setError(null);
    setRetryNote(null);
    startRetry(async () => {
      const r = await refreshAction(appName);
      if (!r.ok) {
        setRetryNote(`Retry failed: ${r.error}`);
        return;
      }
      setRetried({ base: status, status: r.status });
      if (r.status.error) {
        setRetryNote("Still failing (checked just now).");
      } else if (isInert(r.status)) {
        // Up to date now: the badge says so, nothing left to open.
        setOpen(false);
      }
    });
  };

  // ─── Inert states (no popover) ──────────────────────────────────────
  if (isExternal) {
    return <span className={pageStyles.muted}>—</span>;
  }
  if (!live) {
    return <span className={pageStyles.muted}>—</span>;
  }
  if (!live.has_repo) {
    return (
      <span title="source_dir is not a git checkout">
        <StatusBadge text="no git" tone="muted" />
      </span>
    );
  }
  // Up-to-date: read-only green pill. No action needed.
  if (!live.error && live.behind === 0 && live.ahead === 0) {
    return (
      <span title={`up to date · ${live.current_sha ?? ""}`}>
        <StatusBadge text="up to date" tone="ok" />
      </span>
    );
  }
  // Ahead-only (local has unpushed commits, no upstream changes to pull).
  // The update flow would do `git pull --ff-only` which is a no-op here —
  // not worth offering the button, just inform.
  if (!live.error && live.behind === 0 && live.ahead > 0) {
    return (
      <span
        title={`local has unpushed commits: ${live.current_sha ?? ""} (upstream ${live.upstream_sha ?? ""})`}
      >
        <StatusBadge text={`${live.ahead} ahead`} tone="muted" />
      </span>
    );
  }

  // ─── Actionable states (clickable badge + popover) ──────────────────
  const errorLabel = (() => {
    if (!live.error) return null;
    const e = live.error.toLowerCase();
    if (e.includes("dubious ownership") || e.includes("safe.directory")) {
      return "ownership blocked";
    }
    if (e.includes("permission denied (publickey)")) return "ssh denied";
    if (e.includes("repository not found")) return "no access";
    if (e.includes("could not resolve host")) return "dns error";
    if (e.includes("could not connect") || e.includes("connection refused")) {
      return "no network";
    }
    if (e.includes("timed out") || e.includes("timeout")) return "timed out";
    if (e.includes("authentication failed")) return "auth failed";
    if (e.includes("does not appear to be a git repository")) return "bad remote";
    return "check failed";
  })();

  const diverged = live.ahead > 0 && live.behind > 0;
  const badgeText = errorLabel
    ? errorLabel
    : diverged
      ? `${live.behind} behind / ${live.ahead} ahead`
      : `${live.behind} commit${live.behind === 1 ? "" : "s"} behind`;

  return (
    <div className={topbarStyles.updateWrap} ref={wrapRef}>
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        title={live.error ?? "Click for update options"}
        style={{
          background: "transparent",
          border: 0,
          padding: 0,
          cursor: "pointer",
          color: "inherit",
        }}
      >
        <StatusBadge text={badgeText} tone="warn" />
      </button>

      {open && (isMobile || pos) && typeof document !== "undefined" && createPortal(
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
                animation: "vps-gsb-fadein 0.12s ease-out",
              }}
            />
          )}
          <div
            ref={popRef}
            className={topbarStyles.updatePop}
            role="dialog"
            aria-modal={isMobile ? "true" : undefined}
            style={
              isMobile
                ? {
                    // Centered modal. width: calc(100vw - 24px) gives 12px
                    // gutter each side; max-width caps it on tablets where
                    // (max-width: 768px) still applies but the screen is
                    // wider than a phone. max-height: 100dvh - 24px so the
                    // popup fits inside iOS Safari's visible viewport
                    // (vh-includes-address-bar issue).
                    position: "fixed",
                    top: "50%",
                    left: "50%",
                    right: "auto",
                    transform: "translate(-50%, -50%)",
                    width: "calc(100vw - 24px)",
                    maxWidth: 420,
                    maxHeight: "calc(100dvh - 24px)",
                    overflowY: "auto",
                    overflowWrap: "anywhere",
                    zIndex: 9999,
                  }
                : {
                    position: "fixed",
                    // "auto", not left out: .updatePop sets its own top,
                    // which with a bottom would squeeze the popover.
                    top: pos!.top ?? "auto",
                    bottom: pos!.bottom ?? "auto",
                    right: pos!.right,
                    width: POP_WIDTH,
                    maxWidth: "calc(100vw - 16px)",
                    overflowWrap: "anywhere",
                    zIndex: 9999,
                  }
            }
          >
            <div className={topbarStyles.popTitle}>
              Update {appName}
            </div>

          {live.error && (
            <div className={topbarStyles.popLine}>
              <span className={topbarStyles.popBad}>Last fetch failed.</span>
              <pre className={topbarStyles.popError}>{live.error}</pre>
            </div>
          )}

          {!live.error && diverged && (
            <div className={topbarStyles.popLine}>
              <span className={topbarStyles.popBad}>
                Local has {live.ahead} commit{live.ahead === 1 ? "" : "s"}{" "}
                ahead AND {live.behind} commit
                {live.behind === 1 ? "" : "s"} behind.
              </span>
              <div className={topbarStyles.popMuted} style={{ marginTop: 4 }}>
                <span className={pageStyles.code}>git pull --ff-only</span> will
                refuse. Resolve manually on the server, then re-check.
              </div>
            </div>
          )}

          {!live.error && !diverged && live.behind > 0 && (
            <div className={topbarStyles.popLine}>
              <span className={topbarStyles.popMuted}>Behind by</span>{" "}
              <strong>{live.behind}</strong>{" "}
              <span className={topbarStyles.popMuted}>
                commit{live.behind === 1 ? "" : "s"}
              </span>
            </div>
          )}

          {live.current_sha && (
            <div className={topbarStyles.popLine}>
              <span className={topbarStyles.popMuted}>Current:</span>{" "}
              <code className={topbarStyles.popCode}>{live.current_sha}</code>
            </div>
          )}
          {live.upstream_sha && (
            <div className={topbarStyles.popLine}>
              <span className={topbarStyles.popMuted}>Upstream:</span>{" "}
              <code className={topbarStyles.popCode}>
                {live.upstream_sha}
              </code>
            </div>
          )}

          {!live.error && (
            <div
              className={topbarStyles.popMuted}
              style={{ marginTop: 8, lineHeight: 1.4 }}
            >
              Runs on the server:
              <ul style={{ margin: "4px 0 0 18px", padding: 0 }}>
                <li>
                  <span className={pageStyles.code}>git pull --ff-only</span>
                </li>
                <li>
                  <span className={pageStyles.code}>npm ci</span> /{" "}
                  <span className={pageStyles.code}>npm install</span>
                </li>
                <li>
                  <span className={pageStyles.code}>npm run build</span> (if
                  defined)
                </li>
                <li>
                  <span className={pageStyles.code}>systemctl restart</span>{" "}
                  vps-app-{appName}.service
                </li>
              </ul>
            </div>
          )}

          {retryNote && (
            <div className={topbarStyles.popBad} style={{ marginTop: 8 }}>
              {retryNote}
            </div>
          )}
          {error && (
            <div className={topbarStyles.popBad} style={{ marginTop: 8 }}>
              {error}
            </div>
          )}

          <div className={topbarStyles.popActions}>
            <button
              type="button"
              className={topbarStyles.popGhost}
              onClick={() => setOpen(false)}
              disabled={pending || retrying}
            >
              Close
            </button>
            {live.error ? (
              <button
                type="button"
                className={topbarStyles.popPrimary}
                onClick={retryFetch}
                disabled={retrying}
                title="Run git fetch for this app again"
              >
                {retrying ? (
                  <>
                    <Spinner /> Fetching…
                  </>
                ) : (
                  "Retry fetch"
                )}
              </button>
            ) : (
              <button
                type="button"
                className={topbarStyles.popPrimary}
                onClick={runUpdate}
                disabled={pending || diverged}
                title={
                  diverged
                    ? "Resolve diverged history manually first"
                    : "Pull latest, reinstall, build, restart"
                }
              >
                {pending ? (
                  <>
                    <Spinner /> Starting…
                  </>
                ) : (
                  "Update + restart"
                )}
              </button>
            )}
          </div>
          </div>
        </>,
        document.body,
      )}
    </div>
  );
}
