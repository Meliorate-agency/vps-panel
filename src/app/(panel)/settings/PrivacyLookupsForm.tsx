"use client";

import { useEffect, useRef, useState, useTransition } from "react";
import styles from "@/components/page.module.css";
import type { PrivacyResult } from "@/server-actions/privacy";
import mail from "./mailIp.module.css";
import { errorText } from "./mailIpParts";

type Geo = "on" | "off";
type Action = (serverGeo: string) => Promise<PrivacyResult>;

const OPTIONS: { value: Geo; title: string; text: string }[] = [
  {
    value: "on",
    title: "Look up server locations",
    text: "My Networks shows the town and country of each server.",
  },
  {
    value: "off",
    title: "No lookups",
    text: "The panel asks neither service, and My Networks shows no locations.",
  },
];

/** Settings > Privacy > Outside lookups: the server locations on My Networks. */
export default function PrivacyLookupsForm({ serverGeo, action }: { serverGeo: Geo; action: Action }) {
  const [choice, setChoice] = useState<Geo>(serverGeo);
  const [pending, start] = useTransition();
  const [outcome, setOutcome] = useState<{ ok: true; geo: Geo } | { ok: false; error: string } | null>(null);
  const seq = useRef(0);

  // A save re-renders the page with the stored value; follow it.
  const seen = useRef(serverGeo);
  useEffect(() => {
    if (seen.current === serverGeo) return;
    seen.current = serverGeo;
    setChoice(serverGeo);
  }, [serverGeo]);

  const submit = () => {
    if (pending) return;
    const geo = choice;
    const id = ++seq.current;
    start(async () => {
      let r: PrivacyResult;
      try {
        r = await action(geo);
      } catch (e: unknown) {
        r = { ok: false, error: errorText(e) };
      }
      if (id !== seq.current) return;
      start(() => setOutcome(r.ok ? { ok: true, geo } : r));
    });
  };

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        submit();
      }}
    >
      <fieldset className={mail.choices} disabled={pending}>
        <legend className={mail.legend}>Server locations on My Networks</legend>
        {OPTIONS.map((o) => (
          <label key={o.value} className={mail.choice}>
            <input
              type="radio"
              name="privacy-server-geo"
              value={o.value}
              checked={choice === o.value}
              onChange={() => {
                setChoice(o.value);
                seq.current++;
                setOutcome(null);
              }}
            />
            <span className={mail.choiceText}>
              <span className={mail.choiceTitle}>{o.title}</span>
              <span className={mail.sub}>{o.text}</span>
              {o.value === serverGeo && <span className={mail.sub}>Chosen now</span>}
            </span>
          </label>
        ))}
      </fieldset>

      <div className={mail.row}>
        <button type="submit" className={styles.btn} disabled={pending || choice === serverGeo}>
          {pending ? "Saving..." : "Save"}
        </button>
      </div>

      <div aria-live="polite">
        {outcome?.ok === false && (
          <div className={styles.error} style={{ marginTop: 10 }}>
            {outcome.error}
          </div>
        )}
        {outcome?.ok && (
          <div className={styles.notice} style={{ marginTop: 10 }}>
            {outcome.geo === "off"
              ? "Saved. The panel no longer looks up server locations."
              : "Saved. My Networks looks up server locations again."}
          </div>
        )}
      </div>
    </form>
  );
}
