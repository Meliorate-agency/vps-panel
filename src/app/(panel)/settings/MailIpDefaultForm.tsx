"use client";

import { useEffect, useRef, useState, useTransition } from "react";
import styles from "@/components/page.module.css";
import type { MailIpProtocol, MailIpSettings } from "@/lib/agent";
import type { MailIpWriteResult } from "@/server-actions/mail";
import local from "./mailIp.module.css";
import { ForceConfirm, PROTOCOL_NAME } from "./mailIpParts";

type Action = (protocol: string, force: boolean) => Promise<MailIpWriteResult<MailIpSettings>>;

type Outcome =
  | { kind: "saved"; protocol: MailIpProtocol }
  | { kind: "error"; error: string }
  | { kind: "force"; error: string; protocol: MailIpProtocol };

const OPTIONS: { value: MailIpProtocol; title: string }[] = [
  { value: "ipv4", title: "IPv4 first, IPv6 as fallback" },
  { value: "ipv6", title: "IPv6 first, IPv4 as fallback" },
];

/**
 * Settings > Mail: the server-wide protocol outgoing mail tries first. If the
 * agent refuses IPv6 as not ready, its reason is shown with a "Use IPv6
 * anyway" button; only that button sends force.
 */
export default function MailIpDefaultForm({
  settings,
  action,
}: {
  settings: MailIpSettings;
  action: Action;
}) {
  const saved = settings.default_protocol;
  const [choice, setChoice] = useState<MailIpProtocol>(saved);
  const [pending, start] = useTransition();
  const [outcome, setOutcome] = useState<Outcome | null>(null);
  // Only the newest request may set the outcome.
  const seq = useRef(0);

  // A save re-renders the page with the new value but keeps this instance:
  // follow it when it changes.
  const seen = useRef(saved);
  useEffect(() => {
    if (seen.current === saved) return;
    seen.current = saved;
    setChoice(saved);
  }, [saved]);

  const submit = (force: boolean) => {
    if (pending) return;
    const protocol = choice;
    const id = ++seq.current;
    start(async () => {
      let r: MailIpWriteResult<MailIpSettings>;
      try {
        r = await action(protocol, force);
      } catch (e: unknown) {
        r = { ok: false, error: e instanceof Error ? e.message : String(e) };
      }
      if (id !== seq.current) return;
      // In the transition too, so the message and the re-rendered page show together.
      start(() => {
        if (r.ok) setOutcome({ kind: "saved", protocol: r.data.default_protocol });
        else if (r.needsForce) setOutcome({ kind: "force", error: r.error, protocol });
        else setOutcome({ kind: "error", error: r.error });
      });
    });
  };

  const pick = (value: MailIpProtocol) => {
    setChoice(value);
    // A confirmation or message about the other choice no longer applies.
    seq.current++;
    setOutcome(null);
  };

  const address = (p: MailIpProtocol) =>
    p === "ipv4" ? settings.server_ipv4 : settings.server_ipv6;
  const asking = outcome?.kind === "force" && outcome.protocol === choice;

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        submit(false);
      }}
    >
      <fieldset className={local.choices} disabled={pending}>
        <legend className={local.legend}>Server default</legend>
        {OPTIONS.map((o) => {
          const ip = address(o.value);
          return (
            <label key={o.value} className={local.choice}>
              <input
                type="radio"
                name="mail-ip-default"
                value={o.value}
                checked={choice === o.value}
                onChange={() => pick(o.value)}
              />
              <span className={local.choiceText}>
                <span className={local.choiceTitle}>{o.title}</span>
                <span className={local.sub}>
                  {ip ? (
                    <>
                      This server&apos;s {PROTOCOL_NAME[o.value]} address:{" "}
                      <span className={styles.code}>{ip}</span>
                    </>
                  ) : (
                    `This server has no public ${PROTOCOL_NAME[o.value]} address.`
                  )}
                </span>
                {o.value === saved && <span className={local.sub}>In use now</span>}
              </span>
            </label>
          );
        })}
      </fieldset>

      {asking ? (
        <ForceConfirm
          message={outcome.error}
          busy={pending}
          onForce={() => submit(true)}
          onCancel={() => pick(saved)}
        />
      ) : (
        <div className={local.row}>
          {/* Enabled with an unchanged choice too: saving again is how IPv6
              is switched on once its check passes, as the notes below say. */}
          <button type="submit" className={styles.btn} disabled={pending}>
            {pending ? "Saving..." : choice === saved ? "Save again" : "Save"}
          </button>
        </div>
      )}

      <div aria-live="polite">
        {outcome?.kind === "error" && (
          <div className={styles.error} style={{ marginTop: 10 }}>
            {outcome.error}
          </div>
        )}
        {outcome?.kind === "saved" && (
          <div className={styles.notice} style={{ marginTop: 10 }}>
            Saved. {PROTOCOL_NAME[outcome.protocol]} is the protocol tried first, except for
            domains with a choice of their own below.
          </div>
        )}
      </div>

      {settings.notes.length > 0 && (
        <ul className={local.notes}>
          {settings.notes.map((n, i) => (
            <li key={i}>{n}</li>
          ))}
        </ul>
      )}
    </form>
  );
}
