"use client";

import { useActionState, useEffect, useRef, useState } from "react";
import styles from "@/components/page.module.css";

type Result = { ok: true } | { ok: false; error: string };

/**
 * Form for creating a new Linux account on the host. Posts to the server
 * action which calls the agent's `useradd -m` + `chpasswd` (+ optional
 * `usermod -aG sudo`). On success the form resets so the operator can
 * enroll another account without a lingering password value.
 *
 * Mirrors the AddPanelUserForm shape (two-column input rows, reveal toggle,
 * inline notice/error) so the /users page reads consistently across its
 * three sections.
 */
export default function AddSystemUserForm({
  action,
}: {
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
}) {
  const [state, formAction, pending] = useActionState<Result | null, FormData>(
    action,
    null,
  );
  const [revealed, setRevealed] = useState(false);
  const formRef = useRef<HTMLFormElement>(null);

  useEffect(() => {
    if (state?.ok === true) {
      formRef.current?.reset();
      setRevealed(false);
    }
  }, [state]);

  return (
    <form ref={formRef} action={formAction}>
      <div className={styles.formRow}>
        <label className={styles.field}>
          Username
          <input
            name="name"
            type="text"
            className={styles.input}
            placeholder="claude"
            autoComplete="off"
            pattern="[a-z_][a-z0-9_\-]{0,31}"
            title="lowercase letters, digits, _ or -; must start with a letter or _"
            maxLength={32}
            required
          />
        </label>
        <label className={styles.field}>
          Login shell
          <select name="shell" className={styles.select} defaultValue="/bin/bash">
            <option value="/bin/bash">/bin/bash</option>
            <option value="/bin/sh">/bin/sh</option>
            <option value="/bin/zsh">/bin/zsh</option>
            <option value="/usr/sbin/nologin">/usr/sbin/nologin</option>
          </select>
        </label>
      </div>
      <div className={styles.formRow}>
        <label className={styles.field}>
          Password
          <input
            name="password"
            type={revealed ? "text" : "password"}
            className={styles.input}
            autoComplete="new-password"
            minLength={12}
            required
          />
        </label>
        <label className={styles.field}>
          Confirm
          <input
            name="confirm"
            type={revealed ? "text" : "password"}
            className={styles.input}
            autoComplete="new-password"
            minLength={12}
            required
          />
        </label>
      </div>
      <label className={styles.checkbox} style={{ marginBottom: 8 }}>
        <input
          type="checkbox"
          checked={revealed}
          onChange={(e) => setRevealed(e.target.checked)}
        />
        Show passwords
      </label>
      <label className={styles.checkbox} style={{ marginBottom: 12 }}>
        <input type="checkbox" name="sudo" />
        Grant sudo (add to the <span className={styles.code}>sudo</span> group)
      </label>
      <div className={styles.muted} style={{ fontSize: 12, marginBottom: 12 }}>
        Runs <span className={styles.code}>useradd -m</span> +{" "}
        <span className={styles.code}>chpasswd</span> on the host. Home
        directory is created at <span className={styles.code}>/home/&lt;name&gt;</span>.
        Whether <span className={styles.code}>ssh &lt;name&gt;@host</span> with
        the new password works depends on{" "}
        <a href="/settings#panel-host" className={styles.code}>
          /settings → SSH server
        </a>{" "}
        — flip{" "}
        <span className={styles.code}>PasswordAuthentication</span> on, or add
        an SSH key under{" "}
        <a href="/ssh-keys" className={styles.code}>
          /ssh-keys
        </a>{" "}
        for key-only login.
      </div>

      {state?.ok === false && (
        <div className={styles.error} style={{ marginBottom: 12 }}>
          {state.error}
        </div>
      )}
      {state?.ok === true && (
        <div className={styles.notice} style={{ marginBottom: 12 }}>
          Linux user created.
        </div>
      )}

      <button type="submit" className={styles.btn} disabled={pending}>
        {pending ? "Creating..." : "Create Linux user"}
      </button>
    </form>
  );
}
