"use client";

import { createContext, useContext, useEffect, useRef, useState } from "react";
import StatusBadge from "@/components/StatusBadge/StatusBadge";
import styles from "@/components/page.module.css";
import type { PrivacyPart, PrivacyStatus } from "@/lib/agent";
import type { PrivacyDataResult } from "@/server-actions/privacy";
import mail from "./mailIp.module.css";
import { Busy, errorText } from "./mailIpParts";
import local from "./privacy.module.css";

/** What the agent reports, or why it could not be read. */
export type StatusOrError = PrivacyStatus | { error: string };

type Value = {
  status: StatusOrError;
  checking: boolean;
  /** Asks the server again; the answer replaces `status`. */
  check: () => void;
};

const Context = createContext<Value | null>(null);

/**
 * Holds what the agent reports about the privacy settings for the cards of
 * Settings > Privacy. It starts with what the page read; "Check again" asks
 * once more without reloading the page. When the page re-renders (after a
 * save), what it read then is the newest and takes over.
 */
export function PrivacyStatusProvider({
  initial,
  checkAction,
  children,
}: {
  initial: StatusOrError;
  checkAction: () => Promise<PrivacyDataResult<PrivacyStatus>>;
  children: React.ReactNode;
}) {
  const [status, setStatus] = useState<StatusOrError>(initial);
  const [checking, setChecking] = useState(false);
  // Only the newest answer counts, and none that is older than the page.
  const seq = useRef(0);

  const seen = useRef(initial);
  useEffect(() => {
    if (seen.current === initial) return;
    seen.current = initial;
    seq.current++;
    setStatus(initial);
    setChecking(false);
  }, [initial]);

  const check = async () => {
    if (checking) return;
    const id = ++seq.current;
    setChecking(true);
    let next: StatusOrError;
    try {
      const r = await checkAction();
      next = r.ok ? r.data : { error: r.error };
    } catch (e: unknown) {
      next = { error: errorText(e) };
    }
    if (id !== seq.current) return;
    setStatus(next);
    setChecking(false);
  };

  return <Context.Provider value={{ status, checking, check }}>{children}</Context.Provider>;
}

export function usePrivacyStatus(): Value {
  const value = useContext(Context);
  if (!value) throw new Error("usePrivacyStatus needs a PrivacyStatusProvider");
  return value;
}

export function CheckAgainButton() {
  const { checking, check } = usePrivacyStatus();
  return (
    <button type="button" className={styles.btnGhost} onClick={check} disabled={checking} aria-busy={checking}>
      {checking ? <Busy label="Checking..." /> : "Check again"}
    </button>
  );
}

/** The parts with these ids, or every part with none of `not`. */
export function pickParts(status: PrivacyStatus, pick: { ids: string[] } | { not: string[] }): PrivacyPart[] {
  return status.parts.filter((p) => ("ids" in pick ? pick.ids.includes(p.id) : !pick.not.includes(p.id)));
}

/** The status could not be read: why, and a way to try again. */
export function StatusUnavailable({ what, error }: { what: "settings" | "retention"; error: string }) {
  return (
    <>
      <div className={styles.error} role="alert">
        {what === "settings"
          ? "Could not read the log settings from the server, so they can't be changed here: "
          : "Could not read the log retention from the server, so it can't be changed here: "}
        {error}
      </div>
      <div className={mail.row} style={{ marginBottom: 12 }}>
        <CheckAgainButton />
      </div>
    </>
  );
}

/**
 * "What the server does now": for each part the agent reports, what the
 * settings ask for, what holds, and when the two differ, why. Words on the
 * badge, so the state does not rest on its colour.
 */
export function PartList({ parts }: { parts: PrivacyPart[] }) {
  return (
    <div className={mail.block}>
      <div className={local.holdsHead}>
        <h3 className={mail.blockTitle}>What the server does now</h3>
        <CheckAgainButton />
      </div>
      {parts.length === 0 ? (
        <p className={mail.sub}>The server reported nothing about this.</p>
      ) : (
        <ul className={local.parts}>
          {parts.map((p, i) => (
            <li key={`${p.id}-${i}`} className={local.part}>
              <span className={local.partBadge}>
                {p.ok ? <StatusBadge text="in effect" tone="ok" /> : <StatusBadge text="not in effect" tone="warn" />}
              </span>
              <span className={local.partLabel}>{p.label}</span>
              {p.wanted && <span className={local.partLine}>Chosen: {p.wanted}</span>}
              {p.actual && <span className={local.partLine}>Now: {p.actual}</span>}
              {!p.ok && p.detail && <span className={local.partDetail}>{p.detail}</span>}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
