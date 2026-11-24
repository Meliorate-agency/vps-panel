"use client";

import Link from "next/link";
import { useEffect, useRef, useState, useTransition } from "react";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";
import StatusBadge from "@/components/StatusBadge/StatusBadge";
import styles from "@/components/page.module.css";
import type { DomainIpv6, DomainIpv6Record } from "@/lib/agent";
import type { Ipv6Result } from "@/server-actions/ipv6";
import { Busy, CheckList, errorText } from "../../settings/mailIpParts";
import mail from "../../settings/mailIp.module.css";
import local from "./ipv6Card.module.css";

type ReadAction = (domain: string) => Promise<Ipv6Result<DomainIpv6>>;
type SetAction = (domain: string, enabled: boolean) => Promise<Ipv6Result<DomainIpv6>>;

type Details =
  | { kind: "loading" }
  | { kind: "ok"; data: DomainIpv6; at: string }
  | { kind: "error"; error: string; unsupported: boolean };

type SwitchOutcome = { ok: true; enabled: boolean } | { ok: false; error: string };

/**
 * The IPv6 card on a domain's page: the switch, what on means for the zone
 * (its records, present or missing), whether IPv6 will work (the checks) and
 * the agent's notes. The read does DNS lookups and takes seconds, so the page
 * renders without it and this card asks for it once it is up; the zone list's
 * flag (`initialEnabled`) shows the state until then.
 */
