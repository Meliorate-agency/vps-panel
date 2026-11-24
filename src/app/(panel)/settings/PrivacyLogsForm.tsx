"use client";

import { useEffect, useRef, useState, useTransition } from "react";
import styles from "@/components/page.module.css";
import type { PrivacySaveResult } from "@/server-actions/privacy";
import mail from "./mailIp.module.css";
import { errorText } from "./mailIpParts";
import { PartList, StatusUnavailable, pickParts, usePrivacyStatus } from "./PrivacyStatus";

type LogIp = "full" | "masked";
type LogQuery = "keep" | "strip";
type Action = (logIp: string, logQuery: string) => Promise<PrivacySaveResult>;

/** The parts of the agent's report that belong to the retention card. */
export const RETENTION_PART_IDS = ["journal", "nginx_log_files"];

type Outcome =
  | { kind: "saved"; allInEffect: boolean }
  | { kind: "error"; error: string };

const IP_OPTIONS: { value: LogIp; title: string; text: React.ReactNode }[] = [
  {
    value: "full",
    title: "Full",
    text: "The visitor's whole IP address is written to the log.",
  },
  {
    value: "masked",
    title: "Masked",
    text: (
      <>
        The end of the address is cut off: <span className={styles.code}>203.0.113.87</span> is logged as{" "}
        <span className={styles.code}>203.0.113.0</span>, and{" "}
        <span className={styles.code}>2001:db8:85a3:8d3::7</span> as{" "}
        <span className={styles.code}>2001:db8:85a3::</span>.
      </>
    ),
  },
];

const QUERY_OPTIONS: { value: LogQuery; title: string; text: React.ReactNode }[] = [
  {
    value: "keep",
    title: "Kept",
    text: (
      <>
        Addresses are logged as they were requested, e.g.{" "}
        <span className={styles.code}>/search?q=shoes&amp;email=ann@example.com</span>.
      </>
    ),
  },
  {
    value: "strip",
    title: "Left out",
    text: (
      <>
        Everything from the <span className={styles.code}>?</span> on is dropped, in the requested address and in
        the referrer: <span className={styles.code}>/search</span>.
      </>
    ),
  },
];

/**
 * Settings > Privacy > Website logs: whether the web server's access logs
 * hold whole visitor addresses and query strings. One Save for both; under
 * it, what the agent says the server really does.
 */
export default function PrivacyLogsForm({
  logIp,
  logQuery,
  action,
}: {
  logIp: LogIp;
  logQuery: LogQuery;
  action: Action;
}) {
  const { status } = usePrivacyStatus();
  const [ip, setIp] = useState<LogIp>(logIp);
  const [query, setQuery] = useState<LogQuery>(logQuery);
  const [pending, start] = useTransition();
  const [outcome, setOutcome] = useState<Outcome | null>(null);
  // Only the newest request may set the outcome.
  const seq = useRef(0);

  // A save re-renders the page with the stored values but keeps this
  // instance: follow them when they change.
  const seen = useRef(`${logIp}|${logQuery}`);
  useEffect(() => {
    const now = `${logIp}|${logQuery}`;
    if (seen.current === now) return;
    seen.current = now;
    setIp(logIp);
    setQuery(logQuery);
  }, [logIp, logQuery]);

  const unavailable = "error" in status;
  const unchanged = ip === logIp && query === logQuery;

  const submit = () => {
    if (pending || unavailable) return;
    const id = ++seq.current;
    start(async () => {
      let r: PrivacySaveResult;
      try {
        r = await action(ip, query);
      } catch (e: unknown) {
        r = { ok: false, error: errorText(e) };
      }
      if (id !== seq.current) return;
      // In the transition too, so the message and the re-rendered page show together.
      start(() => {
        if (r.ok) {
          const mine = pickParts(r.status, { not: RETENTION_PART_IDS });
          setOutcome({ kind: "saved", allInEffect: mine.every((p) => p.ok) });
        } else {
          setOutcome({ kind: "error", error: r.error });
        }
      });
    });
  };

  const changed = <T,>(set: (v: T) => void, v: T) => {
    set(v);
    // A message about the earlier choice no longer applies.
    seq.current++;
    setOutcome(null);
  };

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        submit();
      }}
    >
      {unavailable && <StatusUnavailable what="settings" error={status.error} />}

      <fieldset className={mail.choices} disabled={pending || unavailable}>
        <legend className={mail.legend}>Visitor addresses</legend>
        {IP_OPTIONS.map((o) => (
          <label key={o.value} className={mail.choice}>
            <input
              type="radio"
              name="privacy-log-ip"
              value={o.value}
              checked={ip === o.value}
              onChange={() => changed(setIp, o.value)}
            />
            <span className={mail.choiceText}>
              <span className={mail.choiceTitle}>{o.title}</span>
              <span className={mail.sub}>{o.text}</span>
              {o.value === logIp && <span className={mail.sub}>Chosen now</span>}
            </span>
          </label>
        ))}
      </fieldset>

      <fieldset className={mail.choices} disabled={pending || unavailable}>
        <legend className={mail.legend}>Query strings</legend>
        {QUERY_OPTIONS.map((o) => (
          <label key={o.value} className={mail.choice}>
            <input
              type="radio"
              name="privacy-log-query"
              value={o.value}
              checked={query === o.value}
              onChange={() => changed(setQuery, o.value)}
            />
            <span className={mail.choiceText}>
              <span className={mail.choiceTitle}>{o.title}</span>
              <span className={mail.sub}>{o.text}</span>
              {o.value === logQuery && <span className={mail.sub}>Chosen now</span>}
            </span>
          </label>
        ))}
      </fieldset>

      <div className={mail.row}>
        {/* Enabled with unchanged choices too: saving again is how a part
            that is not in effect is applied once more. */}
        <button type="submit" className={styles.btn} disabled={pending || unavailable}>
          {pending ? "Saving..." : unchanged ? "Save again" : "Save"}
        </button>
      </div>

      <div aria-live="polite">
        {outcome?.kind === "error" && (
          <div className={styles.error} style={{ marginTop: 10 }}>
            {outcome.error}
          </div>
        )}
        {outcome?.kind === "saved" &&
          (outcome.allInEffect ? (
            <div className={styles.notice} style={{ marginTop: 10 }}>
              Saved. New log lines are written this way.
            </div>
          ) : (
            <div className={mail.warnBox} role="alert">
              Saved, but the server does not do all of it yet. See below for what is missing.
            </div>
          ))}
      </div>

      {!unavailable && (
        <>
          <PartList parts={pickParts(status, { not: RETENTION_PART_IDS })} />
          {status.notes.length > 0 && (
            <ul className={mail.notes}>
              {status.notes.map((n, i) => (
                <li key={i}>{n}</li>
              ))}
            </ul>
          )}
        </>
      )}
    </form>
  );
}
