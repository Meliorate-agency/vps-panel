"use client";

import { useActionState } from "react";
import styles from "@/components/page.module.css";

type Result = { ok: true } | { ok: false; error: string };

type InitialPolicy = {
  bantime: string;
  findtime: string;
  maxretry: number;
  jails: string[];
};

// Mirrors agent/src/handlers/firewall.rs::KNOWN_JAILS. The wizard uses the
// same list; keep them in sync. Unchecking writes `enabled = false` to the
// drop-in conf, so the operator can disable a jail in one click instead of
// editing files by hand.
const ALL_JAILS = ["sshd", "postfix-sasl", "dovecot"];

export default function Fail2banPolicyForm({
  action,
  initial,
}: {
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
  initial: InitialPolicy;
}) {
  const [state, formAction, pending] = useActionState<Result | null, FormData>(
    action,
    null,
  );

  return (
    <form
      action={formAction}
      style={{ display: "flex", flexDirection: "column", gap: 12 }}
    >
      <div className={styles.formRow}>
        <label className={styles.field}>
          Ban time
          <input
            name="bantime"
            className={styles.input}
            defaultValue={initial.bantime}
            placeholder="1h"
            required
          />
          <span className={styles.muted} style={{ fontSize: 12, marginTop: 4 }}>
            How long a banned IP stays blocked. Suffix:{" "}
            <span className={styles.code}>s</span>/<span className={styles.code}>m</span>/
            <span className={styles.code}>h</span>/<span className={styles.code}>d</span>/
            <span className={styles.code}>w</span>. e.g.{" "}
            <span className={styles.code}>30m</span>,{" "}
            <span className={styles.code}>1h</span>,{" "}
            <span className={styles.code}>1d</span>.
          </span>
        </label>
        <label className={styles.field}>
          Find time
          <input
            name="findtime"
            className={styles.input}
            defaultValue={initial.findtime}
            placeholder="10m"
            required
          />
          <span className={styles.muted} style={{ fontSize: 12, marginTop: 4 }}>
            Window in which failed attempts are counted. Same suffix format.
          </span>
        </label>
        <label className={styles.field}>
          Max retries
          <input
            name="maxretry"
            type="number"
            min={1}
            max={1000}
            className={styles.input}
            defaultValue={initial.maxretry}
            required
          />
          <span className={styles.muted} style={{ fontSize: 12, marginTop: 4 }}>
            How many failures inside the find-time window before a ban.
            Default 5 — bump to 10+ if you keep locking yourself out while
            juggling SSH keys.
          </span>
        </label>
      </div>

      <div className={styles.field}>
        Jails to enable
        <div style={{ display: "flex", gap: 16, flexWrap: "wrap", marginTop: 4 }}>
          {ALL_JAILS.map((jail) => (
            <label key={jail} className={styles.checkbox}>
              <input
                type="checkbox"
                name="jails"
                value={jail}
                defaultChecked={initial.jails.includes(jail)}
              />
              {jail}
            </label>
          ))}
        </div>
        <span className={styles.muted} style={{ fontSize: 12, marginTop: 6, display: "block" }}>
          Uncheck a jail to fully disable it. The thresholds above apply to
          every checked jail (Dovecot has its own override for IMAP IDLE
          quirks — left untouched by this form).
        </span>
      </div>

      {state?.ok === false && <div className={styles.error}>{state.error}</div>}
      {state?.ok === true && (
        <div className={styles.notice}>fail2ban policy applied.</div>
      )}
      <div>
        <button type="submit" className={styles.btn} disabled={pending}>
          {pending ? "Saving..." : "Save policy"}
        </button>
      </div>
    </form>
  );
}
