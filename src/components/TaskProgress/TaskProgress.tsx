"use client";

import { useEffect, useRef, useState } from "react";
import styles from "./TaskProgress.module.css";

type Status = "pending" | "running" | "succeeded" | "failed";

type Props = {
  taskId: string;
  onComplete?: (status: "succeeded" | "failed") => void;
};

/** Lines kept in the view; older ones scroll out. */
const MAX_LINES = 6500;

export default function TaskProgress({ taskId, onComplete }: Props) {
  const [lines, setLines] = useState<string[]>([]);
  const [status, setStatus] = useState<Status>("pending");
  // Set only when both the stream AND the polling fallback are unreachable -
  // rendered in place of "(waiting for output...)" so the UI never pretends
  // a task is pending when we simply lost sight of it.
  const [streamNote, setStreamNote] = useState<string | null>(null);
  const boxRef = useRef<HTMLPreElement>(null);
  // Whether the log is scrolled to its end. New lines follow only then, so
  // scrolling up to read an error is not yanked back down by the next line.
  const atBottom = useRef(true);
  const onCompleteRef = useRef(onComplete);
  onCompleteRef.current = onComplete;

  // A new task in the same place (a second Install or Apply, most callers
  // pass no key): start from an empty log, not the last task's lines with
  // the new ones appended.
  const [shownTask, setShownTask] = useState(taskId);
  if (shownTask !== taskId) {
    setShownTask(taskId);
    setLines([]);
    setStatus("pending");
    setStreamNote(null);
  }

  useEffect(() => {
    let cancelled = false;
    let fellBack = false;
    atBottom.current = true;
    let pollTimer: number | undefined;
    let flushTimer: number | undefined;
    const es = new EventSource(`/api/tasks/${encodeURIComponent(taskId)}/stream`);

    const finish = (s: Status) => {
      setStatus(s);
      if (s === "succeeded" || s === "failed") {
        onCompleteRef.current?.(s);
      }
    };

    // The SSE path can die without a `done` event: the proxy forwards an
    // agent error as an `error` event, or the connection drops. Fall back to
    // polling the persisted task state until it's terminal. Each poll
    // replaces `lines` wholesale - the DB log is the authoritative full
    // transcript, so a partial stream + full fetch can't duplicate lines.
    const pollState = (attempt: number) => {
      if (cancelled) return;
      fetch(`/api/tasks/${encodeURIComponent(taskId)}`)
        .then((r) => {
          if (!r.ok) throw new Error(`task fetch ${r.status}`);
          return r.json() as Promise<{ status: Status; log: string[] }>;
        })
        .then((t) => {
          if (cancelled) return;
          setLines(t.log);
          if (t.status === "succeeded" || t.status === "failed") {
            finish(t.status);
          } else {
            setStatus(t.status);
            pollTimer = window.setTimeout(() => pollState(0), 2000);
          }
        })
        .catch(() => {
          if (cancelled) return;
          if (attempt < 3) {
            pollTimer = window.setTimeout(() => pollState(attempt + 1), 2000);
          } else {
            setStreamNote(
              "Lost connection to the task stream. Check the Tasks page for the outcome.",
            );
          }
        });
    };

    // Lines are added in batches. One state update per line copied the whole
    // log for every line, so watching a chatty build slowed the browser to a
    // crawl. The agent keeps at most the first 1000 and last 5000 lines of a
    // log; the view keeps the same order of size.
    let batch: string[] = [];
    const flush = () => {
      flushTimer = undefined;
      if (cancelled || batch.length === 0) return;
      const add = batch;
      batch = [];
      setLines((prev) => {
        const next = prev.concat(add);
        return next.length > MAX_LINES ? next.slice(next.length - MAX_LINES) : next;
      });
    };
    es.addEventListener("line", (e) => {
      batch.push((e as MessageEvent).data);
      if (flushTimer === undefined) flushTimer = window.setTimeout(flush, 100);
    });
    es.addEventListener("status", (e) => {
      const s = stripQuotes((e as MessageEvent).data);
      setStatus(s as Status);
    });
    es.addEventListener("done", (e) => {
      const s = stripQuotes((e as MessageEvent).data) as Status;
      es.close();
      // The last lines first, so the log is whole when the status changes.
      if (flushTimer !== undefined) window.clearTimeout(flushTimer);
      flush();
      finish(s);
    });
    es.onerror = () => {
      es.close();
      if (!fellBack) {
        fellBack = true;
        // The poll replaces the log wholesale: drop what was still waiting.
        if (flushTimer !== undefined) window.clearTimeout(flushTimer);
        flushTimer = undefined;
        batch = [];
        pollState(0);
      }
    };
    return () => {
      cancelled = true;
      if (pollTimer !== undefined) window.clearTimeout(pollTimer);
      if (flushTimer !== undefined) window.clearTimeout(flushTimer);
      es.close();
    };
  }, [taskId]);

  useEffect(() => {
    if (atBottom.current) boxRef.current?.scrollTo(0, boxRef.current.scrollHeight);
  }, [lines, streamNote]);

  return (
    <div className={styles.wrap}>
      <div className={styles.head}>
        <span className={`${styles.dot} ${styles[status]}`} />
        <span className={styles.label}>Task {taskId.slice(0, 8)}</span>
        <span className={styles.statusText}>{status}</span>
      </div>
      <pre
        ref={boxRef}
        className={styles.log}
        onScroll={(e) => {
          const el = e.currentTarget;
          atBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
        }}
      >
        {[...lines, ...(streamNote ? [streamNote] : [])].join("\n") ||
          "(waiting for output...)"}
      </pre>
    </div>
  );
}

function stripQuotes(s: string): string {
  if (s.startsWith('"') && s.endsWith('"')) return s.slice(1, -1);
  return s;
}
