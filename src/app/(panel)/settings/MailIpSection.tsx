import { agent, AgentError, type MailIpSettings } from "@/lib/agent";
import styles from "@/components/page.module.css";
import {
  mailDomainDnsCheckAction,
  mailIpCheckAction,
  setMailDomainIpAction,
  setMailIpDefaultAction,
} from "@/server-actions/mail";
import local from "./mailIp.module.css";
import MailIpDefaultForm from "./MailIpDefaultForm";
import MailIpCheckPanel from "./MailIpCheckPanel";
import MailIpDomains from "./MailIpDomains";

/** The settings, or why they could not be read: no made-up defaults, or the
 *  form would save them. */
export async function loadMailIpSettings(): Promise<MailIpSettings | { error: string }> {
  try {
    return await agent.mail.ipSettings();
  } catch (e: unknown) {
    // An agent older than this setting answers with a bare 404.
    if (e instanceof AgentError && e.status === 404 && !e.body) {
      return { error: "the agent on this server does not have this setting yet. Update the panel." };
    }
    return { error: e instanceof Error ? e.message : String(e) };
  }
}

/** Settings > Mail: which IP protocol outgoing mail uses, server-wide and per domain. */
export default function MailIpSection({ settings }: { settings: MailIpSettings | { error: string } }) {
  return (
    <div className={styles.section} id="mail-ip">
      <h2 className={styles.sectionTitle}>Outgoing mail: IPv4 or IPv6</h2>
      <p className={styles.muted} style={{ marginBottom: 10 }}>
        The protocol this server tries first when it sends mail to other mail servers. When a
        receiving server can&apos;t be reached over it, the other one is used. Receiving mail is
        not affected: the server accepts mail over both.
      </p>
      {"error" in settings ? (
        <div className={styles.error}>
          Could not read the outgoing mail settings, so they can&apos;t be changed here:{" "}
          {settings.error}
        </div>
      ) : (
        <>
          <MailIpDefaultForm settings={settings} action={setMailIpDefaultAction} />

          <div className={local.block}>
            <h3 className={local.blockTitle}>Is each protocol ready to send?</h3>
            <p className={local.hint}>
              Looks up this server&apos;s addresses the way receiving mail servers do, reverse DNS
              included, and says what to fix. It takes a few seconds.
            </p>
            <MailIpCheckPanel action={mailIpCheckAction} />
          </div>

          <div className={local.block}>
            <h3 className={local.blockTitle}>Per domain</h3>
            <p className={local.hint}>
              Mail from an address at the domain goes out over the protocol chosen here. Check DNS
              looks at the domain&apos;s mail records for both protocols.
            </p>
            <MailIpDomains
              domains={settings.domains}
              defaultProtocol={settings.default_protocol}
              saveAction={setMailDomainIpAction}
              checkAction={mailDomainDnsCheckAction}
            />
          </div>
        </>
      )}
    </div>
  );
}
