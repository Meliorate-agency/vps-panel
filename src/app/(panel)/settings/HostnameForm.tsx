"use client";

import { useActionState, useState } from "react";
import styles from "@/components/page.module.css";
import local from "./server.module.css";
import type { HostnameStatus } from "@/lib/agent";
import type { HostnameResult } from "@/server-actions/hostname";

type Action = (prev: HostnameResult | null, fd: FormData) => Promise<HostnameResult>;

/**
 * Settings > Server > Server hostname. `panelHost` (null in IP-only mode)
 * is offered as a one-click value, since most servers use the same name
 * for both.
 */
export default function HostnameForm({
  status,
  panelHost,
  action,
}: {
  status: HostnameStatus;
  panelHost: string | null;
  action: Action;
}) {
  const [state, formAction, pending] = useActionState<HostnameResult | null, FormData>(
    action,
    null,
  );
  const [value, setValue] = useState(status.hostname);
  const name = value.trim().toLowerCase().replace(/\.$/, "");
  // Saving the same name still helps when cloud-init would reset it, or
  // when the running name is not the saved one.
  const needsSave =
    (status.cloud_init && !status.preserved) || status.running !== status.hostname;
  const dirty = name !== "" && (name !== status.hostname || needsSave);

  return (
    <form action={formAction}>
      <ul className={local.facts}>
        <li>
          <span className={styles.muted}>Now:</span>{" "}
          <span className={styles.code}>{status.hostname}</span>
          {status.running !== status.hostname && (
            <>
              {" "}
              (running as <span className={styles.code}>{status.running}</span> until the next
              reboot)
            </>
          )}
        </li>
      </ul>
      {status.cloud_init && !status.preserved && (
        <div className={local.warn}>
          cloud-init resets the hostname at every boot, so a name set with{" "}
          <span className={styles.code}>hostnamectl</span> alone goes back to the provider&apos;s
          after a reboot. Saving here also tells cloud-init to keep it.
        </div>
      )}
      <div className={styles.formRow}>
        <label className={styles.field}>
          Hostname
          <input
            name="hostname"
            value={value}
            onChange={(e) => setValue(e.target.value)}
            className={styles.input}
            required
            maxLength={65}
            spellCheck={false}
            autoCapitalize="none"
            autoComplete="off"
          />
        </label>
      </div>
      <p className={`${styles.muted} ${local.hint}`}>
        Letters, digits and hyphens, with dots between the parts. A full name such as{" "}
        <span className={styles.code}>vps.example.com</span> also gives the short name{" "}
        <span className={styles.code}>vps</span>. Open SSH sessions show the new name after you
        reconnect.
      </p>
      {state?.ok === false && <div className={styles.error}>{state.error}</div>}
      {state?.ok === true && (
        <div className={styles.notice}>
          The hostname is now <span className={styles.code}>{state.hostname}</span>.
          <ul className={local.changes} style={{ margin: "6px 0 0 18px" }}>
            {state.steps.map((s) => (
              <li key={s}>{s}</li>
            ))}
          </ul>
        </div>
      )}
      <div className={local.actions}>
        <button type="submit" className={styles.btn} disabled={pending || !dirty}>
          {pending ? "Saving..." : "Save hostname"}
        </button>
        {panelHost && panelHost !== name && (
          <button type="button" className={styles.btnGhost} onClick={() => setValue(panelHost)}>
            Use the panel hostname ({panelHost})
          </button>
        )}
      </div>
    </form>
  );
}
