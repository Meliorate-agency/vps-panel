"use client";

import { useState } from "react";
import styles from "@/components/page.module.css";
import { useFormActionState } from "@/lib/useFormActionState";
import type { PortActionResult as Result } from "@/server-actions/firewall";
import { ForceConfirm } from "../settings/mailIpParts";

export default function PortForm({
  action,
}: {
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
}) {
  const [state, formAction, pending, formRef] = useFormActionState<Result | null>(action, null);
  const [port, setPort] = useState("");
  const [proto, setProto] = useState("tcp");
  // The refusal the operator cancelled, or edited the port after: its
  // confirmation no longer applies.
  const [dismissed, setDismissed] = useState<Result | null>(null);
  // The agent refused to block a port the server needs (409). Only the
  // "Block it anyway" button below sends force, for the port it refused.
  const refusal =
    state?.ok === false && state.needsForce === true && state !== dismissed ? state : null;
  const asking = refusal !== null;
  const edit = () => {
    if (refusal) setDismissed(refusal);
  };
  return (
    <form
      ref={formRef}
      action={formAction}
      onKeyDown={(e) => {
        // Enter in a field submits with the form's first submit button,
        // which is "Block it anyway" while it is shown.
        if (refusal && e.key === "Enter" && e.target instanceof HTMLInputElement) {
          e.preventDefault();
        }
      }}
    >
      <div style={{ display: "flex", gap: 8, alignItems: "flex-end", flexWrap: "wrap" }}>
        <label className={styles.field}>
          Port
          <input
            name="port"
            type="number"
            required
            min={1}
            max={65535}
            className={styles.input}
            value={port}
            onChange={(e) => {
              setPort(e.target.value);
              edit();
            }}
          />
        </label>
        <label className={styles.field}>
          Proto
          <select
            name="proto"
            className={styles.select}
            value={proto}
            onChange={(e) => {
              setProto(e.target.value);
              edit();
            }}
          >
            <option>tcp</option>
            <option>udp</option>
          </select>
        </label>
        {!asking && (
          <>
            <button
              type="submit"
              name="intent"
              value="allow"
              className={styles.btn}
              disabled={pending}
            >
              {pending ? "..." : "Allow"}
            </button>
            <button
              type="submit"
              name="intent"
              value="deny"
              className={styles.btnDanger}
              disabled={pending}
              onClick={(e) => {
                const msg = port
                  ? `Deny port ${port}?\n\nIf this port is currently in use (e.g. 22/SSH, 443/HTTPS) you may lose access to the server.`
                  : "Deny this port?\n\nIf the port is currently in use (e.g. 22/SSH, 443/HTTPS) you may lose access to the server.";
                if (!window.confirm(msg)) e.preventDefault();
              }}
            >
              {pending ? "..." : "Deny"}
            </button>
          </>
        )}
        {state?.ok === false && !state.needsForce && (
          <span className={styles.error}>{state.error}</span>
        )}
      </div>
      {refusal && (
        <div style={{ marginTop: 10 }}>
          <input type="hidden" name="intent" value="deny" />
          <ForceConfirm
            message={refusal.error}
            busy={pending}
            submitName="force"
            forceLabel="Block it anyway"
            busyLabel="Blocking..."
            note={`Nothing was changed. Blocking ${port}/${proto} anyway can cut off SSH to this server or take its websites offline.`}
            onCancel={() => setDismissed(refusal)}
          />
        </div>
      )}
    </form>
  );
}
