"use client";

import { useEffect, useRef, useState } from "react";
import styles from "./TaskProgress.module.css";

type Status = "pending" | "running" | "succeeded" | "failed";

type Props = {
  taskId: string;
  onComplete?: (status: "succeeded" | "failed") => void;
};

export default function TaskProgress({ taskId, onComplete }: Props) {
  const [lines, setLines] = useState<string[]>([]);
  const [status, setStatus] = useState<Status>("pending");
  // Set only when both the stream AND the polling fallback are unreachable -
  // rendered in place of "(waiting for output...)" so the UI never pretends
  // a task is pending when we simply lost sight of it.
  const [streamNote, setStreamNote] = useState<string | null>(null);
  const boxRef = useRef<HTMLPreElement>(null);
  const onCompleteRef = useRef(onComplete);
  onCompleteRef.current = onComplete;

  useEffect(() => {
    let cancelled = false;
    let fellBack = false;
    let pollTimer: number | undefined;
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

    es.addEventListener("line", (e) => {
      setLines((prev) => [...prev, (e as MessageEvent).data]);
    });
    es.addEventListener("status", (e) => {
      const s = stripQuotes((e as MessageEvent).data);
      setStatus(s as Status);
    });
    es.addEventListener("done", (e) => {
      const s = stripQuotes((e as MessageEvent).data) as Status;
      es.close();
      finish(s);
    });
    es.onerror = () => {
      es.close();
      if (!fellBack) {
        fellBack = true;
        pollState(0);
      }
    };
    return () => {
      cancelled = true;
      if (pollTimer !== undefined) window.clearTimeout(pollTimer);
      es.close();
    };
  }, [taskId]);

  useEffect(() => {
    boxRef.current?.scrollTo(0, boxRef.current.scrollHeight);
  }, [lines]);

  return (
    <div className={styles.wrap}>
      <div className={styles.head}>
        <span className={`${styles.dot} ${styles[status]}`} />
        <span className={styles.label}>Task {taskId.slice(0, 8)}</span>
        <span className={styles.statusText}>{status}</span>
      </div>
      <pre ref={boxRef} className={styles.log}>
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
