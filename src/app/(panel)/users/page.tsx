import { agent } from "@/lib/agent";
import { getSession } from "@/lib/auth";
import Table from "@/components/Table/Table";
import StatusBadge from "@/components/StatusBadge/StatusBadge";
import styles from "@/components/page.module.css";
import {
  createPanelUserAction,
  createSystemUserAction,
  deletePanelUserAction,
  deleteSystemUserAction,
  listPanelUsers,
  listSystemUsers,
  resetPanelUserPasswordAction,
  setRootPasswordAction,
  setSystemUserPasswordAction,
} from "@/server-actions/users";
import FormDialogButton from "@/components/FormDialogButton/FormDialogButton";
import RootPasswordForm from "./RootPasswordForm";
import SshAvailabilityNotice from "./SshAvailabilityNotice";
import AddPanelUserForm from "./AddPanelUserForm";
import AddSystemUserForm from "./AddSystemUserForm";
import PanelUserActions from "./PanelUserActions";
import SystemUserActions from "./SystemUserActions";
import users from "./users.module.css";

export const dynamic = "force-dynamic";

export default async function UsersPage() {
  // Read live sshd state so the notice below reflects what's actually
  // configured right now — not a stale "install.sh disabled SSH password"
  // assumption that becomes wrong as soon as the operator flips the
  // Settings → SSH server toggles.
  const sshConfig = await agent.ssh.getConfig().catch(() => null);

  const [panelUsers, linuxUsers, session] = await Promise.all([
    listPanelUsers(),
    listSystemUsers(),
    getSession(),
  ]);
  const currentUserId = session?.userId ?? -1;
  // Split the agent's list into human accounts (root + UID >= 1000) and the
  // rest (service users like www-data, redis, postgres). Only the human
  // group renders Reset/Delete actions; service users are read-only so an
  // accidental click can't break a system service.
  const humanLinuxUsers = linuxUsers.filter((u) => u.is_human);
  const serviceLinuxUsers = linuxUsers.filter((u) => !u.is_human);

  return (
    <div>
      <div className={styles.head}>
        <div>
          <h1 className={styles.title}>Users</h1>
          <div className={styles.subtitle}>
            Panel-login accounts and host-side root password.
          </div>
        </div>
      </div>

      <div className={styles.section}>
        <div className={users.cardHead}>
          <div>
            <h2 className={styles.sectionTitle}>Panel users</h2>
            <p className={styles.muted}>
              Accounts that can sign in to this panel at{" "}
              <span className={styles.code}>/login</span>. Stored in{" "}
              <span className={styles.code}>panel.db</span> with argon2id-hashed
              passwords.
            </p>
          </div>
          <FormDialogButton label="+ Add panel user" title="Add panel user">
            <p className={styles.muted} style={{ marginBottom: 12 }}>
              A new login account, able to sign in at{" "}
              <span className={styles.code}>/login</span> right away. Pick{" "}
              <span className={styles.code}>admin</span> for full panel access
              including user management, or <span className={styles.code}>user</span>{" "}
              for a regular operator account.
            </p>
            <AddPanelUserForm action={createPanelUserAction} />
          </FormDialogButton>
        </div>
        <Table
          columns={[
            { key: "email", label: "Email" },
            { key: "role", label: "Role", width: "100px" },
            { key: "totp", label: "TOTP", width: "80px" },
            { key: "created", label: "Created", width: "180px" },
            { key: "actions", label: "", width: "220px" },
          ]}
          rows={panelUsers.map((u) => {
            const isSelf = u.id === currentUserId;
            return {
              email: (
                <span>
                  <strong>{u.email}</strong>
                  {isSelf && (
                    <span className={styles.muted} style={{ marginLeft: 6 }}>
                      (you)
                    </span>
                  )}
                </span>
              ),
              role: (
                <StatusBadge
                  text={u.role}
                  tone={u.role === "admin" ? "ok" : "muted"}
                />
              ),
              totp: u.totp_enabled ? (
                <StatusBadge text="on" tone="ok" />
              ) : (
                <StatusBadge text="off" tone="muted" />
              ),
              created: (
                <span className={styles.muted}>{u.created_at}</span>
              ),
              actions: isSelf ? (
                <span
                  className={styles.muted}
                  title="Use /settings to change your own password / email"
                  style={{ fontSize: 12 }}
                >
                  self — manage in /settings
                </span>
              ) : (
                <PanelUserActions
                  userId={u.id}
                  email={u.email}
                  role={u.role}
                  resetAction={resetPanelUserPasswordAction.bind(null, u.id)}
                  deleteAction={deletePanelUserAction.bind(null, u.id)}
                />
              ),
            };
          })}
          empty="No panel users yet."
        />
      </div>

      <div className={styles.section}>
        <div className={users.cardHead}>
          <div>
            <h2 className={styles.sectionTitle}>Linux users</h2>
            <p className={styles.muted}>
              Real OS-level accounts on this host (root +{" "}
              <span className={styles.code}>UID ≥ 1000</span>). The agent reads{" "}
              <span className={styles.code}>/etc/passwd</span> directly, so
              accounts added with{" "}
              <span className={styles.code}>adduser</span> at a shell appear here
              too.
            </p>
          </div>
          <FormDialogButton label="+ Add Linux user" title="Add Linux user">
            <AddSystemUserForm action={createSystemUserAction} />
          </FormDialogButton>
        </div>
        <Table
          columns={[
            { key: "name", label: "Username" },
            { key: "uid", label: "UID", width: "70px" },
            { key: "home", label: "Home" },
            { key: "shell", label: "Shell", width: "120px" },
            { key: "sudo", label: "sudo", width: "70px" },
            { key: "actions", label: "", width: "220px" },
          ]}
          rows={humanLinuxUsers.map((u) => {
            const isRoot = u.name === "root";
            return {
              name: (
                <span>
                  <strong>{u.name}</strong>
                  {isRoot && (
                    <span className={styles.muted} style={{ marginLeft: 6 }}>
                      (system root)
                    </span>
                  )}
                </span>
              ),
              uid: <span className={styles.muted}>{u.uid}</span>,
              home: <span className={styles.code}>{u.home}</span>,
              shell: <span className={styles.code}>{u.shell || "—"}</span>,
              sudo: u.sudo ? (
                <StatusBadge text="yes" tone="ok" />
              ) : (
                <StatusBadge text="no" tone="muted" />
              ),
              actions: isRoot ? (
                <FormDialogButton
                  label="Reset password"
                  title="Set the root password"
                  className={styles.btnGhost}
                  buttonStyle={{ fontSize: 12, padding: "4px 10px" }}
                >
                  <p className={styles.muted} style={{ marginBottom: 12 }}>
                    Resets the OS-level <span className={styles.code}>root</span>{" "}
                    password (<span className={styles.code}>/etc/shadow</span>). Useful
                    when the password for the Hetzner / OVH web console is lost: that
                    console authenticates against{" "}
                    <span className={styles.code}>/etc/shadow</span> through PAM and
                    accepts the new value.
                  </p>
                  <SshAvailabilityNotice config={sshConfig} />
                  <RootPasswordForm action={setRootPasswordAction} />
                </FormDialogButton>
              ) : u.reserved ? (
                <span
                  className={styles.muted}
                  title="Reserved system account: the panel does not reset or delete it"
                  style={{ fontSize: 12 }}
                >
                  reserved
                </span>
              ) : (
                <SystemUserActions
                  name={u.name}
                  home={u.home}
                  resetAction={setSystemUserPasswordAction.bind(null, u.name)}
                  deleteAction={deleteSystemUserAction.bind(null, u.name)}
                />
              ),
            };
          })}
          empty="No Linux user accounts beyond root yet."
        />

        {serviceLinuxUsers.length > 0 && (
          <details style={{ marginTop: 16 }}>
            <summary
              className={styles.muted}
              style={{ cursor: "pointer", fontSize: 12 }}
            >
              Service accounts ({serviceLinuxUsers.length}) — read-only
            </summary>
            <div style={{ marginTop: 10 }}>
              <Table
                columns={[
                  { key: "name", label: "Username" },
                  { key: "uid", label: "UID", width: "70px" },
                  { key: "home", label: "Home" },
                  { key: "shell", label: "Shell", width: "120px" },
                ]}
                rows={serviceLinuxUsers.map((u) => ({
                  name: <strong>{u.name}</strong>,
                  uid: <span className={styles.muted}>{u.uid}</span>,
                  home: <span className={styles.code}>{u.home}</span>,
                  shell: <span className={styles.code}>{u.shell || "—"}</span>,
                }))}
                empty="—"
              />
            </div>
          </details>
        )}
      </div>

    </div>
  );
}
