"use client";

import { useActionState, useEffect, useState } from "react";
import styles from "@/components/page.module.css";
import localStyles from "./mailboxes.module.css";

type Result =
  | { ok: true; data?: { added: number } }
  | { ok: false; error: string };

/**
 * Add a forwarder: an address (or the domain's catch-all) and where its mail
 * goes. Only domains that receive mail here are offered: Postfix ignores a
 * forwarder on any other domain.
 */
export default function ForwarderForm({
  action,
  domains,
}: {
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
  domains: string[];
}) {
  const [state, formAction, pending] = useActionState<Result | null, FormData>(action, null);
  const [localpart, setLocalpart] = useState("");
  const [domain, setDomain] = useState(domains[0] ?? "");
  const [catchAll, setCatchAll] = useState(false);
  const [destinations, setDestinations] = useState("");

  // A forwarder that went in clears the form for the next one.
  useEffect(() => {
    if (state?.ok) {
      setLocalpart("");
      setDestinations("");
      setCatchAll(false);
    }
  }, [state]);

  if (domains.length === 0) {
    return (
      <p className={localStyles.muted}>
        Forwarders need a domain that receives mail here. Create a mailbox on it first.
      </p>
    );
  }

  return (
    <form action={formAction}>
      <label className={styles.field} style={{ marginBottom: 8 }}>
        Address
        <div style={{ display: "flex", alignItems: "center", gap: 4, minWidth: 0, width: "100%" }}>
          <input
            className={styles.input}
            type="text"
            placeholder={catchAll ? "any address" : "sales"}
            value={catchAll ? "" : localpart}
            onChange={(e) => setLocalpart(e.target.value)}
            disabled={catchAll}
            required={!catchAll}
            style={{ flex: 1, minWidth: 0 }}
          />
          <span style={{ color: "var(--text-dim)", flexShrink: 0, userSelect: "none" }}>@</span>
          <select
            className={styles.input}
            value={domain}
            onChange={(e) => setDomain(e.target.value)}
            style={{ flex: 1, minWidth: 0, maxWidth: "100%" }}
          >
            {domains.map((d) => (
              <option key={d} value={d}>{d}</option>
            ))}
          </select>
        </div>
        <input type="hidden" name="source" value={catchAll ? `@${domain}` : `${localpart}@${domain}`} />
      </label>
      <label className={styles.checkbox} style={{ marginBottom: 12 }}>
        <input type="checkbox" checked={catchAll} onChange={(e) => setCatchAll(e.target.checked)} />
        Catch-all: any address at {domain} that has no mailbox or forwarder of its own
      </label>

      <label className={styles.field} style={{ marginBottom: 12 }}>
        Forward to
        <textarea
          name="destinations"
          className={styles.input}
          rows={2}
          placeholder="someone@gmail.com, colleague@example.com"
          value={destinations}
          onChange={(e) => setDestinations(e.target.value)}
          required
        />
        <span className={localStyles.hint}>
          One or more addresses, separated by commas or new lines. A forwarder on a
          mailbox&apos;s own address keeps a copy in that mailbox.
        </span>
      </label>

      {state?.ok === false && <div className={styles.error}>{state.error}</div>}
      {state?.ok === true && (
        <div className={styles.notice}>
          {state.data?.added === 0 ? "It already forwards there." : "Forwarder added."}
        </div>
      )}
      <button type="submit" className={styles.btn} disabled={pending}>
        {pending ? "..." : "Add forwarder"}
      </button>
    </form>
  );
}
