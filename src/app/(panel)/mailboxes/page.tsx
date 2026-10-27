import Link from "next/link";
import { agent, type MailAliasRow, type MailDomainRow } from "@/lib/agent";
import { db } from "@/lib/db";
import { mailboxDomains } from "@/lib/mailboxDomains";
import ConfirmDeleteButton from "@/components/ConfirmDeleteButton/ConfirmDeleteButton";
import StatusBadge from "@/components/StatusBadge/StatusBadge";
import Table from "@/components/Table/Table";
import TypedConfirmButton from "@/components/TypedConfirmButton/TypedConfirmButton";
import styles from "@/components/page.module.css";
import {
  createAliasAction,
  createMailboxAction,
  deleteAliasAction,
  deleteMailboxAction,
  trackDeliveryAction,
  updateMailboxAction,
} from "@/server-actions/mail";
import ForwarderForm from "./ForwarderForm";
import TrackDelivery from "./TrackDelivery";
import MailboxForm from "./MailboxForm";
import MailServerInfo from "./MailServerInfo";
import CopyMailboxConfig from "./CopyMailboxConfig";
import EditMailboxRow from "./EditMailboxRow";
import MailboxActions, { type WebmailOff } from "./MailboxActions";
import MailboxTable, { type MailboxRow } from "./MailboxTable";
import local from "./mailboxes.module.css";

export const dynamic = "force-dynamic";

type MailboxListShape = { server: string; mailboxes: { id: number; email: string; quota_mb: number }[] };

