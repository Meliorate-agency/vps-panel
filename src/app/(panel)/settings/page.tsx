import Link from "next/link";
import { changePasswordAction, changeAdminEmailAction } from "@/server-actions/auth";
import { getSession } from "@/lib/auth";
import {
  setPanelTimezoneAction,
  setSettingAction,
  setTabTitleAction,
  setUploadLimitAction,
} from "@/server-actions/settings";
import { DEFAULT_TAB_TITLE, TAB_TITLE_KEY, tabTitleOrDefault } from "@/lib/tabTitle";
import { setSshServerConfigAction } from "@/server-actions/ssh";
import { setRootPasswordAction } from "@/server-actions/users";
import { setAcmeEmailAction } from "@/server-actions/acme";
import {
  setThemeAction,
  saveThemeAsAction,
  updateSavedThemeAction,
  renameSavedThemeAction,
  deleteSavedThemeAction,
} from "@/server-actions/theme";
import {
  setPanelHostAction,
  completePanelHostSetupAction,
} from "@/server-actions/panelHost";
import {
  generateMcpTokenAction,
  clearMcpTokenAction,
  setMcpScopeAction,
} from "@/server-actions/mcpToken";
import { getSetting } from "@/lib/settings";
import {
  ACCENT_SETTING_KEY,
  THEME_SETTING_KEY,
  getThemeLibrary,
} from "@/lib/accent";
import { getMcpTokenInfo } from "@/lib/mcpToken";
import { agent } from "@/lib/agent";
import { config } from "@/lib/config";
import { ACCESS_TTL_SECONDS } from "@/lib/config";
import styles from "@/components/page.module.css";
import ChangePasswordForm from "./ChangePasswordForm";
import IpAllowlistForm from "./IpAllowlistForm";
import PanelAccessForm from "./PanelAccessForm";
import { setPanelAccessAction } from "@/server-actions/panelAccess";
import { PANEL_ACCESS_KEY } from "@/lib/panelAccess";
import { getClientIp } from "@/lib/getClientIp";
import SessionTimeoutForm from "./SessionTimeoutForm";
import SshServerForm from "./SshServerForm";
import RootPasswordForm from "../users/RootPasswordForm";
import SshAvailabilityNotice from "../users/SshAvailabilityNotice";
import AcmeEmailForm from "./AcmeEmailForm";
import NameserverPairForm from "./NameserverPairForm";
import PanelHostForm from "./PanelHostForm";
import AdminEmailForm from "./AdminEmailForm";
import TimezoneForm from "./TimezoneForm";
import UploadLimitForm from "./UploadLimitForm";
import { UPLOAD_LIMIT_MAX_MB, uploadLimitMb } from "@/lib/uploadLimit";
import ThemeForm from "./ThemeForm";
import TabTitleForm from "./TabTitleForm";
import McpTokenForm from "./McpTokenForm";
import SwapForm from "./SwapForm";
import { setSwapAction } from "@/server-actions/swap";
import HostnameForm from "./HostnameForm";
import { setServerHostnameAction } from "@/server-actions/hostname";
import { isIP } from "node:net";
import SettingsTabs from "./SettingsTabs";

export const dynamic = "force-dynamic";

