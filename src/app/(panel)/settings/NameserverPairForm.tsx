"use client";

import { useState, useTransition } from "react";
import styles from "@/components/page.module.css";
import { nsPairFields, nsPairValue } from "@/lib/nsPair";

type Result = { ok: true } | { ok: false; error: string };

/** The shared `dns.ns_pair`: one field per nameserver, saved as "ns1,ns2". */
export default function NameserverPairForm({
  value,
  action,
}: {
  value: string;
  action: (next: string) => Promise<Result>;
}) {
  const [first, second] = nsPairFields(value);
  const [ns1, setNs1] = useState(first);
  const [ns2, setNs2] = useState(second);
  const [pending, start] = useTransition();
  const [msg, setMsg] = useState<Result | null>(null);

  const save = () => {
    const pair = nsPairValue(ns1, ns2);
    if ("error" in pair) {
      setMsg({ ok: false, error: pair.error });
      return;
    }
    start(async () => {
      setMsg(await action(pair.value));
    });
  };

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        save();
      }}
    >
      <div className={styles.formRow}>
        <label className={styles.field}>
          Nameserver 1
          <input
            type="text"
            value={ns1}
            onChange={(e) => setNs1(e.target.value)}
            placeholder="ns1.example.com"
            className={styles.input}
            autoComplete="off"
            spellCheck={false}
          />
        </label>
        <label className={styles.field}>
          Nameserver 2
          <input
            type="text"
            value={ns2}
            onChange={(e) => setNs2(e.target.value)}
            placeholder="ns2.example.com"
            className={styles.input}
            autoComplete="off"
            spellCheck={false}
          />
        </label>
      </div>
      <div style={{ display: "flex", gap: 8, alignItems: "flex-start", flexWrap: "wrap" }}>
        <button type="submit" className={styles.btn} disabled={pending}>
          {pending ? "..." : "Save"}
        </button>
        {msg?.ok === false && <div className={styles.error}>{msg.error}</div>}
        {msg?.ok === true && (
          <div className={styles.notice}>
            Saved. Applies on the next zone create or reconcile.
          </div>
        )}
      </div>
    </form>
  );
}
