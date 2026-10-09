import styles from "@/components/page.module.css";
import localStyles from "./mailboxes.module.css";

export default function MailServerInfo({ server }: { server: string }) {
  if (!server) return null;
  return (
    <div className={localStyles.serverInfo}>
      <h3 className={localStyles.serverInfoTitle}>
        Mail server connection settings (per domain)
      </h3>
      <p className={localStyles.muted} style={{ marginTop: 0, marginBottom: 10 }}>
        Each mailbox connects via <code className={styles.code}>mail.&lt;your-domain&gt;</code>{" "}
        — every mail domain published by the panel has its own{" "}
        <code className={styles.code}>mail.</code> A record pointing at this server. Use
        the Copy config button on each mailbox row for the exact values.
      </p>
      <table className={localStyles.serverInfoTable}>
        <tbody>
          <tr>
            <td>IMAP</td>
            <td>
              <code className={styles.code}>mail.&lt;your-domain&gt;:993</code>{" "}
              <span className={localStyles.proto}>SSL/TLS</span>
            </td>
          </tr>
          <tr>
            <td>SMTP</td>
            <td>
              <code className={styles.code}>mail.&lt;your-domain&gt;:587</code>{" "}
              <span className={localStyles.proto}>STARTTLS</span>
            </td>
          </tr>
          <tr>
            <td>POP3</td>
            <td>
              <code className={styles.code}>mail.&lt;your-domain&gt;:995</code>{" "}
              <span className={localStyles.proto}>SSL/TLS</span>
            </td>
          </tr>
          <tr>
            <td>Username</td>
            <td>
              <span className={localStyles.muted}>full email address</span>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  );
}
