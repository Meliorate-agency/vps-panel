"use client";

import { useActionState, useState } from "react";
import styles from "@/components/page.module.css";

type Result = { ok: true } | { ok: false; error: string };

export default function PortForm({
  action,
}: {
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
}) {
  const [state, formAction, pending] = useActionState<Result | null, FormData>(action, null);
  const [port, setPort] = useState("");
  return (
    <form action={formAction} style={{ display: "flex", gap: 8, alignItems: "flex-end", flexWrap: "wrap" }}>
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
          onChange={(e) => setPort(e.target.value)}
        />
      </label>
      <label className={styles.field}>
        Proto
        <select name="proto" className={styles.select} defaultValue="tcp">
          <option>tcp</option>
          <option>udp</option>
        </select>
      </label>
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
      {state?.ok === false && <span className={styles.error}>{state.error}</span>}
    </form>
  );
}