export default async function MailboxesPage() {
  const knownDomains = mailboxDomains();

  const [mailboxResp, sysinfo, recipes, aliases, mailDomains] = await Promise.all([
    agent.mail
      .listMailboxes()
      .then<MailboxListShape>((r: unknown) => {
        if (Array.isArray(r)) return { server: "", mailboxes: r as MailboxListShape["mailboxes"] };
        if (
          r !== null &&
          typeof r === "object" &&
          Array.isArray((r as { mailboxes?: unknown }).mailboxes)
        ) {
          return r as MailboxListShape;
        }
        return { server: "", mailboxes: [] };
      })
      .catch<MailboxListShape>(() => ({ server: "", mailboxes: [] })),
    agent.sysinfo().catch(() => null),
    agent.quickInstall.list().catch(() => null),
    agent.mail.listAliases().catch((): MailAliasRow[] => []),
    agent.mail.listDomains().catch((): MailDomainRow[] => []),
  ]);

  const { server, mailboxes } = mailboxResp;
  const mailboxEmails = new Set(mailboxes.map((m) => m.email.toLowerCase()));

  // Probe per-mailbox usage in parallel. We keep raw bytes so the UI can
  // render sub-MB precision (otherwise small mailboxes always show "0 MB"
  // because the agent ceiling-rounds usage_mb to whole MB).
  const usageEntries = await Promise.all(
    mailboxes.map(async (m) => {
      const u = await agent.mail
        .mailboxUsage(m.email)
        .catch(() => ({ usage_bytes: 0, usage_mb: 0 }));
      return [m.email, u.usage_bytes] as const;
    }),
  );
  const bytesByEmail = new Map(usageEntries);

  // Roundcube runs on 127.0.0.1:<port> and is reached publicly through whatever
  // site the operator mapped to that port (e.g. webmail.<panel-host>). Without
  // one the per-row icon stays visible but off, and a notice above the table
  // says what is missing.
  const roundcube = recipes?.find((r) => r.slug === "roundcube");
  const roundcubePort = roundcube?.default_port ?? 8082;
  const webmailSite = db()
    .prepare(
      `SELECT domain FROM sites WHERE mode = 'port' AND upstream_port = ? LIMIT 1`,
    )
    .get(roundcubePort) as { domain: string } | undefined;
  const webmailUrl = webmailSite ? `https://${webmailSite.domain}` : null;
  // An unreachable agent leaves `roundcube` unknown: ask for the domain then,
  // the one missing piece the panel can see for itself.
  const webmailOff: WebmailOff = webmailUrl
    ? null
    : roundcube && !roundcube.installed
      ? "install"
      : "domain";
  const webmailOffTitle =
    webmailOff === "install"
      ? "Webmail is off: Roundcube is not installed"
      : "Webmail is off: Roundcube has no public domain yet";

  const rootDisk =
    sysinfo?.disk.find((d) => d.mount === "/var/mail") ??
    sysinfo?.disk.find((d) => d.mount === "/") ??
    sysinfo?.disk[0] ??
    null;

  return (
    <div>
      <div className={styles.head}>
        <div>
          <h1 className={styles.title}>Mailboxes</h1>
          <div className={styles.subtitle}>
            Postfix + Dovecot, virtual users in MariaDB. DKIM via Rspamd.
          </div>
        </div>
      </div>

      <MailServerInfo server={server} />

      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>Add mailbox</h2>
        <MailboxForm
          action={createMailboxAction}
          domains={knownDomains}
          diskFreeBytes={rootDisk?.free_bytes ?? null}
        />
      </div>

      {webmailOff && mailboxes.length > 0 && (
        <div className={local.webmailNotice}>
          <span>
            {webmailOff === "install" ? (
              <>
                Webmail is off. Install Roundcube under Apps, then give it a public
                domain to open these mailboxes in the browser.
              </>
            ) : (
              <>
                Webmail is off. Roundcube has no public domain yet: add a domain that
                proxies to port {roundcubePort} to open these mailboxes in the browser.
              </>
            )}
          </span>
          <Link
            href={webmailOff === "install" ? "/apps" : `/domains?port=${roundcubePort}`}
            className={styles.btnGhost}
          >
            {webmailOff === "install" ? "Go to Apps" : `Add domain for port ${roundcubePort}`}
          </Link>
        </div>
      )}

      <MailboxTable
        rows={mailboxes.map((m): MailboxRow => {
          const usedBytes = bytesByEmail.get(m.email) ?? 0;
          const usedMb = usedBytes / (1024 * 1024);
          // Ceiling-round for the edit form's min={current_usage + 1} so the
          // HTML number input gets a clean integer (e.g. 0.4 MB used → min=1).
          const usedMbCeil = Math.ceil(usedMb);
          return {
            id: m.id,
            domain: m.email.slice(m.email.indexOf("@") + 1).toLowerCase(),
            cells: {
              email: (
                <div>
                  <span className={styles.code}>{m.email}</span>
                  <div className={local.mobileQuota}>
                    {renderQuotaCell(usedMb, m.quota_mb)}
                  </div>
                </div>
              ),
              quota: renderQuotaCell(usedMb, m.quota_mb),
              copy: <CopyMailboxConfig email={m.email} server={server} />,
              actions: (
                <>
                  <div className={local.desktopActions}>
                    {webmailUrl ? (
                      <a
                        href={`${webmailUrl}/?_user=${encodeURIComponent(m.email)}`}
                        target="_blank"
                        rel="noopener noreferrer"
                        className={styles.btnGhost}
                        title={`Open Roundcube for ${m.email}`}
                        aria-label={`Open Roundcube for ${m.email}`}
                        style={{
                          display: "inline-flex",
                          alignItems: "center",
                          justifyContent: "center",
                          padding: "6px 8px",
                        }}
                      >
                        <EmailIcon />
                      </a>
                    ) : (
                      <span
                        className={local.webmailOff}
                        role="img"
                        title={webmailOffTitle}
                        aria-label={webmailOffTitle}
                      >
                        <EmailIcon />
                      </span>
                    )}
                    <EditMailboxRow
                      email={m.email}
                      currentQuotaMb={m.quota_mb}
                      currentUsageMb={usedMbCeil}
                      action={updateMailboxAction.bind(null, m.email)}
                    />
                    <TypedConfirmButton
                      trigger="Delete"
                      triggerClassName={styles.btnDanger}
                      title="Delete mailbox?"
                      resourceName={m.email}
                      description={
                        <>
                          The address stops working immediately. Its stored mail moves
                          to <strong>Files &gt; Trash</strong>, where it can be restored
                          for 30 days before it is deleted for good.
                        </>
                      }
                      action={deleteMailboxAction.bind(null, m.email)}
                    />
                  </div>
                  <MailboxActions
                    email={m.email}
                    server={server}
                    webmailUrl={webmailUrl}
                    webmailOff={webmailOff}
                    currentQuotaMb={m.quota_mb}
                    currentUsageMb={usedMbCeil}
                    updateAction={updateMailboxAction.bind(null, m.email)}
                    deleteAction={deleteMailboxAction.bind(null, m.email)}
                  />
                </>
              ),
            },
          };
        })}
      />

      {webmailUrl && mailboxes.length > 0 && (
        <p className={local.muted} style={{ margin: "10px 0 16px" }}>
          Out of Office replies and mail filters: each person sets their own in{" "}
          <a href={webmailUrl} target="_blank" rel="noopener noreferrer">
            webmail
          </a>
          , under Settings.
        </p>
      )}

      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>Forwarders</h2>
        <ForwarderForm action={createAliasAction} domains={mailDomains.map((d) => d.name)} />
        {aliases.length > 0 && (
          <div style={{ marginTop: 16 }}>
            <Table
              columns={[
                { key: "source", label: "Address" },
                { key: "destination", label: "Forwards to" },
                { key: "actions", label: "", width: "100px" },
              ]}
              rowKey={(_, i) => aliases[i].id}
              rows={aliases.map((a) => ({
                source: a.source.startsWith("@") ? (
                  <span style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
                    <span className={styles.code}>*{a.source}</span>
                    <StatusBadge text="catch-all" tone="info" />
                  </span>
                ) : (
                  <div>
                    <span className={styles.code}>{a.source}</span>
                    {mailboxEmails.has(a.source) && (
                      <div className={local.hint}>mailbox, keeps a copy</div>
                    )}
                  </div>
                ),
                destination: <span className={styles.code}>{a.destination}</span>,
                actions: (
                  <ConfirmDeleteButton
                    action={deleteAliasAction.bind(null, a.id, `${a.source} -> ${a.destination}`)}
                    title="Remove forwarder?"
                    body={
                      <>
                        Mail to <strong>{a.source.startsWith("@") ? `any other address ${a.source}` : a.source}</strong>{" "}
                        stops going to <strong>{a.destination}</strong>.
                      </>
                    }
                    confirmLabel="Remove"
                    buttonLabel="Remove"
                    buttonClassName={styles.btnGhost}
                  />
                ),
              }))}
            />
          </div>
        )}
      </div>

      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>Track delivery</h2>
        <p className={local.muted} style={{ marginTop: 0 }}>
          What the mail server did with mail to or from an address: delivered, sent on,
          retrying, bounced or rejected, with the receiving server&apos;s answer.
        </p>
        <TrackDelivery action={trackDeliveryAction} />
      </div>
    </div>
  );
}

