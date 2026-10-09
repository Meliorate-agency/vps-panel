"use client";

import { useActionState, useEffect, useState } from "react";
import styles from "@/components/page.module.css";

type Result = { ok: true } | { ok: false; error: string };

// Curated list of common IANA zones. Operators can still type any other
// IANA name into the input — the server validates via Intl.
const COMMON_ZONES = [
  "UTC",
  "Europe/Berlin",
  "Europe/London",
  "Europe/Paris",
  "Europe/Madrid",
  "Europe/Rome",
  "Europe/Amsterdam",
  "Europe/Warsaw",
  "Europe/Athens",
  "Europe/Moscow",
  "America/New_York",
  "America/Chicago",
  "America/Denver",
  "America/Los_Angeles",
  "America/Sao_Paulo",
  "America/Toronto",
  "Asia/Tokyo",
  "Asia/Shanghai",
  "Asia/Singapore",
  "Asia/Hong_Kong",
  "Asia/Seoul",
  "Asia/Dubai",
  "Asia/Kolkata",
  "Australia/Sydney",
  "Australia/Melbourne",
  "Pacific/Auckland",
];

export default function TimezoneForm({
  value,
  action,
}: {
  value: string;
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
}) {
  const [state, formAction, pending] = useActionState<Result | null, FormData>(
    action,
    null,
  );
  const [tz, setTz] = useState(value);
  const [browserTz, setBrowserTz] = useState<string | null>(null);
  const [now, setNow] = useState<Date | null>(null);

  useEffect(() => {
    const tick = () => setNow(new Date());
    tick();
    const id = setInterval(tick, 1000);
    return () => clearInterval(id);
  }, []);

  useEffect(() => {
    try {
      setBrowserTz(Intl.DateTimeFormat().resolvedOptions().timeZone);
    } catch {
      // Some old browsers don't expose .timeZone — fine, leave it null.
    }
  }, []);

  // Sync local state with the server's saved value after revalidatePath
  // (otherwise the input stays on whatever the user typed last, including
  // an invalid value that the server rejected).
  useEffect(() => {
    setTz(value);
  }, [value]);

  const previewTime = (() => {
    if (!now) return null;
    try {
      return new Intl.DateTimeFormat("en-GB", {
        timeZone: tz,
        weekday: "short",
        day: "numeric",
        month: "short",
        hour: "2-digit",
        minute: "2-digit",
        second: "2-digit",
        hour12: false,
        timeZoneName: "short",
      }).format(now);
    } catch {
      return null;
    }
  })();

  return (
    <form action={formAction}>
      <div className={styles.formRow}>
        <label className={styles.field}>
          Timezone
          <input
            name="timezone"
            list="timezone-list"
            className={styles.input}
            value={tz}
            onChange={(e) => setTz(e.target.value)}
            placeholder="UTC"
            required
            autoComplete="off"
          />
          <datalist id="timezone-list">
            {COMMON_ZONES.map((z) => (
              <option key={z} value={z} />
            ))}
          </datalist>
          <span className={styles.muted} style={{ fontSize: 12, marginTop: 4 }}>
            IANA timezone name. Pick from the dropdown or type any valid one
            (e.g. <span className={styles.code}>Africa/Cairo</span>). Saved
            value is used for the top-bar clock and the audit log&apos;s day
            grouping.
          </span>
        </label>
      </div>

      <div
        style={{
          marginBottom: 12,
          padding: "10px 12px",
          background: "var(--bg-elev-2)",
          border: "1px solid var(--border)",
          borderRadius: 6,
          fontSize: 13,
          display: "flex",
          flexDirection: "column",
          gap: 4,
        }}
      >
        <div>
          <span className={styles.muted}>Right now in </span>
          <span className={styles.code}>{tz}</span>
          {now &&
            (previewTime ? (
              <>
                {": "}
                <strong>{previewTime}</strong>
              </>
            ) : (
              <span className={styles.error}>
                {" - invalid IANA zone, won't be accepted"}
              </span>
            ))}
        </div>
        {browserTz && browserTz !== tz && (
          <div className={styles.muted} style={{ fontSize: 12 }}>
            Your browser&apos;s tz is <span className={styles.code}>{browserTz}</span>.{" "}
            <button
              type="button"
              onClick={() => setTz(browserTz)}
              style={{
                background: "transparent",
                border: 0,
                color: "var(--accent)",
                cursor: "pointer",
                padding: 0,
                font: "inherit",
                textDecoration: "underline",
              }}
            >
              Use that
            </button>
          </div>
        )}
      </div>

      {state?.ok === false && <div className={styles.error}>{state.error}</div>}
      {state?.ok === true && (
        <div className={styles.notice}>
          Timezone saved. The clock and audit log will use it on the next
          render.
        </div>
      )}
      <button type="submit" className={styles.btn} disabled={pending}>
        {pending ? "Saving..." : "Save timezone"}
      </button>
    </form>
  );
}
