"use client";

import { useActionState, useEffect, useRef, useState } from "react";
import styles from "@/components/page.module.css";
import type { SshRootLogin, SshServerConfig } from "@/lib/agent";

type Result = { ok: true } | { ok: false; error: string };

const ROOT_LOGIN_LABELS: Record<SshRootLogin, string> = {
  yes: "Allowed (key or password)",
  "prohibit-password": "Key only",
  "forced-commands-only": "Key only, forced commands",
  no: "Not allowed",
};

// Higher = more permissive; used to warn when a change loosens root login.
const ROOT_LOGIN_RANK: Record<SshRootLogin, number> = {
  no: 0,
  "forced-commands-only": 1,
  "prohibit-password": 2,
  yes: 3,
};

export default function SshServerForm({
  config,
  action,
  loginOnly = false,
  suggestPasswordAuth,
  suggestRootLogin,
}: {
  config: SshServerConfig;
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
  /** Only the password and root-login controls (the setup wizard). */
  loginOnly?: boolean;
  /** Start the controls at these values instead of the live ones; nothing
   *  changes until the operator saves. */
  suggestPasswordAuth?: boolean;
  suggestRootLogin?: SshRootLogin;
}) {
  const [state, formAction, pending] = useActionState<Result | null, FormData>(
    action,
    null,
  );
  const [pwAuth, setPwAuth] = useState(suggestPasswordAuth ?? config.password_auth);
  const [rootLogin, setRootLogin] = useState<SshRootLogin>(
    suggestRootLogin ?? config.permit_root_login,
  );

  // After a save, revalidatePath re-renders with the new live values but
  // keeps this component instance, so follow them when they change. Only on
  // a change: syncing on mount would throw away a suggested value.
  const seenPw = useRef(config.password_auth);
  useEffect(() => {
    if (seenPw.current === config.password_auth) return;
    seenPw.current = config.password_auth;
    setPwAuth(config.password_auth);
  }, [config.password_auth]);
  const seenRoot = useRef(config.permit_root_login);
  useEffect(() => {
    if (seenRoot.current === config.permit_root_login) return;
    seenRoot.current = config.permit_root_login;
    setRootLogin(config.permit_root_login);
  }, [config.permit_root_login]);

  const looseningSecurity =
    (!config.password_auth && pwAuth) ||
    ROOT_LOGIN_RANK[rootLogin] > ROOT_LOGIN_RANK[config.permit_root_login];
  const disablingPasswords = config.password_auth && !pwAuth;
  // "Key only, forced commands" is only offered when the server already uses it.
  const rootModes = (Object.keys(ROOT_LOGIN_LABELS) as SshRootLogin[]).filter(
    (m) => m !== "forced-commands-only" || config.permit_root_login === m,
  );

  return (
    <form action={formAction}>
      {/* The live values, so the action sends sshd only what was changed. */}
      <input type="hidden" name="current_port" value={config.port} />
      <input type="hidden" name="current_idle_timeout_minutes" value={config.idle_timeout_minutes} />
      <input type="hidden" name="current_password_auth" value={config.password_auth ? "1" : "0"} />
      <input type="hidden" name="current_permit_root_login" value={config.permit_root_login} />

      <div className={styles.formRow}>
        {!loginOnly && (
        <label className={styles.field}>
          Port
          <input
            name="port"
            type="number"
            min={1}
            max={65535}
            defaultValue={config.port}
            required
            className={styles.input}
          />
        </label>
        )}
        {!loginOnly && (
        <label className={styles.field}>
          Idle timeout (minutes, 0 = never)
          <input
            name="idle_timeout_minutes"
            type="number"
            min={0}
            max={1440}
            defaultValue={config.idle_timeout_minutes}
            required
            className={styles.input}
          />
        </label>
        )}
        <label className={styles.field}>
          Root login over SSH
          <select
            name="permit_root_login"
            value={rootLogin}
            onChange={(e) => setRootLogin(e.target.value as SshRootLogin)}
            className={styles.input}
          >
            {rootModes.map((m) => (
              <option key={m} value={m}>
                {ROOT_LOGIN_LABELS[m]}
              </option>
            ))}
          </select>
        </label>
      </div>

      <div style={{ display: "flex", flexDirection: "column", gap: 14, marginBottom: 14 }}>
        <ToggleRow
          name="password_auth"
          label="Allow password authentication"
          help={
            <>
              <span className={styles.code}>PasswordAuthentication</span> +{" "}
              <span className={styles.code}>KbdInteractiveAuthentication</span>, for every
              user
            </>
          }
          checked={pwAuth}
          onChange={setPwAuth}
        />
      </div>

      {looseningSecurity && (
        <div
          style={{
            marginBottom: 12,
            padding: "10px 12px",
            background: "rgba(255, 196, 0, 0.08)",
            border: "1px solid rgba(255, 196, 0, 0.28)",
            borderRadius: 6,
            fontSize: 13,
          }}
        >
          <strong>Heads up:</strong> you&apos;re relaxing SSH security. A public SSH port
          gets constant brute-force attempts; fail2ban helps, but a strong password is
          non-optional. Consider adding an SSH key under{" "}
          <a href="/ssh-keys" className={styles.code}>/ssh-keys</a> and turning this back
          off once it works.
        </div>
      )}
      {disablingPasswords && (
        <p className={styles.muted} style={{ fontSize: 12, marginBottom: 10 }}>
          Make sure your SSH key is under <a href="/ssh-keys">/ssh-keys</a> first. If you
          get locked out, turn this back on here.
        </p>
      )}

      {!loginOnly && (
      <p className={styles.muted} style={{ fontSize: 12, marginBottom: 10 }}>
        Saved to <span className={styles.code}>/etc/ssh/sshd_config.d/00-vps-panel.conf</span>,
        which overrides the other sshd config files. The agent validates with{" "}
        <span className={styles.code}>sshd -t</span>, checks the result with{" "}
        <span className={styles.code}>sshd -T</span>, then reloads sshd; open sessions stay
        connected. Idle timeout closes a session with no traffic for that long. A port
        change opens the new port in UFW first and closes the old one only once sshd is
        listening on the new port.
      </p>
      )}
      {state?.ok === false && <div className={styles.error}>{state.error}</div>}
      {state?.ok === true && <div className={styles.notice}>SSH config updated.</div>}
      <button type="submit" className={styles.btn} disabled={pending}>
        {pending ? "Saving..." : loginOnly ? "Apply SSH login settings" : "Save SSH config"}
      </button>
    </form>
  );
}

