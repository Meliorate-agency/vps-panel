import { agent, type KeyRow, type SystemUserRow } from "@/lib/agent";
import Table from "@/components/Table/Table";
import StatusBadge from "@/components/StatusBadge/StatusBadge";
import styles from "@/components/page.module.css";
import {
  addSshKeyAction,
  fetchAuthorizedKeysFileAction,
  fetchSshConfigFileAction,
  fetchSshPrivateKeyAction,
  fetchSshPublicKeyAction,
  generateSshKeypairAction,
  removeSshKeyAction,
  saveAuthorizedKeysFileAction,
  saveSshConfigFileAction,
  updateSshPrivateKeyAction,
  updateSshPublicKeyAction,
} from "@/server-actions/ssh";
import GenerateKeypairForm from "./GenerateKeypairForm";
import AddPublicKeyForm, { type SshUserOption } from "./AddPublicKeyForm";
import KeyActionsMenu from "./KeyActionsMenu";
import SshFileButton from "./SshFileButton";

export const dynamic = "force-dynamic";

type UserKeys = { user: SystemUserRow; keys: KeyRow[] };

// Always give the operator at least root if the agent's /system/users probe
// fails (socket down, /etc/passwd unreadable) so the page still renders a
// usable surface instead of an empty one.
const ROOT_FALLBACK: SystemUserRow = {
  name: "root",
  uid: 0,
  home: "/root",
  shell: "/bin/bash",
  is_human: true,
  sudo: false,
  reserved: true,
};

export default async function SshKeysPage() {
  // Every Linux account the agent surfaces (root + UID >= 33, minus the
  // service blocklist), so a key can be assigned to any of them — not just
  // the old hardcoded apps/root pair.
  const users = await agent.system
    .users()
    .then((list) => (list.length > 0 ? list : [ROOT_FALLBACK]))
    .catch((e) => {
      // Don't fail the page — degrade to a root-only view. Log so the failure
      // is diagnosable in the panel's journal (journalctl -u vps-panel)
      // instead of silently looking like "this host only has root".
      console.error("ssh-keys: agent.system.users() failed:", e);
      return [ROOT_FALLBACK];
    });

  // Pull each user's authorized_keys in parallel. A per-user failure degrades
  // to an empty list for that user rather than failing the whole page — but
  // we log it so an empty section caused by a socket/permission error is
  // distinguishable (in the journal) from a user that genuinely has no keys.
  const lists: UserKeys[] = await Promise.all(
    users.map(async (user) => ({
      user,
      keys: await agent.ssh.list(user.name).catch((e) => {
        console.error(`ssh-keys: agent.ssh.list(${user.name}) failed:`, e);
        return [] as KeyRow[];
      }),
    })),
  );

  // Form dropdowns (Generate keypair, Add existing public key) only list
  // accounts that can actually log in over SSH — putting a key in a
  // `/usr/sbin/nologin` user's authorized_keys is a footgun: it appears
  // to "work" but sshd refuses the session because the login shell
  // rejects it. The full `users` list is still used below to render
  // existing key tables for any account, including service users that
  // already have rows in authorized_keys (so we don't silently hide
  // pre-existing assignments). If filtering empties the list, fall back
  // to the unfiltered set so the operator still has something to pick.
  const loginShellUsers = users.filter(
    (u) => !u.shell.endsWith("nologin") && !u.shell.endsWith("false"),
  );
  const userOptions: SshUserOption[] = (
    loginShellUsers.length > 0 ? loginShellUsers : users
  ).map((u) => ({
    name: u.name,
    home: u.home,
    shell: u.shell,
    sudo: u.sudo,
  }));

  // Expanded by default: anything the operator is likely to manage — root,
  // the apps runner, real human accounts, and any account that already has
  // keys. Everything else (idle service accounts with no keys) is tucked
  // into a collapsed section so the page doesn't become a wall of empty
  // tables, while still being reachable for assignment.
  const isPrimary = (uk: UserKeys) =>
    uk.user.is_human || uk.user.name === "apps" || uk.keys.length > 0;
  const primary = lists.filter(isPrimary);
  const secondary = lists.filter((uk) => !isPrimary(uk));

  return (
    <div>
      <div className={styles.head}>
        <div>
          <h1 className={styles.title}>SSH keys</h1>
          <div className={styles.subtitle}>
            Login keys (authorized_keys) and keypairs for any Linux user on this host.
          </div>
        </div>
      </div>

      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>Generate keypair</h2>
        <GenerateKeypairForm
          action={generateSshKeypairAction}
          users={userOptions}
        />
      </div>

      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>Add existing public key</h2>
        <p className={styles.muted} style={{ marginBottom: 12 }}>
          Paste a public key you already have (laptop, hardware key, a
          teammate) and grant it login as the selected user. For a brand-new
          key minted on the server, use Generate keypair above instead.
        </p>
        <AddPublicKeyForm action={addSshKeyAction} users={userOptions} />
      </div>

      {primary.map((uk) => (
        <UserKeysSection key={uk.user.name} entry={uk} />
      ))}

      {secondary.length > 0 && (
        <div className={styles.section}>
          <details>
            <summary
              className={styles.muted}
              style={{ cursor: "pointer", fontSize: 13 }}
            >
              Other accounts ({secondary.length}) — service users with no keys
            </summary>
            <div style={{ marginTop: 12, display: "grid", gap: 12 }}>
              {secondary.map((uk) => (
                <UserKeysSection key={uk.user.name} entry={uk} bare />
              ))}
            </div>
          </details>
        </div>
      )}
    </div>
  );
}

