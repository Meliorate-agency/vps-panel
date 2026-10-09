"use client";

import { useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import StatusBadge from "@/components/StatusBadge/StatusBadge";
import topbarStyles from "@/components/Topbar/Topbar.module.css";
import pageStyles from "@/components/page.module.css";
import type { GitStatusResp } from "@/lib/agent";

type UpdateResult =
  | { ok: true; taskId: string }
  | { ok: false; error: string };

type Props = {
  appName: string;
  /** Snapshot from agent.apps.gitStatus(name) taken at server-render time;
   *  undefined when the call failed or was skipped (no code on disk). */
  status: GitStatusResp | undefined;
  /** The source folder is missing or empty, so there is nothing to check. */
  sourceMissing: boolean;
  /** Server action that triggers pull + reinstall + restart on the agent. */
  updateAction: (name: string) => Promise<UpdateResult>;
};

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

function classifyError(err: string): string {
  const e = err.toLowerCase();
  if (e.includes("dubious ownership") || e.includes("safe.directory")) {
    return "ownership blocked";
  }
  if (e.includes("permission denied (publickey)")) return "ssh denied";
  if (e.includes("could not resolve host")) return "dns error";
  if (e.includes("could not connect") || e.includes("connection refused")) {
    return "no network";
  }
  if (e.includes("timed out") || e.includes("timeout")) return "timed out";
  if (e.includes("authentication failed")) return "auth failed";
  if (e.includes("does not appear to be a git repository")) return "bad remote";
  return "check failed";
}

/**
 * "Updates" section on /apps/[name]. Shows git status + two explicit
 * buttons: Check for updates (re-runs the server fetch via router.refresh)
 * and Rebuild (calls updateAction which runs pull + npm ci + npm run build
 * + systemctl restart on the agent, then redirects to /tasks/<id> so the
 * operator watches the live log).
 *
 * Inline status mirrors the same logic as GitStatusBadge on /apps but laid
 * out as a multi-line block instead of a single pill, so SHAs are
 * always visible without hovering.
 */
export default function UpdatesPanel({ appName, status, sourceMissing, updateAction }: Props) {
  const router = useRouter();
  const [checking, startCheck] = useTransition();
  const [rebuilding, startRebuild] = useTransition();
  const [rebuildError, setRebuildError] = useState<string | null>(null);

  const onCheck = () => {
    setRebuildError(null);
    startCheck(() => {
      // router.refresh re-runs the server component which calls
      // agent.apps.gitStatus again with a fresh git fetch on the agent
      // side. Status prop updates on next render.
      router.refresh();
    });
  };

  const onRebuild = () => {
    setRebuildError(null);
    startRebuild(async () => {
      const r = await updateAction(appName);
      if (!r.ok) {
        setRebuildError(r.error);
        return;
      }
      router.push(`/tasks/${encodeURIComponent(r.taskId)}`);
    });
  };

  // Compute display state.
  const noRepo = !status || !status.has_repo;
  const diverged =
    !!status && !status.error && status.ahead > 0 && status.behind > 0;
  const upToDate =
    !!status &&
    !status.error &&
    status.has_repo &&
    status.behind === 0 &&
    status.ahead === 0;
  const aheadOnly =
    !!status &&
    !status.error &&
    status.has_repo &&
    status.behind === 0 &&
    status.ahead > 0;
  const behind =
    !!status &&
    !status.error &&
    status.has_repo &&
    status.behind > 0 &&
    status.ahead === 0;
  const errorLabel = status?.error ? classifyError(status.error) : null;

  // Rebuild is sensible whenever there IS a repo and we're not diverged
  // (diverged → git pull --ff-only would refuse; operator must resolve
  // manually first).
  const canRebuild = !!status && status.has_repo && !diverged;

  return (
    <div>
      <p className={pageStyles.muted} style={{ marginBottom: 10 }}>
        Pull the latest commit from the configured repo, reinstall deps, run{" "}
        <span className={pageStyles.code}>npm run build</span> if defined, then
        restart the systemd unit. Same flow as the clickable badge on{" "}
        <a href="/apps" className={pageStyles.code}>/apps</a>.
      </p>

      {/* Status block */}
      <div
        style={{
          display: "grid",
          gap: 6,
          marginBottom: 14,
          padding: "12px 14px",
          background: "var(--bg-elev)",
          border: "1px solid var(--border)",
          borderRadius: 8,
          fontSize: 13,
        }}
      >
        <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
          <span className={pageStyles.muted}>Status:</span>
          {sourceMissing ? (
            <StatusBadge text="no code on disk" tone="warn" />
          ) : !status ? (
            // The status call failed, so this is not "still checking": it used
            // to read "(checking...)" forever.
            <span className={pageStyles.muted}>
              unavailable (the agent did not answer) - use Check for updates to retry
            </span>
          ) : noRepo ? (
            <StatusBadge text="no git" tone="muted" />
          ) : errorLabel ? (
            <span title={status?.error ?? ""}>
              <StatusBadge text={errorLabel} tone="warn" />
            </span>
          ) : upToDate ? (
            <StatusBadge text="up to date" tone="ok" />
          ) : diverged ? (
            <StatusBadge
              text={`${status.behind} behind / ${status.ahead} ahead`}
              tone="warn"
            />
          ) : behind ? (
            <StatusBadge
              text={`${status.behind} commit${status.behind === 1 ? "" : "s"} behind`}
              tone="warn"
            />
          ) : aheadOnly ? (
            <StatusBadge text={`${status.ahead} ahead`} tone="muted" />
          ) : null}
        </div>

        {status?.current_sha && (
          <div>
            <span className={pageStyles.muted}>HEAD:</span>{" "}
            <span className={pageStyles.code}>{status.current_sha}</span>
          </div>
        )}
        {status?.upstream_sha && (
          <div>
            <span className={pageStyles.muted}>Upstream:</span>{" "}
            <span className={pageStyles.code}>{status.upstream_sha}</span>
          </div>
        )}
        {status?.error && (
          <div className={pageStyles.muted} style={{ wordBreak: "break-word" }}>
            <span>{status.error}</span>
          </div>
        )}
        {diverged && (
          <div className={pageStyles.muted}>
            <span className={pageStyles.code}>git pull --ff-only</span> will
            refuse. Resolve manually on the server, then re-check.
          </div>
        )}
      </div>

      {rebuildError && (
        <div className={pageStyles.error} style={{ marginBottom: 10 }}>
          {rebuildError}
        </div>
      )}

      <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
        <button
          type="button"
          className={pageStyles.btnGhost}
          onClick={onCheck}
          disabled={checking || rebuilding}
          title="Re-run git fetch and refresh status"
        >
          {checking ? (
            <>
              <Spinner /> Checking…
            </>
          ) : (
            "Check for updates"
          )}
        </button>
        <button
          type="button"
          className={pageStyles.btn}
          onClick={onRebuild}
          disabled={!canRebuild || rebuilding || checking}
          title={
            !canRebuild
              ? diverged
                ? "Resolve diverged history on the server first"
                : "Configure a repo URL first"
              : "Pull, reinstall, build, restart"
          }
        >
          {rebuilding ? (
            <>
              <Spinner /> Starting…
            </>
          ) : (
            "Rebuild"
          )}
        </button>
      </div>
    </div>
  );
}