// Pill-style on/off toggle. Renders a real <input type="checkbox"> behind the
// visual so HTML form submission still works (the form action picks up the
// checked state from FormData) and keyboard accessibility (Space to toggle,
// focus ring on the underlying input) is preserved.
function ToggleRow({
  name,
  label,
  help,
  checked,
  onChange,
}: {
  name: string;
  label: string;
  help: React.ReactNode;
  checked: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <label
      style={{
        display: "flex",
        alignItems: "center",
        gap: 12,
        cursor: "pointer",
        userSelect: "none",
      }}
    >
      <span
        style={{
          position: "relative",
          display: "inline-block",
          width: 40,
          height: 22,
          borderRadius: 999,
          background: checked
            ? "var(--accent)"
            : "var(--bg-elev-2)",
          border: `1px solid ${
            checked ? "transparent" : "var(--border)"
          }`,
          transition: "background-color 0.15s ease",
          flexShrink: 0,
        }}
      >
        <span
          style={{
            position: "absolute",
            top: 2,
            left: 2,
            width: 16,
            height: 16,
            borderRadius: "50%",
            background: checked ? "var(--accent-fg)" : "#fff",
            transform: `translateX(${checked ? 18 : 0}px)`,
            transition: "transform 0.15s ease",
            boxShadow: "0 1px 2px rgba(0,0,0,0.3)",
          }}
        />
        <input
          type="checkbox"
          name={name}
          checked={checked}
          onChange={(e) => onChange(e.target.checked)}
          style={{
            position: "absolute",
            inset: 0,
            opacity: 0,
            cursor: "pointer",
            width: "100%",
            height: "100%",
            margin: 0,
          }}
          aria-label={label}
        />
      </span>
      <span style={{ display: "flex", flexDirection: "column", gap: 2 }}>
        <span>{label}</span>
        <span className={styles.muted} style={{ fontSize: 12 }}>
          {help}
        </span>
      </span>
    </label>
  );
}
