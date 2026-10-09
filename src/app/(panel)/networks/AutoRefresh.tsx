"use client";

import { useEffect } from "react";
import { useRouter } from "next/navigation";

/** Re-reads the page on the heartbeat's pace while the tab is visible. Does
 *  not count as activity, so the session still times out when idle. */
export default function AutoRefresh({ everyMs }: { everyMs: number }) {
  const router = useRouter();
  useEffect(() => {
    const id = window.setInterval(() => {
      if (document.visibilityState === "visible") router.refresh();
    }, everyMs);
    return () => window.clearInterval(id);
  }, [router, everyMs]);
  return null;
}
