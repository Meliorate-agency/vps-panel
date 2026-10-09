"use client";

import { useActionState, useEffect, useRef } from "react";
import styles from "@/components/page.module.css";

type Result = { ok: true } | { ok: false; error: string };

export type SshUserOption = {
  name: string;
  home: string;
  shell: string;
  sudo: boolean;
};

/**
 * Paste an existing SSH public key and append it to a chosen Linux user's
 * `authorized_keys`. This is the "I already have a key (laptop, yubikey,
 * teammate) — let them in as <user>" flow, distinct from Generate keypair
 * which mints a fresh server-side keypair.
 *
 * The user dropdown lists every Linux account with a real login shell, so
 * a key can be assigned to root, the apps user, a freshly-created
 * developer account, or any operator account. Service users with
 * `/usr/sbin/nologin` are filtered out at the page level — putting a key
 * in their authorized_keys looks like it works but sshd refuses the
 * session.
 */
export default function AddPublicKeyForm({
  action,
  users,
}: {
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
  users: SshUserOption[];
}) {
  const [state, formAction, pending] = useActionState<Result | null, FormData>(
    action,
    null,
  );
  const formRef = useRef<HTMLFormElement>(null);

  useEffect(() => {
    if (state?.ok === true) formRef.current?.reset();
  }, [state]);

  return (
    <form ref={formRef} action={formAction}>
      <div className={styles.formRow}>
        <label className={styles.field}>
          User
          <select
            name="user"
            className={styles.select}
            defaultValue={users[0]?.name}
          >
            {users.map((u) => (
              <option key={u.name} value={u.name}>
                {u.name} — {u.home}
                {u.sudo ? " · sudo" : ""}
              </option>
            ))}
          </select>
        </label>
        <label className={styles.field}>
          Label (optional)
          <input
            name="label"
            className={styles.input}
            placeholder="alice@laptop"
            autoComplete="off"
          />
          <span className={styles.muted} style={{ fontSize: 12, marginTop: 4 }}>
            Appended as a comment after the key so you can tell entries apart.
          </span>
        </label>
      </div>
      <label className={styles.field} style={{ marginBottom: 12 }}>
        Public key
        <textarea
          name="key"
          className={styles.textarea}
          placeholder="ssh-ed25519 AAAA... user@host"
          required
          spellCheck={false}
          rows={3}
          style={{ fontFamily: "var(--mono)", fontSize: 12 }}
        />
        <span className={styles.muted} style={{ fontSize: 12, marginTop: 4 }}>
          A single public key line (<span className={styles.code}>ssh-ed25519</span>,{" "}
          <span className={styles.code}>ssh-rsa</span>,{" "}
          <span className={styles.code}>ecdsa-…</span>). The agent validates it
          with <span className={styles.code}>ssh-keygen</span> before writing.
        </span>
      </label>

      {state?.ok === false && (
        <div className={styles.error} style={{ marginBottom: 12 }}>
          {state.error}
        </div>
      )}
      {state?.ok === true && (
        <div className={styles.notice} style={{ marginBottom: 12 }}>
          Key added to authorized_keys.
        </div>
      )}

      <button type="submit" className={styles.btn} disabled={pending}>
        {pending ? "Adding..." : "Add public key"}
      </button>
    </form>
  );
}