function formatQuota(usedMb: number, totalMb: number): string {
  if (totalMb >= 1024) {
    const usedGb = usedMb / 1024;
    const totalGb = totalMb / 1024;
    return `${usedGb.toFixed(2)} / ${totalGb.toFixed(totalGb % 1 === 0 ? 0 : 1)} GB`;
  }
  return `${usedMb.toFixed(2)} / ${totalMb} MB`;
}

function EmailIcon() {
  return (
    <svg
      width="16"
      height="16"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <rect x="3" y="5" width="18" height="14" rx="2" />
      <polyline points="3 7 12 13 21 7" />
    </svg>
  );
}

function renderQuotaCell(usedMb: number, totalMb: number): React.ReactNode {
  const pct =
    totalMb > 0 ? Math.min(100, Math.round((usedMb / totalMb) * 100)) : 0;
  let barColor = "var(--accent)";
  let textColor: string | undefined;
  if (pct >= 90) {
    barColor = "var(--danger)";
    textColor = "var(--danger)";
  } else if (pct >= 75) {
    barColor = "var(--warn)";
  }
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 4, minWidth: 0 }}>
      <span style={{ fontSize: 12, color: textColor }}>
        {formatQuota(usedMb, totalMb)}{" "}
        <span style={{ color: "var(--text-dim)" }}>({pct}%)</span>
      </span>
      <div
        style={{
          width: "100%",
          height: 4,
          background: "var(--bg-elev-2)",
          borderRadius: 2,
          overflow: "hidden",
        }}
      >
        <div
          style={{
            width: `${pct}%`,
            height: "100%",
            background: barColor,
            transition: "width 0.2s",
          }}
        />
      </div>
    </div>
  );
}
