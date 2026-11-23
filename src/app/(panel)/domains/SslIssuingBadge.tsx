"use client";

import { useEffect, useState } from "react";
import { useRouter } from "next/navigation";
import StatusBadge from "@/components/StatusBadge/StatusBadge";

/**
 * "issuing..." badge that keeps the row honest without a manual reload.
 *
 * The cert is issued by an agent task; the panel's sslReconciler flips the
 * `sites` row when that task settles and revalidates /domains, but nothing
 * pushes that to an already-open tab. So while this badge is mounted the
 * page is re-rendered every few seconds. As soon as the server renders a
 * real badge (or a Retry button) this component unmounts. Each badge gives
 * up after the reconciler's own 15 minute window so a stuck task can never
 * keep a tab refreshing forever.
 *
 * All badges on the page share one timer: one refresh per interval however
 * many rows are issuing. With a timer each, N rows refreshed the page N times
 * per interval, and every refresh asks the agent about every zone.
 */
const POLL_MS = 5_000;
const MAX_MS = 15 * 60 * 1000;

type Waiter = { started: number; refresh: () => void; giveUp: () => void };

const waiters = new Set<Waiter>();
let timer: number | null = null;

function stopIfIdle() {
  if (waiters.size === 0 && timer !== null) {
    window.clearInterval(timer);
    timer = null;
  }
}

function tick() {
  const now = Date.now();
  let refresh: (() => void) | null = null;
  for (const w of waiters) {
    if (now - w.started > MAX_MS) {
      waiters.delete(w);
      w.giveUp();
    } else {
      refresh = w.refresh;
    }
  }
  stopIfIdle();
  refresh?.();
}

function wait(w: Waiter): () => void {
  waiters.add(w);
  if (timer === null) timer = window.setInterval(tick, POLL_MS);
  return () => {
    waiters.delete(w);
    stopIfIdle();
  };
}

export default function SslIssuingBadge({ label = "issuing..." }: { label?: string }) {
  const router = useRouter();
  const [stale, setStale] = useState(false);

  useEffect(
    () =>
      wait({
        started: Date.now(),
        refresh: () => router.refresh(),
        giveUp: () => setStale(true),
      }),
    [router],
  );

  return (
    <StatusBadge
      text={stale ? `${label} (taking long, reload to check)` : label}
      tone="warn"
    />
  );
}
