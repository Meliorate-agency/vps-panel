"use client";

import { useEffect, useRef, useState, useTransition } from "react";
import styles from "@/components/page.module.css";
import type { Ipv6NewDomains, Ipv6Status } from "@/lib/agent";
import type { Ipv6Result } from "@/server-actions/ipv6";
import mail from "./mailIp.module.css";
import local from "./ipv6.module.css";
import { errorText } from "./mailIpParts";

type Action = (change: { address?: string; new_domains?: string }) => Promise<Ipv6Result<Ipv6Status>>;

type Outcome = { ok: true; status: Ipv6Status } | { ok: false; error: string };

const NEW_DOMAINS: { value: Ipv6NewDomains; title: string; text: string }[] = [
  {
    value: "off",
    title: "Off",
    text: "New domains get no IPv6 records. Switch it on for a domain on its page.",
  },
  {
    value: "on",
    title: "On",
    text: "New domains publish this server's IPv6 address as they are added.",
  },
];

/**
 * Settings > DNS & SSL > IPv6: which address domains with IPv6 on publish
 * ("" = automatic), and whether new domains get IPv6. Only what was changed
 * is sent, so saving the default for new domains does not touch the address.
 */
export default function Ipv6Form({ status, action }: { status: Ipv6Status; action: Action }) {
  const savedAddress = status.setting;
  const savedNew = status.new_domains;
  const [address, setAddress] = useState(savedAddress);
  const [newDomains, setNewDomains] = useState<Ipv6NewDomains>(savedNew);
  const [pending, start] = useTransition();
  const [outcome, setOutcome] = useState<Outcome | null>(null);
  // Only the newest request may set the outcome.
  const seq = useRef(0);

  // A save re-renders the page with the stored values but keeps this
  // instance: follow them when they change.
  const seen = useRef({ address: savedAddress, newDomains: savedNew });
  useEffect(() => {
    if (seen.current.address !== savedAddress) setAddress(savedAddress);
    if (seen.current.newDomains !== savedNew) setNewDomains(savedNew);
    seen.current = { address: savedAddress, newDomains: savedNew };
  }, [savedAddress, savedNew]);

  const addressChanged = address !== savedAddress;
  const newChanged = newDomains !== savedNew;

  const submit = () => {
    if (pending || (!addressChanged && !newChanged)) return;
    const change = {
      ...(addressChanged ? { address } : {}),
      ...(newChanged ? { new_domains: newDomains } : {}),
    };
    const id = ++seq.current;
    start(async () => {
      let r: Ipv6Result<Ipv6Status>;
      try {
        r = await action(change);
      } catch (e: unknown) {
        r = { ok: false, error: errorText(e) };
      }
      if (id !== seq.current) return;
      // In the transition too, so the message and the re-rendered page show together.
      start(() => setOutcome(r.ok ? { ok: true, status: r.data } : { ok: false, error: r.error }));
    });
  };

  const touched = () => {
    // A message about the earlier choice no longer applies.
    seq.current++;
    setOutcome(null);
  };

  // The stored address when it is not on an interface any more: still listed,
  // so the form shows what is stored.
  const options = [...status.candidates];
  if (savedAddress && !options.some((c) => c.address === savedAddress)) {
    options.push({ address: savedAddress, device: "", note: "" });
  }
  const showAddresses = options.length > 0;
  const missing = (a: string) => !status.candidates.some((c) => c.address === a);

  // Records move only when the address changes and some domain has IPv6 on.
  const moving = addressChanged && status.domains_on > 0;
  const n = status.domains_on;
  const domainsText = `${n} domain${n === 1 ? "" : "s"} with IPv6 on`;

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        submit();
      }}
    >
      {showAddresses && (
        <fieldset className={mail.choices} disabled={pending}>
          <legend className={mail.legend}>Address domains publish</legend>
          <label className={mail.choice}>
            <input
              type="radio"
              name="ipv6-address"
              value=""
              checked={address === ""}
              onChange={() => {
                setAddress("");
                touched();
              }}
            />
            <span className={mail.choiceText}>
              <span className={mail.choiceTitle}>Automatic</span>
              <span className={mail.sub}>
                The address the panel hostname&apos;s AAAA record points to, or the server&apos;s only
                IPv6 address.
              </span>
              {savedAddress === "" && <span className={mail.sub}>Chosen now</span>}
            </span>
          </label>
          {options.map((c) => (
            <label key={c.address} className={mail.choice}>
              <input
                type="radio"
                name="ipv6-address"
                value={c.address}
                checked={address === c.address}
                onChange={() => {
                  setAddress(c.address);
                  touched();
                }}
              />
              <span className={mail.choiceText}>
                <span className={local.address}>{c.address}</span>
                {c.device && <span className={mail.sub}>On {c.device}</span>}
                {c.note && <span className={mail.sub}>{c.note}</span>}
                {missing(c.address) && (
                  <span className={mail.sub}>Not on this server&apos;s network interfaces now.</span>
                )}
                {savedAddress === c.address && <span className={mail.sub}>Chosen now</span>}
                {savedAddress === "" && status.address === c.address && (
                  <span className={mail.sub}>In effect now, found by Automatic</span>
                )}
              </span>
            </label>
          ))}
        </fieldset>
      )}

      <fieldset className={mail.choices} disabled={pending}>
        <legend className={mail.legend}>IPv6 for new domains</legend>
        {NEW_DOMAINS.map((o) => (
          <label key={o.value} className={mail.choice}>
            <input
              type="radio"
              name="ipv6-new-domains"
              value={o.value}
              checked={newDomains === o.value}
              onChange={() => {
                setNewDomains(o.value);
                touched();
              }}
            />
            <span className={mail.choiceText}>
              <span className={mail.choiceTitle}>{o.title}</span>
              <span className={mail.sub}>{o.text}</span>
              {o.value === savedNew && <span className={mail.sub}>Chosen now</span>}
            </span>
          </label>
        ))}
      </fieldset>

      <div className={mail.row}>
        <button
          type="submit"
          className={styles.btn}
          disabled={pending || (!addressChanged && !newChanged)}
        >
          {pending ? "Saving..." : "Save"}
        </button>
        {moving && (
          <span className={local.saveNote}>
            {address === ""
              ? `Saving moves the IPv6 records of the ${domainsText} to the address Automatic finds, if that is a different one.`
              : `Saving moves the IPv6 records of the ${domainsText} to ${address}.`}
          </span>
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
            {outcome.status.address
              ? `Saved. Domains with IPv6 on publish ${outcome.status.address}.`
              : "Saved. This server has no IPv6 address in effect, so no domain publishes one."}{" "}
            {outcome.status.new_domains === "on"
              ? "New domains get IPv6."
              : "New domains get no IPv6."}
          </div>
        )}
      </div>

      {status.notes.length > 0 && (
        <ul className={mail.notes}>
          {status.notes.map((note, i) => (
            <li key={i}>{note}</li>
          ))}
        </ul>
      )}
    </form>
  );
}
