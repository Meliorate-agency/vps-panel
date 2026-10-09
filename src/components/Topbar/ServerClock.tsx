"use client";

import { useEffect, useMemo, useState } from "react";
import styles from "./Topbar.module.css";

/**
 * Live wall-clock for the server, rendered in the operator-chosen timezone
 * (set via /settings → Panel timezone, default UTC). Server renders the
 * initial tick value, client takes over with `setInterval(1000)`. Drift
 * offset against the client's clock so the displayed time still tracks
 * the server when the operator's laptop is skewed or in a different TZ.
 *
 * Format: `Wed 27 May, 16:42:03 CEST`. No "live indicator" pulse — the
 * seconds ticking is sufficient visual feedback.
 */
export default function ServerClock({
  initialServerMs,
  timezone,
}: {
  initialServerMs: number;
  timezone: string;
}) {
  const [now, setNow] = useState<Date | null>(null);

  // Rebuild formatters when the timezone setting changes (operator saved a
  // new tz; layout re-renders and passes the new prop in).
  const fmts = useMemo(
    () => ({
      time: new Intl.DateTimeFormat("en-GB", {
        timeZone: timezone,
        hour: "2-digit",
        minute: "2-digit",
        second: "2-digit",
        hour12: false,
      }),
      date: new Intl.DateTimeFormat("en-GB", {
        timeZone: timezone,
        weekday: "short",
        day: "numeric",
        month: "short",
      }),
      zone: new Intl.DateTimeFormat("en-GB", {
        timeZone: timezone,
        timeZoneName: "short",
      }),
    }),
    [timezone],
  );

  useEffect(() => {
    const offset = initialServerMs - Date.now();
    const tick = () => setNow(new Date(Date.now() + offset));
    tick();
    const id = setInterval(tick, 1000);
    return () => clearInterval(id);
  }, [initialServerMs]);

  // Skip the first server-render frame to avoid a hydration mismatch (the
  // server can't predict client tick timing). One frame of empty is fine.
  if (!now) return <span className={styles.serverClock} aria-hidden />;

  const zone = zoneAbbr(fmts.zone, now);
  return (
    <span
      className={styles.serverClock}
      title={`Server time (${timezone} · ${zone})`}
    >
      <span className={styles.serverClockDate}>{fmts.date.format(now)}</span>
      <span className={styles.serverClockTime}>{fmts.time.format(now)}</span>
      <span className={styles.serverClockZone}>{zone}</span>
    </span>
  );
}

function zoneAbbr(fmt: Intl.DateTimeFormat, d: Date): string {
  // `.format` returns something like "27/05/2026, CEST". Pull out the last
  // token (the tz abbreviation) regardless of locale variation.
  const parts = fmt.formatToParts(d);
  return parts.find((p) => p.type === "timeZoneName")?.value ?? "";
}