/**
 * One user's authorized_keys block: header (with home + sudo badge + the
 * raw-file editor buttons) and a table of keys with per-row actions.
 *
 * `bare` drops the outer `.section` card so this can be nested inside the
 * collapsed "Other accounts" details without a card-in-card look.
 */
function UserKeysSection({
  entry,
  bare = false,
}: {
  entry: UserKeys;
  bare?: boolean;
}) {
  const { user, keys } = entry;
  const inner = (
    <>
      <div
        style={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          gap: 12,
          marginBottom: 12,
          flexWrap: "wrap",
        }}
      >
        <h2 className={styles.sectionTitle} style={{ margin: 0 }}>
          {user.name}
          <span
            className={styles.muted}
            style={{ marginLeft: 8, fontWeight: 400, fontSize: 12 }}
          >
            {user.home}
            {user.sudo ? " · sudo" : ""}
          </span>
        </h2>
        <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
          <SshFileButton
            user={user.name}
            filePath="~/.ssh/authorized_keys"
            label="View authorized_keys"
            fetchAction={fetchAuthorizedKeysFileAction}
            saveAction={saveAuthorizedKeysFileAction}
          />
          <SshFileButton
            user={user.name}
            filePath="~/.ssh/config"
            label="View config"
            fetchAction={fetchSshConfigFileAction}
            saveAction={saveSshConfigFileAction}
          />
        </div>
      </div>
      <Table
        columns={[
          { key: "fp", label: "Fingerprint" },
          { key: "type", label: "Type", width: "100px" },
          { key: "comment", label: "Comment" },
          { key: "actions", label: "", width: "60px" },
        ]}
        rows={keys.map((k) => ({
          fp: <span className={styles.code}>{k.fingerprint}</span>,
          type: k.key_type,
          comment: k.authorized ? (
            <span className={styles.muted}>{k.comment}</span>
          ) : (
            // A keypair in ~/.ssh that is not in authorized_keys: a deploy
            // key, say. Nothing here makes it a login key.
            <span
              style={{ display: "inline-flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}
              title={`In ~/.ssh but not in authorized_keys: this server can use it to reach other hosts (a deploy key, for example), but it can't be used to log in as ${user.name}.`}
            >
              <span className={styles.muted}>{k.comment || k.keypair_name}</span>
              <StatusBadge text="can't log in" tone="muted" />
            </span>
          ),
          actions: (
            <div style={{ textAlign: "right" }}>
              <KeyActionsMenu
                user={user.name}
                fingerprint={k.fingerprint}
                keyType={k.key_type}
                comment={k.comment}
                keypairName={k.keypair_name}
                authorized={k.authorized}
                fetchPublic={fetchSshPublicKeyAction}
                fetchPrivate={fetchSshPrivateKeyAction}
                savePublic={updateSshPublicKeyAction}
                savePrivate={updateSshPrivateKeyAction}
                deleteAction={removeSshKeyAction}
              />
            </div>
          ),
        }))}
        empty={`No keys for ${user.name}.`}
      />
    </>
  );

  if (bare) return <div>{inner}</div>;
  return <div className={styles.section}>{inner}</div>;
}