export default function Ipv6Card({
  domain,
  initialEnabled,
  readAction,
  setAction,
}: {
  domain: string;
  /** From the zone list; undefined from an agent older than the switch. */
  initialEnabled: boolean | undefined;
  readAction: ReadAction;
  setAction: SetAction;
}) {
  const [details, setDetails] = useState<Details>({ kind: "loading" });
  const [reading, setReading] = useState(false);
  const readSeq = useRef(0);
  const [switching, startSwitch] = useTransition();
  const [outcome, setOutcome] = useState<SwitchOutcome | null>(null);
  const [confirm, setConfirm] = useState<"on" | "off" | null>(null);
  const switchSeq = useRef(0);
  // Counts switches that succeeded: a read that started before one holds
  // the old state and must not replace the switch's result.
  const switched = useRef(0);

  // Not a transition: the switch must not wait for a slow read.
  const read = async () => {
    const id = ++readSeq.current;
    const before = switched.current;
    setReading(true);
    let next: Details;
    try {
      const r = await readAction(domain);
      next = r.ok
        ? { kind: "ok", data: r.data, at: new Date().toLocaleTimeString() }
        : { kind: "error", error: r.error, unsupported: r.unsupported === true };
    } catch (e: unknown) {
      next = { kind: "error", error: `The check did not finish: ${errorText(e)}`, unsupported: false };
    }
    if (id !== readSeq.current) return;
    if (switched.current === before) setDetails(next);
    setReading(false);
  };

  useEffect(() => {
    void read();
    // Once per domain; "Check again" reads again.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [domain]);

  const data = details.kind === "ok" ? details.data : null;
  const enabled = data ? data.enabled : initialEnabled;
  const unsupported = details.kind === "error" && details.unsupported;
  const noAddress = data !== null && !data.can_enable && !data.enabled;
  const problems = data ? data.checks.filter((c) => c.status !== "ok") : [];

  const run = (target: boolean) => {
    if (switching) return;
    const id = ++switchSeq.current;
    startSwitch(async () => {
      let r: Ipv6Result<DomainIpv6>;
      try {
        r = await setAction(domain, target);
      } catch (e: unknown) {
        r = { ok: false, error: errorText(e) };
      }
      if (id !== switchSeq.current) return;
      // In the transition too, so the message, the new state and the
      // re-rendered records table show together.
      startSwitch(() => {
        setConfirm(null);
        if (r.ok) {
          switched.current++;
          setDetails({ kind: "ok", data: r.data, at: new Date().toLocaleTimeString() });
          setOutcome({ ok: true, enabled: r.data.enabled });
        } else {
          setOutcome({ ok: false, error: r.error });
        }
      });
    });
  };

  const onSwitch = () => {
    setOutcome(null);
    if (enabled) {
      setConfirm("off");
      return;
    }
    // Every check ok: no question to ask.
    if (data && problems.length === 0) run(true);
    else setConfirm("on");
  };

  if (unsupported) {
    return (
      <div className={styles.section} id="ipv6">
        <h2 className={styles.sectionTitle}>IPv6</h2>
        <p className={styles.muted}>{details.error}</p>
      </div>
    );
  }

  const address = data?.address ?? null;

  return (
    <div className={styles.section} id="ipv6">
      <div className={local.head}>
        <h2 className={styles.sectionTitle}>IPv6</h2>
        {enabled === true && <StatusBadge text="on" tone="ok" />}
        {enabled === false && <StatusBadge text="off" tone="muted" />}
      </div>

      <div className={local.top}>
        <div className={local.state}>
          {enabled === undefined ? (
            <span className={local.stateTitle}>
              {details.kind === "error"
                ? `Could not read whether IPv6 is on for ${domain}.`
                : `Reading whether IPv6 is on for ${domain}...`}
            </span>
          ) : enabled ? (
            <>
              <span className={local.stateTitle}>IPv6 is on for {domain}.</span>
              <span className={mail.sub}>
                {address ? (
                  <>
                    Its AAAA records point to <span className={styles.code}>{address}</span>.
                  </>
                ) : (
                  "Its AAAA records point to this server's IPv6 address."
                )}
              </span>
            </>
          ) : (
            <>
              <span className={local.stateTitle}>IPv6 is off for {domain}.</span>
              <span className={mail.sub}>
                {noAddress ? (
                  <>
                    This server has no IPv6 address, so it can&apos;t be switched on.{" "}
                    <Link href="/settings?tab=dns#ipv6">Settings &gt; DNS &amp; SSL &gt; IPv6</Link> says how to
                    get one.
                  </>
                ) : (
                  "The domain is reached over IPv4 only."
                )}
              </span>
            </>
          )}
        </div>
        {enabled !== undefined && (
          <button
            type="button"
            className={enabled ? styles.btnGhost : styles.btn}
            onClick={onSwitch}
            disabled={switching || noAddress}
            aria-busy={switching}
          >
            {switching ? (
              <Busy label={enabled ? "Switching off..." : "Switching on..."} />
            ) : enabled ? (
              "Switch off"
            ) : (
              "Switch on"
            )}
          </button>
        )}
      </div>

      <div aria-live="polite">
        {outcome?.ok === false && (
          <div className={styles.error} style={{ marginTop: 10 }}>
            {outcome.error}
          </div>
        )}
        {outcome?.ok && (
          <div className={styles.notice} style={{ marginTop: 10 }}>
            {outcome.enabled
              ? `IPv6 is on for ${domain}: its records are published.`
              : `IPv6 is off for ${domain}: its IPv6 records are removed.`}
          </div>
        )}
      </div>

      {details.kind === "loading" && (
        <p className={mail.sub} style={{ marginTop: 12 }}>
          <Busy label="Checking IPv6 for this domain. This takes a few seconds..." />
        </p>
      )}

      {details.kind === "error" && (
        <div className={local.block}>
          <div className={styles.error}>Could not check IPv6 for {domain}: {details.error}</div>
          <button type="button" className={styles.btnGhost} onClick={() => void read()} disabled={reading}>
            {reading ? <Busy label="Checking..." /> : "Try again"}
          </button>
        </div>
      )}

      {data && (
        <>
          <div className={local.block}>
            <h3 className={local.blockTitle}>What on means for this domain</h3>
            <p className={local.hint}>
              {data.enabled
                ? "These records point at this server's IPv6 address. Switching off removes them."
                : "Switching on publishes these records; switching off removes them again."}
            </p>
            <RecordList records={data.records} enabled={data.enabled} />
          </div>

          <div className={local.block}>
            <div className={local.blockHead}>
              <h3 className={local.blockTitle}>Will IPv6 work for this domain?</h3>
              <span className={local.blockHead}>
                {!reading && details.kind === "ok" && (
                  <span className={mail.checkedAt}>Checked at {details.at}</span>
                )}
                <button type="button" className={styles.btnGhost} onClick={() => void read()} disabled={reading}>
                  {reading ? <Busy label="Checking..." /> : "Check again"}
                </button>
              </span>
            </div>
            <p className={local.hint}>
              Warnings do not stop you from switching it on, but visitors and mail servers that use IPv6
              may not reach the domain until they are fixed.
            </p>
            <CheckList checks={data.checks} />
          </div>

          {data.notes.length > 0 && (
            <ul className={mail.notes}>
              {data.notes.map((n, i) => (
                <li key={i}>{n}</li>
              ))}
            </ul>
          )}
        </>
      )}

      <ConfirmDialog
        open={confirm === "on"}
        title={`Switch on IPv6 for ${domain}?`}
        confirmLabel="Switch on anyway"
        pending={switching}
        onConfirm={() => run(true)}
        onCancel={() => setConfirm(null)}
      >
        {data ? (
          <>
            <p className={local.dialogText}>
              {problems.length === 1 ? "This check found a problem:" : "These checks found problems:"}
            </p>
            <CheckList checks={problems} />
            <p className={local.dialogText} style={{ marginTop: 12 }}>
              IPv6 can still be switched on. Until this is fixed, visitors and mail servers that use IPv6
              may not reach {domain}.
            </p>
          </>
        ) : (
          <p className={local.dialogText}>
            {details.kind === "error"
              ? `The checks could not be run (${details.error}), so it is not known whether IPv6 will work for ${domain}.`
              : `The checks have not finished yet, so it is not known whether IPv6 will work for ${domain}.`}
          </p>
        )}
      </ConfirmDialog>

      <ConfirmDialog
        open={confirm === "off"}
        title={`Switch off IPv6 for ${domain}?`}
        confirmLabel="Switch off"
        tone="danger"
        pending={switching}
        onConfirm={() => run(false)}
        onCancel={() => setConfirm(null)}
      >
        <RemovedRecords data={data} domain={domain} />
      </ConfirmDialog>
    </div>
  );
}

function RecordList({ records, enabled }: { records: DomainIpv6Record[]; enabled: boolean }) {
  if (records.length === 0) return <p className={mail.sub}>No records.</p>;
  return (
    <ul className={local.records}>
      {records.map((r, i) => (
        <li key={`${r.type}-${r.name}-${i}`} className={local.record}>
          <span className={local.recordBadge}>
            {r.present ? (
              // Present while off: a leftover the operator should know about.
              <StatusBadge text="present" tone={enabled ? "ok" : "warn"} />
            ) : enabled ? (
              <StatusBadge text="missing" tone="warn" />
            ) : (
              <StatusBadge text="absent" tone="muted" />
            )}
          </span>
          <span className={local.recordName}>
            <code>{r.name}</code>
            <span className={local.recordType}>{r.type}</span>
          </span>
          <span className={local.recordContent}>{r.content}</span>
        </li>
      ))}
    </ul>
  );
}

/** The switch-off confirmation: which records go. */
function RemovedRecords({ data, domain }: { data: DomainIpv6 | null; domain: string }) {
  if (!data) {
    return (
      <p className={local.dialogText}>
        The AAAA records of {domain} that point at this server&apos;s IPv6 address are removed, and the
        address is taken out of its SPF record. AAAA records that point elsewhere stay.
      </p>
    );
  }
  const present = data.records.filter((r) => r.present);
  if (present.length === 0) {
    return (
      <p className={local.dialogText}>
        None of this server&apos;s IPv6 records are in the zone now, so nothing is removed. New sites and
        Reconcile zone will not add any.
      </p>
    );
  }
  return (
    <>
      <p className={local.dialogText}>These records are removed from the zone:</p>
      <ul className={local.removeList}>
        {present.map((r, i) => (
          <li key={`${r.type}-${r.name}-${i}`}>
            {r.type === "TXT"
              ? `${r.name} TXT: ${data.address ? `ip6:${data.address}` : "this server's ip6: term"} is taken out of the SPF record`
              : `${r.name} AAAA ${r.content}`}
          </li>
        ))}
      </ul>
      <p className={local.dialogText}>
        The domain is then reached over IPv4 only. AAAA records that point elsewhere stay.
      </p>
    </>
  );
}
