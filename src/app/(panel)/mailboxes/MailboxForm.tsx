"use client";

import { useActionState, useEffect, useRef, useState } from "react";
import styles from "@/components/page.module.css";
import localStyles from "./mailboxes.module.css";

type Result =
  | { ok: true; data?: { password: string } }
  | { ok: false; error: string };

function fmtBytes(bytes: number): string {
  if (bytes >= 1_073_741_824) return `${(bytes / 1_073_741_824).toFixed(1)} GB`;
  if (bytes >= 1_048_576) return `${(bytes / 1_048_576).toFixed(0)} MB`;
  return `${(bytes / 1024).toFixed(0)} KB`;
}

export default function MailboxForm({
  action,
  domains,
  diskFreeBytes,
}: {
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
  domains: string[];
  diskFreeBytes: number | null;
}) {
  const [state, formAction, pending] = useActionState<Result | null, FormData>(action, null);
  const [localpart, setLocalpart] = useState("");
  const [domain, setDomain] = useState(domains[0] ?? "");
  const formRef = useRef<HTMLFormElement>(null);
  const domainRef = useRef<HTMLSelectElement>(null);
  // React 19 resets the form after the action; the <select> DOM snaps to the
  // first domain while `domain` (and so the hidden email) stays, so the next
  // mailbox would land on a domain the picker no longer shows. Re-apply it
  // once the reset has landed, as DeployForm does.
  useEffect(() => {
    const form = formRef.current;
    if (!form) return;
    const onReset = () => {
      queueMicrotask(() => {
        if (domainRef.current) domainRef.current.value = domain;
      });
    };
    form.addEventListener("reset", onReset);
    return () => form.removeEventListener("reset", onReset);
  }, [domain]);

  if (domains.length === 0) {
    return (
      <p className={localStyles.muted}>
        No domains registered yet. Add one on the Domains page first.
      </p>
    );
  }

  return (
    <form ref={formRef} action={formAction}>
      {/* Username gets its own row so the domain <select>'s open dropdown
          can lay out naturally without bleeding into a sibling cell. The
          inner flex uses `min-width: 0` + `flex: 1` on the children so the
          longest domain name doesn't push the layout wider than the row. */}
      <label className={styles.field} style={{ marginBottom: 12 }}>
        Username
        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: 4,
            minWidth: 0,
            width: "100%",
          }}
        >
          <input
            className={styles.input}
            type="text"
            placeholder="username"
            value={localpart}
            onChange={(e) => setLocalpart(e.target.value)}
            required
            style={{ flex: 1, minWidth: 0 }}
          />
          <span style={{ color: "var(--text-dim)", flexShrink: 0, userSelect: "none" }}>@</span>
          <select
            ref={domainRef}
            className={styles.input}
            value={domain}
            onChange={(e) => setDomain(e.target.value)}
            style={{ flex: 1, minWidth: 0, maxWidth: "100%" }}
          >
            {domains.map((d) => (
              <option key={d} value={d}>{d}</option>
            ))}
          </select>
          <input type="hidden" name="email" value={`${localpart}@${domain}`} />
        </div>
      </label>

      <div className={styles.formRow}>
        <label className={styles.field}>
          Password (optional)
          <input name="password" className={styles.input} type="text" placeholder="leave blank to generate" />
        </label>
        <label className={styles.field}>
          Quota (MB)
          <input name="quota_mb" className={styles.input} type="number" defaultValue={1024} />
          {diskFreeBytes !== null && (
            <span className={localStyles.diskHint}>{fmtBytes(diskFreeBytes)} free on server</span>
          )}
        </label>
      </div>
      {state?.ok === false && <div className={styles.error}>{state.error}</div>}
      {state?.ok === true && state.data && (
        <div className={styles.notice}>
          Initial password (copy it now, it will not be shown again):{" "}
          <code className={styles.code}>{state.data.password}</code>
        </div>
      )}
      <button type="submit" className={styles.btn} disabled={pending}>
        {pending ? "..." : "Create mailbox"}
      </button>
    </form>
  );
}
