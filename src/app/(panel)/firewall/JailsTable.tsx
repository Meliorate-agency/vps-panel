"use client";

import { useState, useTransition } from "react";
import ConfirmSubmitButton from "@/components/ConfirmSubmitButton/ConfirmSubmitButton";
import styles from "@/components/page.module.css";
import localStyles from "./jails.module.css";
import type { JailRow, FailedByIp } from "@/lib/agent";

type FailedState =
  | { kind: "idle" }
  | { kind: "loading" }
  | { kind: "loaded"; rows: FailedByIp[] }
  | { kind: "error"; message: string };

type Props = {
  jails: JailRow[];
  /** Pre-bound by jail in the parent: `unbanIpAction.bind(null, jail, ip)`. */
  unbanIpAction: (jail: string, ip: string) => Promise<void>;
  /** Bulk unban + restart fail2ban for the given jail. */
  unbanAllAction: (jail: string) => Promise<void>;
  /** Reads /var/log/fail2ban.log and counts attempts per IP for the jail. */
  fetchFailedByIp: (jail: string) => Promise<FailedByIp[]>;
};

export default function JailsTable({
  jails,
  unbanIpAction,
  unbanAllAction,
  fetchFailedByIp,
}: Props) {
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [data, setData] = useState<Map<string, FailedState>>(new Map());
  const [, startFetch] = useTransition();

  function toggle(name: string) {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(name)) {
        next.delete(name);
      } else {
        next.add(name);
        // Lazy-load on first expand; subsequent expands keep cached data
        // until the user explicitly hits Refresh below.
        if (!data.has(name) || data.get(name)?.kind === "error") {
          loadJail(name);
        }
      }
      return next;
    });
  }

  function loadJail(name: string) {
    setData((prev) => new Map(prev).set(name, { kind: "loading" }));
    startFetch(async () => {
      try {
        const rows = await fetchFailedByIp(name);
        setData((prev) => new Map(prev).set(name, { kind: "loaded", rows }));
      } catch (e) {
        setData((prev) =>
          new Map(prev).set(name, {
            kind: "error",
            message: e instanceof Error ? e.message : String(e),
          }),
        );
      }
    });
  }

  if (jails.length === 0) {
    return <div className={styles.empty}>No jails configured.</div>;
  }

  return (
    <div className={localStyles.list}>
      {jails.map((j) => {
        const isOpen = expanded.has(j.name);
        const state: FailedState = data.get(j.name) ?? { kind: "idle" };
        return (
          <div key={j.name} className={localStyles.jail}>
            <div className={localStyles.head}>
              <div className={localStyles.headLeft}>
                <button
                  type="button"
                  className={localStyles.toggle}
                  onClick={() => toggle(j.name)}
                  aria-expanded={isOpen}
                  aria-label={isOpen ? "Collapse" : "Expand"}
                >
                  <span className={isOpen ? localStyles.chevronOpen : localStyles.chevronClosed}>
                    ▸
                  </span>
                </button>
                <strong className={localStyles.name}>{j.name}</strong>
                <span className={localStyles.failedBadge} title="Total `Found` matches in fail2ban.log">
                  {j.total_failed} failed
                </span>
              </div>
              <div className={localStyles.headRight}>
                {j.banned.length > 0 ? (
                  <>
                    <span className={styles.muted}>
                      {j.banned.length} banned
                    </span>
                    <form action={unbanAllAction.bind(null, j.name)}>
                      <ConfirmSubmitButton
                        className={styles.btnDanger}
                        message={`Unban ALL ${j.banned.length} IP${j.banned.length === 1 ? "" : "s"} in jail "${j.name}" and restart fail2ban?\n\nThis is destructive — every currently-banned IP will be released immediately, and the in-memory failure counters will reset. Use when you've fixed a misconfigured client that caused an accidental lockout.`}
                        pendingLabel="Unbanning..."
                      >
                        Unban all + restart
                      </ConfirmSubmitButton>
                    </form>
                  </>
                ) : (
                  <span className={styles.muted}>no bans</span>
                )}
              </div>
            </div>

            {j.banned.length > 0 && (
              <div className={localStyles.banList}>
                {j.banned.map((ip) => (
                  <form key={ip} action={unbanIpAction.bind(null, j.name, ip)}>
                    <ConfirmSubmitButton
                      className={styles.btnDanger}
                      message={`Unban ${ip} from ${j.name}?\n\nThe IP can immediately retry whatever got it banned.`}
                    >
                      {ip} unban
                    </ConfirmSubmitButton>
                  </form>
                ))}
              </div>
            )}

            {isOpen && (
              <div className={localStyles.expand}>
                <div className={localStyles.expandHead}>
                  <span className={styles.muted}>Per-IP failed attempts (last log rotation)</span>
                  <button
                    type="button"
                    className={styles.btnGhost}
                    onClick={() => loadJail(j.name)}
                  >
                    Refresh
                  </button>
                </div>
                <FailedTable state={state} />
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}

function FailedTable({ state }: { state: FailedState }) {
  if (state.kind === "idle" || state.kind === "loading") {
    return <div className={styles.muted}>Loading...</div>;
  }
  if (state.kind === "error") {
    return <div className={styles.error}>Could not load: {state.message}</div>;
  }
  if (state.rows.length === 0) {
    return <div className={styles.muted}>No `Found` entries in fail2ban.log for this jail.</div>;
  }
  return (
    <table className={localStyles.failedTable}>
      <thead>
        <tr>
          <th>IP</th>
          <th style={{ width: 100 }}>Attempts</th>
          <th style={{ width: 180 }}>Last seen</th>
        </tr>
      </thead>
      <tbody>
        {state.rows.map((r) => (
          <tr key={r.ip}>
            <td><span className={styles.code}>{r.ip}</span></td>
            <td>{r.attempts}</td>
            <td><span className={styles.muted}>{r.last_seen}</span></td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}
