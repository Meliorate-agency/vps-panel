"use client";

import { useActionState, useState } from "react";
import styles from "@/components/page.module.css";
import type { SshUserOption } from "./AddPublicKeyForm";

type Result =
  | {
      ok: true;
      publicKey: string;
      privateKeyPath: string;
      publicKeyPath: string;
    }
  | { ok: false; error: string };

export default function GenerateKeypairForm({
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
  // Controlled so we can sanitize keystrokes — strip uppercase/spaces and
  // reject invalid chars before they even reach the server validator.
  const [name, setName] = useState("");
  // root first: the common reason to mint a key here is a private app
  // repository, and app repositories are mirrored as root. A keypair made
  // for another account puts its Host alias in that account's ~/.ssh/config,
  // where the deploy never looks.
  const [user, setUser] = useState(
    () => users.find((u) => u.name === "root")?.name ?? users[0]?.name ?? "",
  );

  const sanitize = (raw: string) =>
    raw
      .toLowerCase()
      .replace(/\s+/g, "")
      .replace(/[^a-z0-9_-]/g, "")
      .slice(0, 32);

  return (
    <form action={formAction}>
      <div className={styles.formRow}>
        <label className={styles.field}>
          User
          <select
            name="user"
            className={styles.select}
            value={user}
            onChange={(e) => setUser(e.target.value)}
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
          Keypair name
          <input
            name="name"
            className={styles.input}
            value={name}
            onChange={(e) => setName(sanitize(e.target.value))}
            placeholder="github_vps_panel"
            required
            autoComplete="off"
            pattern="^[a-z0-9][a-z0-9_-]{0,30}[a-z0-9]$"
            title="lowercase letters, digits, _ or -; 2-32 chars; no spaces"
          />
          <span className={styles.muted} style={{ fontSize: 12, marginTop: 4 }}>
            Becomes the file name in{" "}
            <span className={styles.code}>~/.ssh/{name || "<name>"}</span>{" "}
            and the <span className={styles.code}>Host</span> alias in{" "}
            <span className={styles.code}>~/.ssh/config</span>. Forced
            lowercase, no spaces.
          </span>
        </label>
      </div>

      <p className={styles.muted} style={{ fontSize: 12, marginBottom: 10 }}>
        Generates an <span className={styles.code}>ed25519</span> keypair with{" "}
        <span className={styles.code}>ssh-keygen</span>, appends the public
        half to <span className={styles.code}>authorized_keys</span>, and adds
        a <span className={styles.code}>Host {name || "<name>"}</span>{" "}
        block to <span className={styles.code}>~/.ssh/config</span> pointing at{" "}
        <span className={styles.code}>github.com</span> (use{" "}
        <span className={styles.code}>git@{name || "<name>"}:owner/repo</span>{" "}
        for git remotes).
      </p>

      {user !== "root" && (
        <p className={styles.notice} style={{ fontSize: 12, marginBottom: 10 }}>
          For an app&apos;s Repository URL, generate this for{" "}
          <span className={styles.code}>root</span>. App repositories are
          mirrored as root, so a{" "}
          <span className={styles.code}>Host</span> alias written into{" "}
          <span className={styles.code}>
            {users.find((u) => u.name === user)?.home ?? `~${user}`}/.ssh/config
          </span>{" "}
          is invisible to the deploy - it fails with &quot;Could not resolve
          hostname&quot;. Keys for logging in as{" "}
          <span className={styles.code}>{user}</span> belong here either way.
        </p>
      )}

      {state?.ok === false && <div className={styles.error}>{state.error}</div>}
      {state?.ok === true && (
        <div className={styles.notice} style={{ marginTop: 12 }}>
          <div style={{ marginBottom: 8, fontWeight: 600 }}>
            Keypair generated.
          </div>
          <div style={{ fontSize: 12, marginBottom: 6 }}>
            Private key:{" "}
            <span className={styles.code}>{state.privateKeyPath}</span>{" "}
            (chmod 600)
          </div>
          <div style={{ fontSize: 12, marginBottom: 8 }}>
            Public key:{" "}
            <span className={styles.code}>{state.publicKeyPath}</span>
          </div>
          <div style={{ fontSize: 12, marginBottom: 6 }}>
            Public key — paste this into GitHub → Settings → SSH and GPG keys →
            New SSH key (or as a Deploy Key on a specific repo):
          </div>
          <pre
            style={{
              background: "var(--bg-elev-2)",
              border: "1px solid var(--border)",
              borderRadius: 6,
              padding: 10,
              fontSize: 11,
              fontFamily: "var(--mono)",
              whiteSpace: "pre-wrap",
              wordBreak: "break-all",
              margin: 0,
            }}
          >
            {state.publicKey.trim()}
          </pre>
        </div>
      )}

      <button
        type="submit"
        className={styles.btn}
        disabled={pending || name.length < 2}
        style={{ marginTop: 12 }}
      >
        {pending ? "Generating..." : "Generate keypair"}
      </button>
    </form>
  );
}
