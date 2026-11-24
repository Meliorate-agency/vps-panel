"use client";

import { useEffect, useRef, useState, useTransition } from "react";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";
import styles from "@/components/page.module.css";
import type { PrivacyPruned, PrivacySaveResult } from "@/server-actions/privacy";
import mail from "./mailIp.module.css";
import { errorText } from "./mailIpParts";
import { RETENTION_PART_IDS } from "./PrivacyLogsForm";
import { PartList, StatusUnavailable, pickParts, usePrivacyStatus } from "./PrivacyStatus";
import local from "./privacy.module.css";

type Action = (days: string, confirmed: boolean) => Promise<PrivacySaveResult>;

const PRESETS = [30, 90, 180, 365];

type Outcome =
  | { kind: "saved"; days: number | null; allInEffect: boolean; pruned: PrivacyPruned | null }
  | { kind: "error"; error: string };

function plural(n: number, one: string, many: string): string {
  return `${n} ${n === 1 ? one : many}`;
}

/** "3 audit log entries, 1 sign-in record and 2 finished tasks", or "". */
function prunedText(p: PrivacyPruned | null): string {
  if (!p) return "";
  const parts = [
    p.audit > 0 ? plural(p.audit, "audit log entry", "audit log entries") : "",
    p.logins > 0 ? plural(p.logins, "sign-in record", "sign-in records") : "",
    p.tasks > 0 ? plural(p.tasks, "finished task", "finished tasks") : "",
  ].filter(Boolean);
  if (parts.length === 0) return "";
  const last = parts.pop();
  return parts.length > 0 ? `${parts.join(", ")} and ${last}` : `${last}`;
}

/**
 * Settings > Privacy > Log retention: "Keep logs for N days" or no limit.
 * A shorter time than the stored one deletes data, so it asks first and only
 * then sends the save with `confirmed`.
 */
export default function PrivacyRetentionForm({ days, action }: { days: number | null; action: Action }) {
  const { status } = usePrivacyStatus();
  const stored = days === null ? "" : String(days);
  const [choice, setChoice] = useState(stored);
  const [pending, start] = useTransition();
  const [outcome, setOutcome] = useState<Outcome | null>(null);
  const [confirming, setConfirming] = useState(false);
  const seq = useRef(0);

  // A save re-renders the page with the stored value; follow it.
  const seen = useRef(stored);
  useEffect(() => {
    if (seen.current === stored) return;
    seen.current = stored;
    setChoice(stored);
  }, [stored]);

  const unavailable = "error" in status;
  const next = choice === "" ? null : Number(choice);
  const lower = next !== null && (days === null || next < days);
  // A stored value that is none of the presets (set over MCP) stays selectable.
  const options = [...new Set([...PRESETS, ...(days === null ? [] : [days])])].sort((a, b) => a - b);

  const submit = (confirmed: boolean) => {
    if (pending || unavailable) return;
    const id = ++seq.current;
    start(async () => {
      let r: PrivacySaveResult;
      try {
        r = await action(choice, confirmed);
      } catch (e: unknown) {
        r = { ok: false, error: errorText(e) };
      }
      if (id !== seq.current) return;
      // In the transition too, so the message and the re-rendered page show together.
      start(() => {
        if (r.ok) {
          const mine = pickParts(r.status, { ids: RETENTION_PART_IDS });
          setOutcome({
            kind: "saved",
            days: next,
            allInEffect: mine.every((p) => p.ok),
            pruned: r.pruned ?? null,
          });
        } else if (r.needsConfirm) {
          // The stored value changed since this page was loaded: ask now.
          setConfirming(true);
        } else {
          setOutcome({ kind: "error", error: r.error });
        }
      });
    });
  };

  const pick = (value: string) => {
    setChoice(value);
    seq.current++;
    setOutcome(null);
  };

  const removed = outcome?.kind === "saved" ? prunedText(outcome.pruned) : "";

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        if (lower) setConfirming(true);
        else submit(false);
      }}
    >
      {unavailable && <StatusUnavailable what="retention" error={status.error} />}

      <fieldset className={local.plain} disabled={pending || unavailable}>
        <div className={local.inline}>
        <label className={`${styles.field} ${local.inlineField}`}>
          Keep logs for
          <select
            className={styles.select}
            name="privacy-retention"
            value={choice}
            onChange={(e) => pick(e.target.value)}
          >
            {options.map((d) => (
              <option key={d} value={String(d)}>
                {d} days
              </option>
            ))}
            <option value="">No limit</option>
          </select>
        </label>
        {/* Enabled with an unchanged choice too: saving again applies it once more. */}
        <button type="submit" className={styles.btn}>
          {pending ? "Saving..." : choice === stored ? "Save again" : "Save"}
        </button>
        </div>
      </fieldset>
      <p className={mail.sub} style={{ marginTop: 8 }}>
        {days === null
          ? "Now: no limit. Nothing is deleted because of its age, apart from what the system and the panel already remove on their own."
          : `Now: ${days} days. Older logs are deleted once a day.`}
      </p>

      <div aria-live="polite">
        {outcome?.kind === "error" && (
          <div className={styles.error} style={{ marginTop: 10 }}>
            {outcome.error}
          </div>
        )}
        {outcome?.kind === "saved" &&
          (outcome.allInEffect ? (
            <div className={styles.notice} style={{ marginTop: 10 }}>
              {outcome.days === null
                ? "Saved. Logs are no longer deleted because of their age."
                : `Saved. Logs are kept for ${outcome.days} days.`}
              {removed && ` The panel deleted ${removed}.`}
            </div>
          ) : (
            <div className={mail.warnBox} role="alert">
              Saved, but the server does not do all of it yet. See below for what is missing.
              {removed && ` The panel deleted ${removed}.`}
            </div>
          ))}
      </div>

      {!unavailable && <PartList parts={pickParts(status, { ids: RETENTION_PART_IDS })} />}

      <ConfirmDialog
        open={confirming && next !== null}
        title={`Delete logs older than ${next} days?`}
        confirmLabel="Delete and save"
        tone="danger"
        onCancel={() => setConfirming(false)}
        onConfirm={() => {
          setConfirming(false);
          submit(true);
        }}
      >
        <p>
          Logs are kept {days === null ? "without a limit" : `for ${days} days`} now. With {next} days,
          everything older is deleted, starting now and then once a day:
        </p>
        <ul>
          <li>the web server&apos;s log files</li>
          <li>the system journal, which holds the mail, SSH and service logs</li>
          <li>the other system logs in /var/log: mail, sign-in, firewall and ban logs</li>
          <li>the panel&apos;s audit log</li>
          <li>the panel&apos;s sign-in records</li>
          <li>the task history with its logs</li>
        </ul>
        <p>Deleted logs cannot be brought back.</p>
      </ConfirmDialog>
    </form>
  );
}
