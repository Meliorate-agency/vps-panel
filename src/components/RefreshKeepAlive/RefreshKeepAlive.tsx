"use client";

import { useEffect } from "react";

const ACTIVITY_EVENTS = ["pointerdown", "keydown", "wheel", "touchstart"] as const;

/**
 * Renews the session when the operator uses the panel, so the session
 * setting works as an idle timeout: it ends that long after the last
 * activity. It used to renew every 50 minutes no matter what, so an
 * untouched tab kept the session alive forever, and a timeout under 50
 * minutes signed the operator out mid-use.
 */
export default function RefreshKeepAlive({ ttlSeconds }: { ttlSeconds: number }) {
  useEffect(() => {
    // At most one renewal per quarter of the lifetime. The first activity
    // after a page load always renews, since the token's age is unknown here.
    const minGapMs = Math.max(30_000, ttlSeconds * 250);
    let last = 0;
    const onActivity = () => {
      const now = Date.now();
      if (now - last < minGapMs) return;
      last = now;
      fetch("/api/auth/refresh", { method: "POST", credentials: "same-origin" }).catch(() => {});
    };
    for (const e of ACTIVITY_EVENTS) window.addEventListener(e, onActivity, { passive: true });
    return () => {
      for (const e of ACTIVITY_EVENTS) window.removeEventListener(e, onActivity);
    };
  }, [ttlSeconds]);
  return null;
}
