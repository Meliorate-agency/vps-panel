"use client";

import { useActionState, useState } from "react";
import styles from "@/components/page.module.css";
import StatusBadge from "@/components/StatusBadge/StatusBadge";
import Table from "@/components/Table/Table";
import type { MailTrackResp, MailTrackStatus } from "@/lib/agent";
import localStyles from "./mailboxes.module.css";

type Result = { ok: true; data?: MailTrackResp } | { ok: false; error: string };

const STATUS: Record<MailTrackStatus, { label: string; tone: "ok" | "warn" | "danger" | "muted" }> = {
  delivered: { label: "delivered", tone: "ok" },
  sent: { label: "sent on", tone: "ok" },
  deferred: { label: "retrying", tone: "warn" },
  queued: { label: "queued", tone: "warn" },
  bounced: { label: "bounced", tone: "danger" },
  expired: { label: "gave up", tone: "danger" },
  rejected: { label: "rejected", tone: "danger" },
};

/** journalctl short-iso (2026-10-11T14:11:03+0200) as "2026-10-11 14:11:03". */
function when(t: string): string {
  return t.length >= 19 ? `${t.slice(0, 10)} ${t.slice(11, 19)}` : t;
}

/**
 * Track delivery: search Postfix's log for mail to or from one address and
 * show what happened to each message. Read-only.
 */
export default function TrackDelivery({
  action,
}: {
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
}) {
  const [state, formAction, pending] = useActionState<Result | null, FormData>(action, null);
  // Controlled, so the searched address stays in the field: React resets an
  // uncontrolled form after its action runs.
  const [address, setAddress] = useState("");
  const [days, setDays] = useState("3");
  const resp = state?.ok ? state.data : undefined;

  return (
    <div>
      <form action={formAction} className={localStyles.trackForm}>
        <label className={styles.field} style={{ flex: "2 1 220px" }}>
          Address
          <input
            name="address"
            type="email"
            className={styles.input}
            placeholder="jane@example.com"
            value={address}
            onChange={(e) => setAddress(e.target.value)}
            required
          />
        </label>
        <label className={styles.field} style={{ flex: "1 1 120px" }}>
          Period
          <select
            name="days"
            className={styles.input}
            value={days}
            onChange={(e) => setDays(e.target.value)}
          >
            <option value="1">Last day</option>
            <option value="3">Last 3 days</option>
            <option value="7">Last 7 days</option>
            <option value="14">Last 14 days</option>
          </select>
        </label>
        <button type="submit" className={styles.btn} disabled={pending}>
          {pending ? "Searching..." : "Search"}
        </button>
      </form>

      {state?.ok === false && <div className={styles.error}>{state.error}</div>}
      {resp && resp.rows.length === 0 && (
        <p className={localStyles.muted}>
          No mail to or from {resp.address} in the last {resp.days === 1 ? "day" : `${resp.days} days`}.
        </p>
      )}
      {resp && resp.rows.length > 0 && (
        <>
          {resp.truncated && (
            <p className={localStyles.muted}>Showing the newest {resp.rows.length} results.</p>
          )}
          <Table
            scrollX="always"
            layout="fixed"
            columns={[
              { key: "time", label: "Time", width: "150px" },
              { key: "from", label: "From", width: "210px" },
              { key: "to", label: "To", width: "230px" },
              { key: "status", label: "Status", width: "110px" },
              { key: "detail", label: "Details", width: "380px" },
            ]}
            rows={resp.rows.map((r) => ({
              time: <span className={localStyles.trackTime}>{when(r.time)}</span>,
              from: r.from ? (
                <span className={styles.code}>{r.from}</span>
              ) : (
                <span className={localStyles.muted} title="Empty sender: a bounce notice or an automatic reply">
                  automatic
                </span>
              ),
              to: r.to ? (
                <div>
                  <span className={styles.code}>{r.to}</span>
                  {r.orig_to && r.orig_to !== r.to && (
                    <div className={localStyles.hint}>sent to {r.orig_to}</div>
                  )}
                </div>
              ) : (
                <span className={localStyles.muted}>-</span>
              ),
              status: <StatusBadge text={STATUS[r.status]?.label ?? r.status} tone={STATUS[r.status]?.tone ?? "muted"} />,
              detail: <span className={localStyles.trackDetail}>{r.detail}</span>,
            }))}
          />
        </>
      )}
    </div>
  );
}
