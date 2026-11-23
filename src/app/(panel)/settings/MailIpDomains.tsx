"use client";

import Link from "next/link";
import { useEffect, useRef, useState, useTransition } from "react";
import styles from "@/components/page.module.css";
import type {
  MailDomainDnsCheck,
  MailIpDomain,
  MailIpPreference,
  MailIpProtocol,
} from "@/lib/agent";
import type { MailIpReadResult, MailIpWriteResult } from "@/server-actions/mail";
import local from "./mailIp.module.css";
import { Busy, CheckList, ForceConfirm, PROTOCOL_NAME, errorText, hasProblems } from "./mailIpParts";

type SaveAction = (
  domain: string,
  preference: string,
  force: boolean,
) => Promise<MailIpWriteResult<MailIpDomain>>;
type CheckAction = (domain: string) => Promise<MailIpReadResult<MailDomainDnsCheck>>;

type SaveOutcome =
  | { kind: "saved"; effective: MailIpProtocol }
  | { kind: "error"; error: string }
  | { kind: "force"; error: string; preference: MailIpPreference };

type CheckOutcome =
  | { ok: true; data: MailDomainDnsCheck; at: string }
  | { ok: false; error: string };

/**
 * Settings > Mail, per domain: which protocol mail from that domain goes out
 * over, and a live check of its DNS records. One <tbody> per domain, keyed by
 * name, so a row keeps its own state when the page re-renders after a save.
 */
export default function MailIpDomains({
  domains,
  defaultProtocol,
  saveAction,
  checkAction,
}: {
  domains: MailIpDomain[];
  defaultProtocol: MailIpProtocol;
  saveAction: SaveAction;
  checkAction: CheckAction;
}) {
  if (domains.length === 0) {
    return (
      <div className={styles.empty}>
        No mail domains yet. A domain gets mail when it is added under Domains or gets its first
        mailbox.
      </div>
    );
  }
  return (
    <div className={local.tableWrap}>
      <table className={local.table}>
        <thead>
          <tr>
            <th>Domain</th>
            <th>Outgoing mail</th>
            <th>In use now</th>
            <th aria-label="Actions" />
          </tr>
        </thead>
        {domains.map((d) => (
          <DomainRow
            key={d.name}
            domain={d}
            defaultProtocol={defaultProtocol}
            saveAction={saveAction}
            checkAction={checkAction}
          />
        ))}
      </table>
    </div>
  );
}

