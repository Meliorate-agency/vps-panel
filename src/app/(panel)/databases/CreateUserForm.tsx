"use client";

import { useActionState, useState } from "react";
import styles from "@/components/page.module.css";

// Mirror of the agent's RESERVED_USERS list in agent/src/handlers/databases.rs.
// Keep in sync — the agent re-enforces this server-side, but blocking on the
// client saves a round-trip and gives the operator inline feedback.
const RESERVED_USERS = [
  "root",
  "mariadb.sys",
  "mysql",
  "sys",
  "debian-sys-maint",
  "pdns",
  "mail",
  "panel",
];

type Result =
  | { ok: true; data?: { password: string; user: string; host: string } }
  | { ok: false; error: string };

type HostMode = "localhost" | "%" | "custom";

export default function CreateUserForm({
  action,
}: {
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
}) {
  const [state, formAction, pending] = useActionState<Result | null, FormData>(action, null);
  const [user, setUser] = useState("");
  const [hostMode, setHostMode] = useState<HostMode>("localhost");
  const [customHost, setCustomHost] = useState("");

  const host = hostMode === "custom" ? customHost : hostMode;
  const userIsReserved = RESERVED_USERS.includes(user.trim().toLowerCase());

  return (
    <form action={formAction}>
      <div className={styles.formRow}>
        <label className={styles.field}>
          User
          <input
            name="user"
            className={styles.input}
            required
            placeholder="appuser"
            value={user}
            onChange={(e) => setUser(e.target.value)}
          />
          {userIsReserved && (
            <span className={styles.muted}>
              <code className={styles.code}>{user.trim()}</code> is a reserved system user — pick a different name.
            </span>
          )}
        </label>
        <label className={styles.field}>
          Host
          <select
            className={styles.select}
            value={hostMode}
            onChange={(e) => setHostMode(e.target.value as HostMode)}
          >
            <option value="localhost">Local apps only (localhost)</option>
            <option value="%">Any host (% — required for phpMyAdmin)</option>
            <option value="custom">Custom…</option>
          </select>
          {hostMode === "custom" && (
            <input
              className={styles.input}
              placeholder="e.g. 172.17.0.%"
              value={customHost}
              onChange={(e) => setCustomHost(e.target.value)}
            />
          )}
          <span className={styles.muted}>
            Use <code className={styles.code}>%</code> if the user will log in from phpMyAdmin or a remote client.
          </span>
          <input type="hidden" name="host" value={host} />
        </label>
        <label className={styles.field}>
          Password (optional)
          <input
            name="password"
            className={styles.input}
            type="text"
            placeholder="leave blank to generate"
          />
        </label>
      </div>
      {state?.ok === false && <div className={styles.error}>{state.error}</div>}
      {state?.ok === true && state.data && (
        <div className={styles.notice}>
          Created <code className={styles.code}>{state.data.user}@{state.data.host}</code> with
          password <code className={styles.code}>{state.data.password}</code>
        </div>
      )}
      <button
        type="submit"
        className={styles.btn}
        disabled={pending || userIsReserved || (hostMode === "custom" && !customHost.trim())}
      >
        {pending ? "..." : "Create user"}
      </button>
    </form>
  );
}