export default async function SettingsPage() {
  const session = await getSession();
  const ipAllowlist = await getSetting("panel.ip_allowlist", "");
  const panelAccess = await getSetting(PANEL_ACCESS_KEY, "");
  const clientIp = await getClientIp();
  const defaultMinutes = String(ACCESS_TTL_SECONDS / 60);
  const sessionTtl = await getSetting("panel.session_ttl_minutes", defaultMinutes);
  const acmeEmail = await getSetting("panel.acme_email", "");
  const nsPair = await getSetting("dns.ns_pair", "");
  const panelTimezone = await getSetting("panel.timezone", "UTC");
  // ThemeForm parses this itself and tolerates anything, including the bare
  // hex the setting held before the dials and per-shade pins existed.
  const theme =
    (await getSetting(THEME_SETTING_KEY, "")) ||
    (await getSetting(ACCENT_SETTING_KEY, ""));
  const savedThemes = getThemeLibrary();
  const tabTitle = await getSetting(TAB_TITLE_KEY, "");
  // Metadata only: the token itself is stored as a hash.
  const mcpInfo = getMcpTokenInfo();
  const mcpEnvFallback = config.mcpToken !== null;
  // No made-up defaults on failure: a form showing guesses would save them.
  const sshConfig = await agent.ssh
    .getConfig()
    .catch((e: unknown) => ({ error: e instanceof Error ? e.message : String(e) }));
  const swap = await agent.system
    .swap()
    .catch((e: unknown) => ({ error: e instanceof Error ? e.message : String(e) }));
  const hostname = await agent.system
    .hostname()
    .catch((e: unknown) => ({ error: e instanceof Error ? e.message : String(e) }));
  // Offered as the server hostname in one click; not in IP-only mode.
  const panelHostName = config.panelHost && !isIP(config.panelHost) ? config.panelHost : null;
  const pdnsZones = await agent.dns.listZones().catch(() => []);
  // Cert status for the panel host so PanelHostForm can show "SSL · Nd"
  // instead of just an "Issue cert" button when a cert already exists.
  // Skip the lookup in IP-only mode (no FQDN to query).
  const panelHostCertStatus = config.panelHost
    ? await agent.sites
        .status(config.panelHost)
        .catch(() => ({
          cert_exists: false,
          cert_not_after: null,
          days_remaining: null,
        }))
    : null;
  return (
    <div>
      <div className={styles.head}>
        <div>
          <h1 className={styles.title}>Settings</h1>
          <div className={styles.subtitle}>Panel configuration and account.</div>
        </div>
      </div>

      <SettingsTabs
        tabs={[
        {
          id: "panel",
          label: "Panel",
          anchors: ["panel-host", "uploads"],
          content: (
            <>
            <div className={styles.section} id="panel-host">
                    <h2 className={styles.sectionTitle}>Panel hostname</h2>
                    <p className={styles.muted} style={{ marginBottom: 10 }}>
                      The FQDN this panel is accessible at. Leave blank for IP-only mode
                      (HTTP on <span className={styles.code}>http://{config.panelHost || "&lt;server-ip&gt;"}</span>{" "}
                      with non-Secure cookies). Setting an FQDN rewrites the nginx vhost,
                      Postfix <span className={styles.code}>myhostname</span>, Dovecot
                      <span className={styles.code}>postmaster_address</span>, and{" "}
                      <span className={styles.code}>/etc/vps-panel/.env</span>, then
                      schedules a panel restart. After it restarts you&apos;ll need to log
                      in again at the new URL. Issue HTTPS afterwards via SSL settings.{" "}
                      The panel also registers the hostname under{" "}
                      <span className={styles.code}>/domains</span>: it adds an{" "}
                      <span className={styles.code}>A</span> record to the managed parent
                      zone if one exists, otherwise it creates a new PowerDNS zone with the
                      standard seed records. This is the public web and mail name; the
                      server&apos;s own hostname (what <span className={styles.code}>hostname</span>{" "}
                      prints) is set separately under Server.
                    </p>
                    <PanelHostForm
                      value={config.panelHost}
                      saveAction={setPanelHostAction}
                      completeAction={completePanelHostSetupAction}
                      managedZones={pdnsZones.map((z) => z.name)}
                      certStatus={panelHostCertStatus}
                    />
                  </div>

            <div className={styles.section}>
                    <h2 className={styles.sectionTitle}>Panel timezone</h2>
                    <p className={styles.muted} style={{ marginBottom: 10 }}>
                      Controls how times are displayed in the top-bar clock and the
                      audit log&apos;s day grouping. The agent stores all timestamps in
                      UTC underneath — switching this only changes how they&apos;re
                      rendered, never the data on disk.
                    </p>
                    <TimezoneForm value={panelTimezone} action={setPanelTimezoneAction} />
                  </div>

            <div className={styles.section} id="uploads">
                    <h2 className={styles.sectionTitle}>Uploads</h2>
                    <p className={styles.muted} style={{ marginBottom: 10 }}>
                      The largest file one upload can carry, in Files and in SQL imports on
                      Databases. A bigger file is refused before anything is written. The server
                      takes at most {UPLOAD_LIMIT_MAX_MB} MB (5 GB) per file.
                    </p>
                    <UploadLimitForm
                      value={uploadLimitMb()}
                      max={UPLOAD_LIMIT_MAX_MB}
                      action={setUploadLimitAction}
                    />
                  </div>

            <div className={styles.section}>
                    <h2 className={styles.sectionTitle}>System</h2>
                    <ul style={{ display: "flex", flexDirection: "column", gap: 6, listStyle: "none" }}>
                      <li>
                        <span className={styles.muted}>Panel host:</span>{" "}
                        <span className={styles.code}>{config.panelHost}</span>
                      </li>
                      <li>
                        <span className={styles.muted}>Agent socket:</span>{" "}
                        <span className={styles.code}>{config.agentSocket}</span>
                      </li>
                      <li>
                        <span className={styles.muted}>Database:</span>{" "}
                        <span className={styles.code}>{config.panelDbPath}</span>
                      </li>
                    </ul>
                  </div>
            </>
          ),
        },
        {
          id: "server",
          label: "Server",
          anchors: ["hostname", "ssh-server", "swap"],
          content: (
            <>
            <div className={styles.section} id="hostname">
              <h2 className={styles.sectionTitle}>Server hostname</h2>
              <p className={styles.muted} style={{ marginBottom: 10 }}>
                The name the server calls itself: what{" "}
                <span className={styles.code}>hostname</span> prints, the SSH prompt and log
                lines. It is separate from the panel hostname, which is the public web and mail
                address, and changing it does not touch the web, mail or certificates. Most
                servers use the same name for both.
              </p>
              {"error" in hostname ? (
                <div className={styles.error}>
                  Could not read the hostname, so it can&apos;t be changed here: {hostname.error}
                </div>
              ) : (
                <HostnameForm
                  status={hostname}
                  panelHost={panelHostName}
                  action={setServerHostnameAction}
                />
              )}
            </div>

            <div className={styles.section} id="ssh-server">
              <h2 className={styles.sectionTitle}>SSH server</h2>
              <p className={styles.muted} style={{ marginBottom: 10 }}>
                Port, idle timeout, password login and root login for the SSH daemon. The values
                shown are what sshd is running with right now.
              </p>
              {"error" in sshConfig ? (
                <div className={styles.error}>
                  Could not read the SSH server settings, so they can&apos;t be edited here:{" "}
                  {sshConfig.error}
                </div>
              ) : (
                <SshServerForm config={sshConfig} action={setSshServerConfigAction} />
              )}
              <h3 className={styles.sectionTitle} style={{ marginTop: 20 }}>
                Root password
              </h3>
              <p className={styles.muted} style={{ marginBottom: 10 }}>
                Sets the password of the Linux <span className={styles.code}>root</span> account,
                like Reset password on the root row in{" "}
                <Link href="/users" className={styles.code}>/users</Link>. Your provider&apos;s
                web console accepts it straight away.
              </p>
              <SshAvailabilityNotice config={"error" in sshConfig ? null : sshConfig} inSettings />
              <RootPasswordForm action={setRootPasswordAction} />
            </div>

            <div className={styles.section} id="swap">
              <h2 className={styles.sectionTitle}>Swap</h2>
              <p className={styles.muted} style={{ marginBottom: 10 }}>
                Disk space Linux uses as extra memory. When the apps need more RAM than the server
                has, Linux moves memory nothing is using right now into the swap file instead of
                killing a process, so a build or a burst of traffic gets slower rather than
                failing. The disk is far slower than RAM: if swap is busy all the time, the server
                needs more RAM.
              </p>
              {"error" in swap ? (
                <div className={styles.error}>
                  Could not read the swap settings, so they can&apos;t be changed here: {swap.error}
                </div>
              ) : (
                <SwapForm status={swap} action={setSwapAction} />
              )}
            </div>
            </>
          ),
        },
        {
          id: "account",
          label: "Account",
          content: (
            <>
            <div className={styles.section}>
                    <h2 className={styles.sectionTitle}>Account</h2>
                    <p className={styles.muted} style={{ marginBottom: 10 }}>
                      Change the admin login email. The current value was seeded from{" "}
                      <span className={styles.code}>ADMIN_EMAIL</span> in{" "}
                      <span className={styles.code}>/etc/vps-panel/.env</span> at install
                      time; once you change it here, the .env value is no longer
                      authoritative. You&apos;ll be signed out and asked to log in with
                      the new email.
                    </p>
                    <AdminEmailForm
                      currentEmail={session?.email ?? ""}
                      action={changeAdminEmailAction}
                    />
                    <h3 className={styles.sectionTitle} style={{ marginTop: 20 }}>
                      Password
                    </h3>
                    <ChangePasswordForm action={changePasswordAction} />
                  </div>

            <div className={styles.section}>
                    <h2 className={styles.sectionTitle}>Session security</h2>
                    <p className={styles.muted} style={{ marginBottom: 10 }}>
                      How long the panel stays signed in without use. Clicks and key presses in an
                      open panel tab renew the session; after this long with no activity you are
                      signed out. Logins with Remember me last 30 days instead.
                    </p>
                    <SessionTimeoutForm
                      value={sessionTtl}
                      action={setSettingAction.bind(null, "panel.session_ttl_minutes")}
                    />
                  </div>
            </>
          ),
        },
        {
          id: "appearance",
          label: "Appearance",
          anchors: ["theme", "tab-name"],
          content: (
            <>
            <div className={styles.section} id="theme">
                    <h2 className={styles.sectionTitle}>Panel theme</h2>
                    <p className={styles.muted} style={{ marginBottom: 10 }}>
                      The whole palette is computed from one accent colour: buttons, links,
                      focus rings, surfaces, borders, text and the background wash move
                      together. The three dials control how loud that is &mdash; saturated
                      colour on a near-black page is what makes a theme glare, so turn{" "}
                      <span className={styles.code}>Chrome tint</span> and{" "}
                      <span className={styles.code}>Ambient glow</span> down for a calm,
                      mostly-grey panel with just the accent showing. Any individual shade
                      can be pinned to a fixed colour under &quot;Fine-tune each shade&quot;.
                      Name a combination you like and it joins the grid, ready to edit,
                      rename or delete. The preview is live; nothing is stored until you
                      save. Applies to every panel user, including the login screen.
                    </p>
                    <ThemeForm
                      value={theme}
                      savedThemes={savedThemes}
                      action={setThemeAction}
                      saveAsAction={saveThemeAsAction}
                      updateSavedAction={updateSavedThemeAction}
                      renameSavedAction={renameSavedThemeAction}
                      deleteSavedAction={deleteSavedThemeAction}
                    />
                  </div>

            <div className={styles.section} id="tab-name">
                    <h2 className={styles.sectionTitle}>Browser tab</h2>
                    <p className={styles.muted} style={{ marginBottom: 10 }}>
                      The name browsers show for this panel in tabs, bookmarks and history, handy
                      for telling your servers apart. Pages with a title of their own put it in
                      front, e.g. <span className={styles.code}>Sign in - {tabTitleOrDefault(tabTitle)}</span>.
                      The icon is the theme&apos;s bolt in its colours, so the preview follows the
                      theme above, saved or only previewed. Leave the name empty for{" "}
                      <span className={styles.code}>{DEFAULT_TAB_TITLE}</span>.
                    </p>
                    <TabTitleForm value={tabTitle} host={config.panelHost} action={setTabTitleAction} />
                  </div>
            </>
          ),
        },
        {
          id: "security",
          label: "Security",
          anchors: ["mcp-token"],
          content: (
            <>
            <div className={styles.section}>
                    <h2 className={styles.sectionTitle}>Panel access</h2>
                    <p className={styles.muted} style={{ marginBottom: 10 }}>
                      Only these IP addresses or ranges can open the panel, the MCP endpoint for AI agents
                      included. Leave it empty to allow every address. Your current address is{" "}
                      <span className={styles.code}>{clientIp}</span> and has to be in the list. Locked out
                      after your IP changed? On the server run{" "}
                      <span className={styles.code}>vps-panel-agent access-reset</span>.
                    </p>
                    <PanelAccessForm value={panelAccess} clientIp={clientIp} action={setPanelAccessAction} />
                  </div>

            <div className={styles.section}>
                    <h2 className={styles.sectionTitle}>Trusted IPs</h2>
                    <p className={styles.muted} style={{ marginBottom: 10 }}>
                      Comma-separated IPs or CIDRs that fail2ban never bans for failed mail logins (the
                      dovecot jail), so an office full of mail clients can&apos;t lock itself out of IMAP
                      and SMTP. This does not limit who can reach the panel.
                    </p>
                    <IpAllowlistForm
                      value={ipAllowlist}
                      action={setSettingAction.bind(null, "panel.ip_allowlist")}
                    />
                  </div>

            <div className={styles.section} id="mcp-token">
                    <h2 className={styles.sectionTitle}>MCP token</h2>
                    <p className={styles.muted} style={{ marginBottom: 10 }}>
                      Bearer token external AI clients (Claude Desktop, Cursor, custom
                      GPTs) send to <span className={styles.code}>/api/mcp</span> as{" "}
                      <span className={styles.code}>Authorization: Bearer &lt;token&gt;</span>.
                      Only a hash of it is stored, so it is shown once, when generated. A read-only token
                      can list and check status but change nothing. Calls are rate-limited and each one
                      is in the Audit log with the caller&apos;s address. The old{" "}
                      <span className={styles.code}>MCP_TOKEN</span> env var still works as a fallback
                      when no token is set here. Browse the tools under{" "}
                      <span className={styles.code}>/ai-agent</span>.
                    </p>
                    <McpTokenForm
                      info={mcpInfo}
                      envFallbackAvailable={mcpEnvFallback}
                      generateAction={generateMcpTokenAction}
                      setScopeAction={setMcpScopeAction}
                      clearAction={clearMcpTokenAction}
                    />
                  </div>
            </>
          ),
        },
        {
          id: "dns",
          label: "DNS & SSL",
          content: (
            <>
            <div className={styles.section}>
                    <h2 className={styles.sectionTitle}>DNS nameserver pair</h2>
                    <p className={styles.muted} style={{ marginBottom: 10 }}>
                      Authoritative nameservers used when the panel creates or reconciles a
                      PowerDNS zone, e.g. <span className={styles.code}>ns1.example.com</span> and{" "}
                      <span className={styles.code}>ns2.example.com</span>. Leave both blank to use
                      per-zone defaults (<span className={styles.code}>ns1.&lt;zone&gt;</span>{" "}
                      / <span className={styles.code}>ns2.&lt;zone&gt;</span>). Changing this does
                      not rewrite existing zones: run &quot;Reconcile zone&quot; on each
                      zone&apos;s detail page to pick up the new pair.
                    </p>
                    <NameserverPairForm
                      value={nsPair}
                      action={setSettingAction.bind(null, "dns.ns_pair")}
                    />
                  </div>

            <div className={styles.section}>
                    <h2 className={styles.sectionTitle}>SSL contact email</h2>
                    <p className={styles.muted} style={{ marginBottom: 10 }}>
                      Address Let&apos;s Encrypt uses for expiry and security notifications. Must be a real
                      email with a dotted domain (e.g.{" "}
                      <span className={styles.code}>you@yourdomain.com</span>) &mdash; LE rejects single-label
                      domains. Leave blank to fall back to <span className={styles.code}>ACME_EMAIL</span> from{" "}
                      <span className={styles.code}>/etc/vps-panel/.env</span>. Takes effect on the next
                      issuance, no agent restart needed.
                    </p>
                    <AcmeEmailForm value={acmeEmail} action={setAcmeEmailAction} />
                  </div>
            </>
          ),
        },
        ]}
      />
    </div>
  );
}
