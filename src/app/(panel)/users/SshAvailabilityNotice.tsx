import type { SshServerConfig } from "@/lib/agent";
import styles from "@/components/page.module.css";

export default function SshAvailabilityNotice({
  config,
  inSettings = false,
}: {
  config: SshServerConfig | null;
  inSettings?: boolean;
}) {
  if (!config) {
    return (
      <p className={styles.muted} style={{ marginBottom: 12, fontSize: 13 }}>
        Could not read the SSH server settings, so it is unknown whether SSH accepts
        this password.
      </p>
    );
  }
  const passwordAuth = config.password_auth;
  const rootSshUsable = passwordAuth && config.permit_root_login === "yes";

  if (rootSshUsable) {
    return (
      <div
        style={{
          marginBottom: 12,
          padding: "10px 12px",
          background: "rgba(74, 222, 128, 0.08)",
          border: "1px solid rgba(74, 222, 128, 0.28)",
          borderRadius: 6,
          fontSize: 13,
        }}
      >
        <strong>SSH will also accept this password.</strong>{" "}
        <span className={styles.code}>PasswordAuthentication</span> and{" "}
        <span className={styles.code}>PermitRootLogin</span> are both enabled
        on this host, so <span className={styles.code}>ssh root@host</span>{" "}
        with the new password will work in addition to web console login.
      </div>
    );
  }

  const reasons: string[] = [];
  if (!passwordAuth) reasons.push("PasswordAuthentication is off");
  if (config.permit_root_login !== "yes") {
    reasons.push(`PermitRootLogin is ${config.permit_root_login}`);
  }

  return (
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
      <strong>SSH won&apos;t accept this password.</strong>{" "}
      In the live SSH settings {reasons.join(" and ")}. This password is only
      for serial / web console login. To allow root SSH by password, turn on
      password login and set root login to Allowed{" "}
      {inSettings ? (
        "above"
      ) : (
        <>
          at{" "}
          <a href="/settings#ssh-server" className={styles.code}>/settings → SSH server</a>
        </>
      )}{" "}
      - or, more secure, add a key under{" "}
      <a href="/ssh-keys" className={styles.code}>/ssh-keys</a> and leave
      passwords off.
    </div>
  );
}
