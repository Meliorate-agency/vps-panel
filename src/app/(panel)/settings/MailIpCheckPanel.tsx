"use client";

import { useRef, useState } from "react";
import StatusBadge from "@/components/StatusBadge/StatusBadge";
import styles from "@/components/page.module.css";
import type { MailIpCheck, MailIpFamilyCheck, MailIpProtocol } from "@/lib/agent";
import type { MailIpReadResult } from "@/server-actions/mail";
import local from "./mailIp.module.css";
import { Busy, CheckList, PROTOCOL_NAME, errorText } from "./mailIpParts";

type Result = { ok: true; data: MailIpCheck; at: string } | { ok: false; error: string };

/**
 * "Check both protocols": the agent's live check of whether mail can be sent
 * from each address (reverse DNS and so on). Slow, so it only runs on a click.
 * Not a transition: a row Save elsewhere on the page must not wait for it.
 */
export default function MailIpCheckPanel({
  action,
}: {
  action: () => Promise<MailIpReadResult<MailIpCheck>>;
}) {
  const [running, setRunning] = useState(false);
  const [result, setResult] = useState<Result | null>(null);
  const seq = useRef(0);

  const run = async () => {
    if (running) return;
    const id = ++seq.current;
    setRunning(true);
    let next: Result;
    try {
      const r = await action();
      next = r.ok
        ? { ok: true, data: r.data, at: new Date().toLocaleTimeString() }
        : { ok: false, error: r.error };
    } catch (e: unknown) {
      next = { ok: false, error: `The check did not finish: ${errorText(e)}` };
    }
    if (id !== seq.current) return;
    setResult(next);
    setRunning(false);
  };

  return (
    <div>
      <div className={local.row}>
        <button
          type="button"
          className={styles.btnGhost}
          onClick={run}
          disabled={running}
          aria-busy={running}
        >
          {running ? <Busy label="Checking..." /> : "Check both protocols"}
        </button>
        {result?.ok && !running && <span className={local.checkedAt}>Checked at {result.at}</span>}
      </div>
      <div aria-live="polite">
        {result?.ok === false && (
          <div className={styles.error} style={{ marginTop: 10 }}>
            {result.error}
          </div>
        )}
        {result?.ok && (
          <>
            {result.data.helo_host && (
              <p className={local.sub} style={{ marginTop: 10 }}>
                This server introduces itself to other mail servers as{" "}
                <span className={styles.code}>{result.data.helo_host}</span>.
              </p>
            )}
            <div className={local.families}>
              <Family protocol="ipv4" check={result.data.ipv4} />
              <Family protocol="ipv6" check={result.data.ipv6} />
            </div>
          </>
        )}
      </div>
    </div>
  );
}

function Family({ protocol, check }: { protocol: MailIpProtocol; check: MailIpFamilyCheck }) {
  const name = PROTOCOL_NAME[protocol];
  return (
    <section className={local.family} aria-label={name}>
      <div className={local.familyHead}>
        <span className={local.familyName}>{name}</span>
        {check.address ? (
          <span className={styles.code}>{check.address}</span>
        ) : (
          <span className={local.sub}>no public address</span>
        )}
        {check.ready ? (
          <StatusBadge text="ready to send" tone="ok" />
        ) : check.address ? (
          <StatusBadge text="not ready" tone="danger" />
        ) : (
          <StatusBadge text="not available" tone="muted" />
        )}
      </div>
      <CheckList checks={check.checks} />
    </section>
  );
}
