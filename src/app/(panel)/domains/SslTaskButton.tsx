"use client";

import { useEffect, useRef, useState } from "react";
import { useRouter } from "next/navigation";
import StatusBadge from "@/components/StatusBadge/StatusBadge";
import styles from "@/components/page.module.css";
import type { SslTaskResult } from "@/lib/sslTaskResult";

type TaskSnapshot = {
  status: "pending" | "running" | "succeeded" | "failed";
  log: string[];
};

/**
 * Button for the SSL actions that hand back an agent task id (Renew, Issue
 * SSL for a mail host). Runs the server action, then follows the task by
 * polling /api/tasks/<id> until it settles and refreshes the page so the
 * badge next to it shows the new expiry. A failed task surfaces its last
 * log line inline instead of leaving the operator guessing.
 */
export default function SslTaskButton({
  label,
  pendingLabel,
  action,
  className,
}: {
  label: string;
  pendingLabel: string;
  action: () => Promise<SslTaskResult>;
  className?: string;
}) {
  const router = useRouter();
  const [phase, setPhase] = useState<"idle" | "starting" | "running" | "done">("idle");
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const taskId = useRef<string | null>(null);

  useEffect(() => {
    if (phase !== "running" || !taskId.current) return;
    let cancelled = false;
    const started = Date.now();
    const id = taskId.current;
    const poll = async () => {
      if (cancelled) return;
      if (Date.now() - started > 15 * 60 * 1000) {
        setError("The task is still running after 15 minutes. Check the Tasks page.");
        setPhase("done");
        return;
      }
      try {
        const r = await fetch(`/api/tasks/${encodeURIComponent(id)}`);
        if (r.ok) {
          const t = (await r.json()) as TaskSnapshot;
          if (t.status === "succeeded") {
            setPhase("done");
            router.refresh();
            return;
          }
          if (t.status === "failed") {
            const last = t.log.length > 0 ? t.log[t.log.length - 1] : "task failed";
            setError(last.length > 300 ? `${last.slice(0, 300)}...` : last);
            setPhase("done");
            router.refresh();
            return;
          }
        }
      } catch {
        // transient - keep polling
      }
      window.setTimeout(poll, 3_000);
    };
    window.setTimeout(poll, 2_000);
    return () => {
      cancelled = true;
    };
  }, [phase, router]);

  const run = async () => {
    setError(null);
    setNote(null);
    setPhase("starting");
    let res: SslTaskResult;
    try {
      res = await action();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setPhase("done");
      return;
    }
    if (!res.ok) {
      setError(res.error);
      setPhase("done");
      return;
    }
    if (res.taskId) {
      taskId.current = res.taskId;
      setPhase("running");
    } else {
      // Nothing to wait for (already covered, or the agent answered
      // synchronously). Refresh so the row reflects whatever changed.
      setNote(res.message ?? null);
      setPhase("done");
      router.refresh();
    }
  };

  const busy = phase === "starting" || phase === "running";

  return (
    <span style={{ display: "inline-flex", gap: 6, alignItems: "center", flexWrap: "wrap" }}>
      {phase === "running" ? (
        <StatusBadge text={pendingLabel} tone="warn" />
      ) : (
        <button
          type="button"
          className={className ?? styles.btnGhost}
          onClick={run}
          disabled={busy}
          aria-busy={busy}
        >
          {phase === "starting" ? "..." : label}
        </button>
      )}
      {error && (
        <span className={styles.error} style={{ fontSize: 11 }} title={error}>
          {error}
        </span>
      )}
      {note && !error && (
        <span className={styles.muted} style={{ fontSize: 11 }}>
          {note}
        </span>
      )}
    </span>
  );
}
