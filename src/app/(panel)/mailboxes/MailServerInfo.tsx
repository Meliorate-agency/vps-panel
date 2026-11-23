import Link from "next/link";
import styles from "@/components/page.module.css";
import type { MailIpSettings } from "@/lib/agent";
import localStyles from "./mailboxes.module.css";

const PROTOCOL_NAME = { ipv4: "IPv4", ipv6: "IPv6" } as const;

export default function MailServerInfo({
  server,
  mailIp,
}: {
  server: string;
  /** null when the agent could not say; the line is left out then. */
  mailIp: MailIpSettings | null;
}) {
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
      {mailIp && <OutgoingProtocol settings={mailIp} />}
    </div>
  );
}

/** One line: which protocol outgoing mail uses, and where to change it. */
function OutgoingProtocol({ settings }: { settings: MailIpSettings }) {
  const first = settings.default_protocol;
  const other = first === "ipv4" ? "ipv6" : "ipv4";
  const differ = (settings.domains ?? []).filter((d) => d.effective !== first).length;
  const noV6 = first === "ipv4" && !settings.server_ipv6 && differ === 0;
  return (
    <p className={localStyles.muted} style={{ margin: "10px 0 0" }}>
      {noV6 ? (
        <>Outgoing mail goes out over IPv4; this server has no public IPv6 address. </>
      ) : settings.ipv6_active === false ? (
        // IPv6 has not passed its check yet: IPv4 only, whatever is chosen.
        <>Outgoing mail goes out over IPv4 only; IPv6 is not switched on yet. </>
      ) : (
        <>
          Outgoing mail goes out over {PROTOCOL_NAME[first]} first, {PROTOCOL_NAME[other]} as the
          fallback
          {differ > 0 &&
            `, except from ${differ} ${differ === 1 ? "domain" : "domains"} set to ${PROTOCOL_NAME[other]}`}
          .{" "}
        </>
      )}
      <Link href="/settings?tab=mail#mail-ip">Change in Settings</Link>
    </p>
  );
}
