"use client";

import { useEffect, useState } from "react";
import { useRouter } from "next/navigation";
import StatusBadge from "@/components/StatusBadge/StatusBadge";

/**
 * "issuing..." badge that keeps the row honest without a manual reload.
 *
 * The cert is issued by an agent task; the panel's sslReconciler flips the
 * `sites` row when that task settles and revalidates /domains, but nothing
 * pushes that to an already-open tab. So while this badge is mounted it asks
 * the router to re-render the page every few seconds. As soon as the server
 * renders a real badge (or a Retry button) this component unmounts and the
 * polling stops with it. Capped at the reconciler's own 15 minute window so
 * a stuck task can never keep a tab refreshing forever.
 */
const POLL_MS = 5_000;
const MAX_MS = 15 * 60 * 1000;

export default function SslIssuingBadge({ label = "issuing..." }: { label?: string }) {
  const router = useRouter();
  const [stale, setStale] = useState(false);

  useEffect(() => {
    const started = Date.now();
    const id = window.setInterval(() => {
      if (Date.now() - started > MAX_MS) {
        window.clearInterval(id);
        setStale(true);
        return;
      }
      router.refresh();
    }, POLL_MS);
    return () => window.clearInterval(id);
  }, [router]);

  return (
    <StatusBadge
      text={stale ? `${label} (taking long, reload to check)` : label}
      tone="warn"
    />
  );
}
