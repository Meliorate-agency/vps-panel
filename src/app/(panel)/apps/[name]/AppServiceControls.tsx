"use client";

import { useEffect, useState, useTransition } from "react";
import topbarStyles from "@/components/Topbar/Topbar.module.css";
import pageStyles from "@/components/page.module.css";
import styles from "./AppServiceControls.module.css";
import type { ActionResult } from "@/server-actions/apps";

type Kind = "start" | "stop" | "restart";

type Props = {
  /** Bound server actions from /apps/[name]/page.tsx. */
  startAction: () => Promise<ActionResult>;
  stopAction: () => Promise<ActionResult>;
  restartAction: () => Promise<ActionResult>;
  /** systemd ActiveState of the unit at server-render time
   *  (active/inactive/failed/activating/...); "" when unknown. */
  active: string;
  /** True when the app has no routing slot: Start/Restart would bind its
   *  stale port, so both stay greyed until a domain is assigned. */
  unassigned?: boolean;
};

const BUSY: Record<Kind, string> = {
  start: "Starting...",
  stop: "Stopping...",
  restart: "Restarting...",
};

const DONE: Record<Kind, string> = {
  start: "App started",
  stop: "App stopped",
  restart: "App restarted",
};

const FAILED: Record<Kind, string> = {
  start: "Start failed",
  stop: "Stop failed",
  restart: "Restart failed",
};

/** How long the success pill stays before fading out. Must match the
 *  fadeInOut animation duration in the CSS module. */
const SUCCESS_DISMISS_MS = 4000;

/** Next signals redirect() from a server action by rejecting the client
 *  promise with an error whose digest starts with NEXT_REDIRECT (the SPA
 *  navigation happens regardless). That's control flow, not a failure. */
function isRedirectError(e: unknown): boolean {
  return (
    typeof e === "object" &&
    e !== null &&
    "digest" in e &&
    String((e as { digest: unknown }).digest).startsWith("NEXT_REDIRECT")
  );
}

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

/**
 * Start / Stop / Restart row on /apps/[name]. The clicked button swaps to a
 * spinner until the agent's systemctl call AND the revalidated status badge
 * land (the transition covers both); the other two disable meanwhile so
 * conflicting systemd ops can't be queued. Outcome shows in a pill in the
 * same row: green "App started" auto-fades after SUCCESS_DISMISS_MS, red
 * errors wrap to full text and persist until the next action.
 */
export default function AppServiceControls({
  startAction,
  stopAction,
  restartAction,
  active,
  unassigned = false,
}: Props) {
  const [pending, startTransition] = useTransition();
  const [running, setRunning] = useState<Kind | null>(null);
  const [notice, setNotice] = useState<{
    tone: "ok" | "error";
    text: string;
  } | null>(null);

  // Auto-dismiss success pills; keep errors up until the next action.
  useEffect(() => {
    if (notice?.tone !== "ok") return;
    const t = setTimeout(() => setNotice(null), SUCCESS_DISMISS_MS);
    return () => clearTimeout(t);
  }, [notice]);

  const busy = running !== null || pending;

  // Grey out the no-op button for the unit's current state: a down unit has
  // nothing to stop, an up (or starting/crash-looping) one nothing to start.
  // Restart works from any state and an unknown state ("") disables neither.
  // revalidatePath in the actions refreshes `active` after every operation.
  const isDown = active === "inactive" || active === "failed";
  const isUp =
    active === "active" || active === "reloading" || active === "activating";

  function run(kind: Kind, action: () => Promise<ActionResult>) {
    if (busy) return;
    setNotice(null);
    setRunning(kind);
    startTransition(async () => {
      let r: ActionResult;
      try {
        r = await action();
      } catch (e: unknown) {
        // Session expired -> the action redirect()s to /login before its own
        // try/catch; the router is already navigating, so keep the spinner
        // and don't misreport control flow as a failure.
        if (isRedirectError(e)) return;
        // Anything else here is transport-level (network drop, server gone).
        r = { ok: false, error: e instanceof Error ? e.message : String(e) };
      }
      setRunning(null);
      setNotice(
        r.ok
          ? { tone: "ok", text: DONE[kind] }
          : { tone: "error", text: `${FAILED[kind]}: ${r.error}` },
      );
    });
  }

  function label(kind: Kind, idle: string) {
    if (running !== kind) return idle;
    return (
      <>
        <Spinner /> {BUSY[kind]}
      </>
    );
  }

  const tone =
    notice === null ? "" : notice.tone === "ok" ? styles.ok : styles.err;

  return (
    <>
      {/* Permanently mounted live region: screen readers only announce text
          changes in a region that already exists in the accessibility tree,
          so the span stays rendered (collapsed via .pill:empty) when idle. */}
      <span role="status" className={`${styles.pill} ${tone}`}>
        {notice ? (
          <>
            <span aria-hidden="true">{notice.tone === "ok" ? "✓" : "✕"}</span>{" "}
            {notice.text}
          </>
        ) : null}
      </span>
      <button
        type="button"
        className={`${pageStyles.btnGhost} ${styles.ctrl}`}
        disabled={busy || isUp || unassigned}
        title={
          unassigned
            ? "Assign a domain first"
            : isUp
              ? "App is already running"
              : undefined
        }
        aria-busy={running === "start"}
        onClick={() => run("start", startAction)}
      >
        {label("start", "Start")}
      </button>
      <button
        type="button"
        className={`${pageStyles.btnGhost} ${styles.ctrl}`}
        disabled={busy || isDown}
        title={isDown ? "App is not running" : undefined}
        aria-busy={running === "stop"}
        onClick={() => run("stop", stopAction)}
      >
        {label("stop", "Stop")}
      </button>
      <button
        type="button"
        className={`${pageStyles.btn} ${styles.ctrl}`}
        disabled={busy || unassigned}
        title={unassigned ? "Assign a domain first" : undefined}
        aria-busy={running === "restart"}
        onClick={() => run("restart", restartAction)}
      >
        {label("restart", "Restart")}
      </button>
    </>
  );
}
