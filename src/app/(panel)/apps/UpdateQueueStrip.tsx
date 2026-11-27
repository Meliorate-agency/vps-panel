"use client";

import { useEffect, useState, useTransition } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import type { AppUpdateOutcome, AppUpdateQueue } from "@/lib/agent";
import {
  appUpdateQueueAction,
  dismissUpdateAllResultAction,
  stopAppUpdateQueueAction,
} from "@/server-actions/apps";
import styles from "./UpdateQueueStrip.module.css";

const POLL_MS = 2000;

const RESULT_TEXT: Record<AppUpdateOutcome["result"], string> = {
  updated: "updated",
  up_to_date: "up to date",
  failed: "failed",
  skipped: "skipped",
};

/**
 * The Update all queue on /apps: which app is being checked or updated,
 * which wait, and what happened to the ones done. Follows the agent every
 * 2 s while the queue runs and refreshes the page when it ends, so the
 * Updates column shows the new state. The last run's result stays until it
 * is dismissed (remembered in panel.db) or the next run starts.
 */
export default function UpdateQueueStrip({
  initial,
  dismissedTaskId,
  canManage,
}: {
  initial: AppUpdateQueue | null;
  dismissedTaskId: string;
  canManage: boolean;
}) {
  const router = useRouter();
  const [queue, setQueue] = useState(initial);
  const [seenInitial, setSeenInitial] = useState(initial);
  const [dismissed, setDismissed] = useState(dismissedTaskId);
  const [error, setError] = useState<string | null>(null);
  const [stopping, startStop] = useTransition();

  // A page refresh (Update all started, or the run ended) hands a newer
  // queue down.
  if (initial !== seenInitial) {
    setSeenInitial(initial);
    setQueue(initial);
  }

  const running = queue?.running ?? false;
  useEffect(() => {
    if (!running) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout>;
    const tick = async () => {
      const res = await appUpdateQueueAction().catch(() => null);
      if (cancelled) return;
      if (res?.ok) {
        setQueue(res.queue);
        if (!res.queue.running) {
          router.refresh();
          return;
        }
      }
      timer = setTimeout(tick, POLL_MS);
    };
    timer = setTimeout(tick, POLL_MS);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [running, router]);

  if (!queue || !queue.task_id) return null;
  if (!running && (queue.done.length === 0 || dismissed === queue.task_id)) return null;

  const stop = () => {
    setError(null);
    startStop(async () => {
      const res = await stopAppUpdateQueueAction();
      if (res.ok) setQueue(res.queue);
      else setError(res.error);
    });
  };

  const dismiss = () => {
    const id = queue.task_id ?? "";
    setDismissed(id);
    void dismissUpdateAllResultAction(id);
  };

  const count = (r: AppUpdateOutcome["result"]) => queue.done.filter((o) => o.result === r).length;
  const failed = count("failed");
  const total = queue.done.length + queue.waiting.length + (queue.current ? 1 : 0);
  // Up-to-date apps are only counted; the rest get a line each.
  const notable = queue.done.filter((o) => o.result !== "up_to_date");
  const tone = running ? styles.running : failed > 0 ? styles.failed : styles.finished;

  return (
    <div className={`${styles.strip} ${tone}`} role="status" aria-live="polite">
      <div className={styles.row}>
        {running && <span className={styles.spinner} aria-hidden="true" />}
        <div className={styles.text}>
          {running ? (
            <>
              <strong>Update all:</strong> {queue.done.length} of {total} {total === 1 ? "app" : "apps"} done.
              {queue.current && (
                <>
                  {" "}
                  Now <strong>{queue.current.name}</strong> (
                  {queue.current.step === "updating" ? "updating" : "checking for something new"}
                  {queue.current.task_id && (
                    <>
                      , <Link href={`/tasks/${queue.current.task_id}`}>its log</Link>
                    </>
                  )}
                  ).
                </>
              )}
              {queue.waiting.length > 0 && (
                <span className={styles.dim}>
                  {" "}
                  Next: {queue.waiting.slice(0, 4).join(", ")}
                  {queue.waiting.length > 4 ? `, +${queue.waiting.length - 4} more` : ""}.
                </span>
              )}
            </>
          ) : (
            <>
              <strong>Update all finished:</strong> {count("updated")} updated, {count("up_to_date")} up to date
              {count("skipped") > 0 ? `, ${count("skipped")} skipped` : ""}
              {failed > 0 ? `, ${failed} failed` : ""}.
            </>
          )}
        </div>
        <div className={styles.buttons}>
          <Link href={`/tasks/${queue.task_id}`} className={styles.linkBtn}>
            Queue log
          </Link>
          {running && canManage && queue.waiting.length > 0 && (
            <button type="button" className={styles.btn} onClick={stop} disabled={stopping}>
              {stopping ? "Stopping..." : "Stop after this one"}
            </button>
          )}
          {!running && (
            <button type="button" className={styles.close} onClick={dismiss} aria-label="Dismiss" title="Dismiss">
              ×
            </button>
          )}
        </div>
      </div>
      {error && <div className={styles.error}>{error}</div>}
      {notable.length > 0 && (
        <ul className={styles.list}>
          {notable.map((o) => (
            <li key={o.name} className={o.result === "failed" ? styles.itemFailed : undefined}>
              <strong>{o.name}</strong>: {RESULT_TEXT[o.result]}
              {o.detail ? ` - ${o.detail}` : ""}
              {o.task_id && (
                <>
                  {" "}
                  (<Link href={`/tasks/${o.task_id}`}>log</Link>)
                </>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
