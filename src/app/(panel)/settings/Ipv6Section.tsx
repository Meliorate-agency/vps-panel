import Link from "next/link";
import { agent, AgentError, type Ipv6Status } from "@/lib/agent";
import styles from "@/components/page.module.css";
import { saveIpv6SettingsAction } from "@/server-actions/ipv6";
import mail from "./mailIp.module.css";
import local from "./ipv6.module.css";
import Ipv6Form from "./Ipv6Form";

/** The status, or why it could not be read: no made-up defaults, or the form
 *  would save them. */
export async function loadIpv6Status(): Promise<Ipv6Status | { error: string }> {
  try {
    return await agent.ipv6.status();
  } catch (e: unknown) {
    // An agent older than this setting answers with a bare 404.
    if (e instanceof AgentError && e.status === 404 && !e.body) {
      return { error: "the agent on this server does not have this setting yet. Update the panel." };
    }
    return { error: e instanceof Error ? e.message : String(e) };
  }
}

/** Settings > DNS & SSL > IPv6: the address domains publish, and the default
 *  for new domains. Each domain is switched on its own page. */
export default function Ipv6Section({ status }: { status: Ipv6Status | { error: string } }) {
  return (
    <div className={styles.section} id="ipv6">
      <h2 className={styles.sectionTitle}>IPv6</h2>
      <p className={styles.muted} style={{ marginBottom: 10 }}>
        The IPv6 address of this server that domains with IPv6 on publish in DNS: AAAA records for the
        domain, <span className={styles.code}>www</span>, <span className={styles.code}>mail</span> and its
        sites, and the address in the domain&apos;s SPF record. IPv6 is switched on or off for each domain on
        its page under <Link href="/domains">Domains</Link>; a domain with it off has no IPv6 records.
      </p>
      {"error" in status ? (
        <div className={styles.error}>
          Could not read the IPv6 settings, so they can&apos;t be changed here: {status.error}
        </div>
      ) : (
        <>
          <InEffect status={status} />
          <Ipv6Form status={status} action={saveIpv6SettingsAction} />
        </>
      )}
    </div>
  );
}

/** The address in effect and where it comes from, or what to do without one. */
function InEffect({ status }: { status: Ipv6Status }) {
  const { address, source, candidates, setting, domains_on, domains_total } = status;
  let line: React.ReactNode;
  if (address) {
    const from =
      source === "setting"
        ? "You chose it below."
        : source === "hostname"
          ? "Found automatically: the panel hostname's AAAA record points to it."
          : source === "interface"
            ? "Found automatically: it is the only public IPv6 address on this server's network interfaces."
            : null;
    line = (
      <>
        <p>
          In effect: <span className={styles.code}>{address}</span>
        </p>
        {from && <p className={mail.sub}>{from}</p>}
      </>
    );
  } else if (candidates.length === 0) {
    line = (
      <>
        <p>This server has no public IPv6 address, so no domain can have IPv6.</p>
        <p className={mail.sub}>
          Most hosting providers give a server an IPv6 address, often as an option to switch on in the
          server&apos;s network settings at the provider. Once the address is on the server, reload this page.
        </p>
      </>
    );
  } else if (setting === "") {
    line = (
      <>
        <p>No address in effect: this server has several public IPv6 addresses.</p>
        <p className={mail.sub}>
          Automatic only picks one when the panel hostname&apos;s AAAA record points to it or the server has
          just one. Choose the address domains publish below.
        </p>
      </>
    );
  } else {
    line = (
      <>
        <p>
          No address in effect: <span className={styles.code}>{setting}</span>, the address chosen below, is
          not on this server&apos;s network interfaces now.
        </p>
        <p className={mail.sub}>Choose another one, or Automatic.</p>
      </>
    );
  }
  return (
    <div className={`${local.state} ${address ? "" : local.stateWarn}`}>
      {line}
      <p className={mail.sub}>
        IPv6 is on for {domains_on} of {domains_total} domain{domains_total === 1 ? "" : "s"}.
      </p>
    </div>
  );
}
