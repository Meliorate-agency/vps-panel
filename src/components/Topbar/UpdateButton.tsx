"use client";

import { useCallback, useEffect, useRef, useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import {
  checkUpdateAction,
  triggerUpdateAction,
  type CheckResult,
} from "@/server-actions/update";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";
import pageStyles from "@/components/page.module.css";
import styles from "./Topbar.module.css";

function Spinner() {
  return (
    <svg
      className={styles.spinner}
      width="14"
      height="14"
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

type Status =
  | { kind: "loading" }
  | { kind: "up_to_date"; current: string; checked_at: number }
  | { kind: "available"; behind: number; current: string; target: string; checked_at: number }
  | { kind: "diverged"; ahead: number; behind: number; checked_at: number }
  | { kind: "error"; message: string };

function statusFromCheck(result: CheckResult): Status {
  if (!result.ok) return { kind: "error", message: result.error };
  const d = result.data;
  const now = Date.now();
  if (d.fetch_error) return { kind: "error", message: d.fetch_error };
  if (d.ahead > 0 && d.behind > 0) {
    return { kind: "diverged", ahead: d.ahead, behind: d.behind, checked_at: now };
  }
  if (d.behind > 0) {
    return {
      kind: "available",
      behind: d.behind,
      current: d.current_commit,
      target: d.target_commit,
      checked_at: now,
    };
  }
  return { kind: "up_to_date", current: d.current_commit, checked_at: now };
}

export default function UpdateButton() {
  const [status, setStatus] = useState<Status>({ kind: "loading" });
  const [open, setOpen] = useState(false);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [checking, startCheck] = useTransition();
  const [installing, startInstall] = useTransition();
  const [installError, setInstallError] = useState<string | null>(null);
  const checkedOnce = useRef(false);
  const router = useRouter();

  const runCheck = useCallback(() => {
    startCheck(async () => {
      const r = await checkUpdateAction();
      setStatus(statusFromCheck(r));
    });
  }, []);

  // Run the check exactly once on mount. The user can re-check manually from
  // the popover.
  useEffect(() => {
    if (checkedOnce.current) return;
    checkedOnce.current = true;
    runCheck();
  }, [runCheck]);

  // Close popover on outside click. Skip while the install-confirm modal is
  // open — the modal's own backdrop click owns dismissal and we don't want
  // its overlay to register as "outside" and yank the popover shut too.
  const popRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open || confirmOpen) return;
    function onClick(e: MouseEvent) {
      if (!popRef.current?.contains(e.target as Node)) setOpen(false);
    }
    document.addEventListener("mousedown", onClick);
    return () => document.removeEventListener("mousedown", onClick);
  }, [open, confirmOpen]);

  const onInstall = () => {
    setInstallError(null);
    setConfirmOpen(true);
  };

  const runInstall = () => {
    startInstall(async () => {
      const r = await triggerUpdateAction();
      if (!r.ok) {
        setInstallError(r.error);
        setConfirmOpen(false);
        return;
      }
      setConfirmOpen(false);
      setOpen(false);
      router.push(`/tasks/${encodeURIComponent(r.taskId)}`);
    });
  };

  const label = (() => {
    switch (status.kind) {
      case "loading":
        return "Checking...";
      case "up_to_date":
        return "Up to date";
      case "available":
        return `Update (${status.behind})`;
      case "diverged":
        return "Diverged";
      case "error":
        return "Check failed";
    }
  })();

  const dotClass = (() => {
    switch (status.kind) {
      case "available":
        return styles.dotWarn;
      case "up_to_date":
        return styles.dotOk;
      case "diverged":
      case "error":
        return styles.dotBad;
      default:
        return styles.dotMuted;
    }
  })();

  return (
    <div className={styles.updateWrap} ref={popRef}>
      <button
        type="button"
        className={styles.updateBtn}
        onClick={() => setOpen((v) => !v)}
        title={label}
      >
        {checking ? (
          <Spinner />
        ) : (
          <span className={`${styles.updateDot} ${dotClass}`} />
        )}
        <span>{checking ? "Checking…" : label}</span>
      </button>

      {open && (
        <div className={styles.updatePop} role="dialog">
          <div className={styles.popTitle}>System update</div>

          {status.kind === "loading" && (
            <div className={styles.popMuted}>Checking origin…</div>
          )}

          {status.kind === "up_to_date" && (
            <div>
              <div className={styles.popLine}>
                <span className={styles.popMuted}>Status:</span>{" "}
                <span className={styles.popOk}>up to date</span>
              </div>
              <div className={styles.popLine}>
                <span className={styles.popMuted}>HEAD:</span>{" "}
                <code className={styles.popCode}>{status.current || "—"}</code>
              </div>
            </div>
          )}

          {status.kind === "available" && (
            <div>
              <div className={styles.popLine}>
                <span className={styles.popMuted}>Behind by</span>{" "}
                <strong>{status.behind}</strong>{" "}
                <span className={styles.popMuted}>commit{status.behind === 1 ? "" : "s"}</span>
              </div>
              <div className={styles.popLine}>
                <span className={styles.popMuted}>Current:</span>{" "}
                <code className={styles.popCode}>{status.current || "—"}</code>
              </div>
              <div className={styles.popLine}>
                <span className={styles.popMuted}>Target:</span>{" "}
                <code className={styles.popCode}>{status.target || "—"}</code>
              </div>
            </div>
          )}

          {status.kind === "diverged" && (
            <div>
              <div className={styles.popLine}>
                <span className={styles.popBad}>
                  Local has {status.ahead} commit{status.ahead === 1 ? "" : "s"} ahead AND{" "}
                  {status.behind} commit{status.behind === 1 ? "" : "s"} behind.
                </span>
              </div>
              <div className={styles.popLine}>
                <span className={styles.popMuted}>
                  `git pull --ff-only` will refuse. Resolve manually on the server,
                  then re-check.
                </span>
              </div>
            </div>
          )}

          {status.kind === "error" && (
            <div>
              <div className={styles.popBad}>Could not check.</div>
              <div
                className={styles.popMuted}
                style={{ marginTop: 4, whiteSpace: "pre-wrap", overflowWrap: "anywhere" }}
              >
                {status.message}
              </div>
            </div>
          )}

          {installError && (
            <div className={styles.popBad} style={{ marginTop: 8 }}>
              {installError}
            </div>
          )}

          <div className={styles.popActions}>
            <button
              type="button"
              className={styles.popGhost}
              onClick={runCheck}
              disabled={checking}
            >
              {checking ? (
                <>
                  <Spinner /> Checking…
                </>
              ) : (
                "Re-check"
              )}
            </button>
            <button
              type="button"
              className={styles.popPrimary}
              onClick={onInstall}
              disabled={
                installing ||
                checking ||
                status.kind === "loading" ||
                status.kind === "diverged" ||
                status.kind === "error"
              }
              title={
                status.kind === "up_to_date"
                  ? "Already up to date — running this would still git pull (no-op) and rebuild. Click anyway if you want a fresh build."
                  : ""
              }
            >
              {installing ? (
                <>
                  <Spinner /> Starting…
                </>
              ) : status.kind === "available" ? (
                "Install update"
              ) : (
                "Rebuild anyway"
              )}
            </button>
          </div>
        </div>
      )}

      <ConfirmDialog
        open={confirmOpen}
        title="Pull latest and rebuild?"
        confirmLabel={installing ? "Starting…" : "Install update"}
        cancelLabel="Cancel"
        pending={installing}
        onConfirm={runInstall}
        onCancel={() => {
          if (installing) return;
          setConfirmOpen(false);
        }}
      >
        <p>This will run on the server:</p>
        <ul>
          <li>
            <span className={pageStyles.code}>git pull --ff-only</span> in the
            repo
          </li>
          <li>
            <span className={pageStyles.code}>bash scripts/update.sh</span>{" "}
            (Rust agent rebuild + Next.js panel rebuild)
          </li>
          <li>Both <span className={pageStyles.code}>vps-panel-agent</span> and{" "}
            <span className={pageStyles.code}>vps-panel</span> services restart</li>
        </ul>
        {status.kind === "available" && (
          <p>
            Pulling{" "}
            <strong>
              {status.behind} commit{status.behind === 1 ? "" : "s"}
            </strong>{" "}
            from <span className={pageStyles.code}>{status.current.slice(0, 7) || "?"}</span>{" "}
            → <span className={pageStyles.code}>{status.target.slice(0, 7) || "?"}</span>.
          </p>
        )}
        {status.kind === "up_to_date" && (
          <p className={pageStyles.muted}>
            Already up to date — this will git pull (no-op) and rebuild anyway.
          </p>
        )}
        <div
          style={{
            marginTop: 12,
            padding: "8px 10px",
            background: "rgba(255, 196, 0, 0.08)",
            border: "1px solid rgba(255, 196, 0, 0.28)",
            borderRadius: 6,
            fontSize: 12,
          }}
        >
          Takes about 5–15 minutes. You&apos;ll be redirected to the task page
          to watch the live log. Once the panel restarts you may need to
          refresh.
        </div>
      </ConfirmDialog>
    </div>
  );
}