function DomainRow({
  domain,
  defaultProtocol,
  saveAction,
  checkAction,
}: {
  domain: MailIpDomain;
  defaultProtocol: MailIpProtocol;
  saveAction: SaveAction;
  checkAction: CheckAction;
}) {
  const { name, preference: saved, effective } = domain;
  const [choice, setChoice] = useState<MailIpPreference>(saved);
  const [saving, startSave] = useTransition();
  const [save, setSave] = useState<SaveOutcome | null>(null);
  const saveSeq = useRef(0);
  const [checking, setChecking] = useState(false);
  const [check, setCheck] = useState<CheckOutcome | null>(null);
  const checkSeq = useRef(0);

  // After a save the page re-renders with the stored value; follow it.
  const seen = useRef(saved);
  useEffect(() => {
    if (seen.current === saved) return;
    seen.current = saved;
    setChoice(saved);
  }, [saved]);

  const submit = (force: boolean) => {
    if (saving) return;
    const preference = choice;
    const id = ++saveSeq.current;
    startSave(async () => {
      let r: MailIpWriteResult<MailIpDomain>;
      try {
        r = await saveAction(name, preference, force);
      } catch (e: unknown) {
        r = { ok: false, error: errorText(e) };
      }
      if (id !== saveSeq.current) return;
      // In the transition too, so the message and the re-rendered row show together.
      startSave(() => {
        if (r.ok) setSave({ kind: "saved", effective: r.data.effective });
        else if (r.needsForce) setSave({ kind: "force", error: r.error, preference });
        else setSave({ kind: "error", error: r.error });
      });
    });
  };

  const pick = (value: MailIpPreference) => {
    setChoice(value);
    saveSeq.current++;
    setSave(null);
  };

  const runCheck = async () => {
    if (checking) return;
    const id = ++checkSeq.current;
    setChecking(true);
    let next: CheckOutcome;
    try {
      const r = await checkAction(name);
      next = r.ok
        ? { ok: true, data: r.data, at: new Date().toLocaleTimeString() }
        : { ok: false, error: r.error };
    } catch (e: unknown) {
      next = { ok: false, error: `The check did not finish: ${errorText(e)}` };
    }
    if (id !== checkSeq.current) return;
    setCheck(next);
    setChecking(false);
  };

  const asking = save?.kind === "force" && save.preference === choice;
  const showDetail = save !== null || check !== null;

  return (
    <tbody>
      <tr>
        <td>
          <span className={local.cellLabel} aria-hidden>
            Domain
          </span>
          <span className={`${styles.code} ${local.name}`}>{name}</span>
        </td>
        <td>
          <span className={local.cellLabel} aria-hidden>
            Outgoing mail
          </span>
          <select
            className={`${styles.input} ${local.select}`}
            aria-label={`Outgoing mail for ${name}`}
            value={choice}
            disabled={saving}
            onChange={(e) => pick(e.target.value as MailIpPreference)}
          >
            <option value="default">Server default ({PROTOCOL_NAME[defaultProtocol]})</option>
            <option value="ipv4">IPv4</option>
            <option value="ipv6">IPv6</option>
          </select>
        </td>
        <td>
          <span className={local.cellLabel} aria-hidden>
            In use now
          </span>
          {PROTOCOL_NAME[effective] ?? effective} first
          {saved === "default" && <span className={local.sub}> (server default)</span>}
        </td>
        <td>
          <div className={local.rowActions}>
            {!asking && (
              <button
                type="button"
                className={styles.btnGhost}
                onClick={() => submit(false)}
                disabled={saving || choice === saved}
              >
                {saving ? <Busy label="Saving..." /> : "Save"}
              </button>
            )}
            <button
              type="button"
              className={styles.btnGhost}
              onClick={runCheck}
              disabled={checking}
              aria-busy={checking}
            >
              {checking ? <Busy label="Checking..." /> : "Check DNS"}
            </button>
          </div>
        </td>
      </tr>
      {showDetail && (
        <tr>
          <td colSpan={4} className={local.detailCell}>
            <div className={local.detail} aria-live="polite">
              {save?.kind === "error" && <div className={styles.error}>{save.error}</div>}
              {save?.kind === "saved" && (
                <div className={styles.notice}>
                  Saved. Mail from {name} now goes out over{" "}
                  {PROTOCOL_NAME[save.effective] ?? save.effective} first.
                </div>
              )}
              {asking && (
                <ForceConfirm
                  message={save.error}
                  busy={saving}
                  onForce={() => submit(true)}
                  onCancel={() => pick(saved)}
                />
              )}
              {check && <DnsResult name={name} check={check} onHide={() => setCheck(null)} />}
            </div>
          </td>
        </tr>
      )}
    </tbody>
  );
}

function DnsResult({
  name,
  check,
  onHide,
}: {
  name: string;
  check: CheckOutcome;
  onHide: () => void;
}) {
  if (!check.ok) return <div className={styles.error}>{check.error}</div>;
  const { data } = check;
  const problems = hasProblems(data.checks);
  const domainLink = (
    <Link href={`/domains/${encodeURIComponent(name)}`}>{name}</Link>
  );
  return (
    <div>
      <div className={local.row} style={{ justifyContent: "space-between", marginBottom: 8 }}>
        <span className={local.checkedAt}>
          DNS for {name}, checked at {check.at}
        </span>
        <button type="button" className={styles.btnGhost} onClick={onHide}>
          Hide
        </button>
      </div>
      <CheckList checks={data.checks} />
      {data.managed ? (
        <p className={local.sub} style={{ marginTop: 10 }}>
          {problems ? (
            <>
              This panel hosts the zone: open {domainLink} and use Reconcile zone to put the mail
              records right.
            </>
          ) : (
            <>This panel hosts the zone. Its records are on the page for {domainLink}.</>
          )}
        </p>
      ) : (
        problems && (
          <p className={local.sub} style={{ marginTop: 10 }}>
            The zone is hosted elsewhere, so these records are changed at that DNS provider.
          </p>
        )
      )}
    </div>
  );
}
