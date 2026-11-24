import styles from "@/components/page.module.css";
import { loadPrivacyStatus, privacySettings, type PrivacySettings } from "@/lib/privacy";
import {
  privacyStatusAction,
  savePrivacyLogsAction,
  savePrivacyLookupsAction,
  savePrivacyRetentionAction,
  subjectEraseAction,
  subjectSearchAction,
} from "@/server-actions/privacy";
import PrivacyLogsForm from "./PrivacyLogsForm";
import PrivacyLookupsForm from "./PrivacyLookupsForm";
import PrivacyRequest from "./PrivacyRequest";
import PrivacyRetentionForm from "./PrivacyRetentionForm";
import { PrivacyStatusProvider, type StatusOrError } from "./PrivacyStatus";
import local from "./privacy.module.css";

const PRIVACY_DOC = "https://github.com/Meliorate-agency/vps-panel/blob/main/docs/PRIVACY.md";

export type PrivacyData = {
  /** As stored in panel.db: the panel reads these itself. */
  settings: PrivacySettings;
  /** What the agent says holds on the server, or why it could not be read. */
  status: StatusOrError;
};

export async function loadPrivacy(): Promise<PrivacyData> {
  return { settings: privacySettings(), status: await loadPrivacyStatus() };
}

/**
 * Settings > Privacy: what the web server logs, how long logs are kept, the
 * lookups that send addresses to outside services, and the search-and-erase
 * tool for a request about one person. That last card holds personal data
 * and is for admins: nothing of it is rendered for anyone else.
 */
export default function PrivacySection({ data, isAdmin }: { data: PrivacyData; isAdmin: boolean }) {
  const { settings, status } = data;
  return (
    <PrivacyStatusProvider initial={status} checkAction={privacyStatusAction}>
      <p className={local.intro}>
        Which personal data this server stores, where, for how long, and what leaves the server is written down
        in{" "}
        <a href={PRIVACY_DOC} target="_blank" rel="noreferrer noopener">
          What the server stores
        </a>
        . Nothing here changes until you save it: every setting starts as the server has always worked.
      </p>

      <div className={styles.section} id="privacy-logs">
        <h2 className={styles.sectionTitle}>Website logs</h2>
        <p className={styles.muted} style={{ marginBottom: 10 }}>
          The web server writes a line to its access log for every request to a site on this server: the
          visitor&apos;s IP address, the address requested and the page the visitor came from. Choose how much
          of that is kept. It applies to lines written from now on.
        </p>
        <PrivacyLogsForm logIp={settings.log_ip} logQuery={settings.log_query} action={savePrivacyLogsAction} />
      </div>

      <div className={styles.section} id="privacy-retention">
        <h2 className={styles.sectionTitle}>Log retention</h2>
        <p className={styles.muted} style={{ marginBottom: 10 }}>
          How long logs that hold personal data are kept before they are deleted: the web server&apos;s logs,
          the system journal with the mail and SSH logs, the other system logs in /var/log (mail, sign-in,
          firewall and ban logs), the panel&apos;s audit log, its sign-in records and the task history. Sign-in records and task history never stay longer than 30 days, whatever is chosen
          here.
        </p>
        <PrivacyRetentionForm days={settings.retention_days} action={savePrivacyRetentionAction} />
      </div>

      <div className={styles.section} id="privacy-lookups">
        <h2 className={styles.sectionTitle}>Outside lookups</h2>
        <p className={styles.muted} style={{ marginBottom: 10 }}>
          My Networks shows where each server of a network is. To find that out, the panel sends the public IP
          address of every server in your networks, this one included, to{" "}
          <span className={styles.code}>ipinfo.io</span>, and to <span className={styles.code}>get.geojs.io</span>{" "}
          when ipinfo.io does not answer. It does so when My Networks is opened and keeps the answer for a day.
          Addresses of visitors and users are never sent.
        </p>
        <PrivacyLookupsForm serverGeo={settings.server_geo} action={savePrivacyLookupsAction} />
        <p className={styles.muted} style={{ marginTop: 10, fontSize: 12 }}>
          Not part of this switch: when the panel hostname does not lead to a public address, the panel asks{" "}
          <span className={styles.code}>api.ipify.org</span> which address this server has. That request
          carries no data; the service sees the server&apos;s own address.
        </p>
      </div>

      {isAdmin && (
        <div className={styles.section} id="privacy-request">
          <h2 className={styles.sectionTitle}>Personal data request</h2>
          <p className={styles.muted} style={{ marginBottom: 10 }}>
            For when someone asks what this server holds about them, or asks for it to be erased. Search by
            their email address or IP address: the web server&apos;s logs, the system journal and the other
            logs in /var/log, fail2ban&apos;s bans, mailboxes, forwarders, stored mail and the mail queue, the
            task logs, and the panel&apos;s own accounts, sign-in records and audit log. Not searched: what
            your apps keep in their own files and databases, and backups.
          </p>
          <PrivacyRequest searchAction={subjectSearchAction} eraseAction={subjectEraseAction} />
        </div>
      )}
    </PrivacyStatusProvider>
  );
}
