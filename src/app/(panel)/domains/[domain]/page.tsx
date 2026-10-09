import Link from "next/link";
import { agent, type SiteStatus } from "@/lib/agent";
import { db } from "@/lib/db";
import Table from "@/components/Table/Table";
import StatusBadge from "@/components/StatusBadge/StatusBadge";
import PendingButton from "@/components/PendingButton/PendingButton";
import ConfirmSubmitButton from "@/components/ConfirmSubmitButton/ConfirmSubmitButton";
import styles from "@/components/page.module.css";
import {
  createRecordAction,
  deleteRecordAction,
  updateRecordAction,
  signZoneAction,
  enableZoneAction,
  adoptZoneActionForm,
  deleteZoneAction,
  deleteDomainAction,
  reconcileZoneAction,
} from "@/server-actions/domains";
import {
  deleteSiteAction,
  updateSiteRouteAction,
  renewSslAction,
  retrySslAction,
  siteFormDispatchAction,
} from "@/server-actions/sites";
import RecordForm from "./RecordForm";
import EditRecordRow from "./EditRecordRow";
import EditRoutingForm from "./EditRoutingForm";
import EnableDnsForm from "./EnableDnsForm";
import AddDomainForm from "../AddDomainForm";
import ReconcileButton from "./ReconcileButton";
import DomainDeleteButton from "../DomainDeleteButton";

export const dynamic = "force-dynamic";

type SiteRow = {
  domain: string;
  mode: "port" | "folder";
  upstream_port: number | null;
  folder_path: string | null;
  force_www_redirect: number;
  ssl_requested: number;
  ssl_issued_at: string | null;
  last_ssl_error: string | null;
};

type DomainRow = { name: string; dnssec: number };

export default async function DomainDetailPage({
  params,
}: {
  params: Promise<{ domain: string }>;
}) {
  const { domain } = await params;

  const site = db()
    .prepare(
      `SELECT domain, mode, upstream_port, folder_path, force_www_redirect,
              ssl_requested, ssl_issued_at, last_ssl_error
         FROM sites WHERE domain = ?`,
    )
    .get(domain) as SiteRow | undefined;

  // A port slot bound to an app is edited from the app's page: changing the
  // port or removing routing here would desync the unit's PORT.
  const boundApp =
    (
      db().prepare(`SELECT name FROM apps WHERE subdomain = ?`).get(domain) as
        | { name: string }
        | undefined
    )?.name ?? null;

  const dnsZone = db()
    .prepare(`SELECT name, dnssec FROM domains WHERE name = ?`)
    .get(domain) as DomainRow | undefined;

  // Count of sub-sites under this zone (e.g. app.<zone>, blog.<zone>) so the
  // cascade-delete dialog can warn about them. The apex itself is excluded.
  const subSiteCount = (
    db()
      .prepare(
        `SELECT COUNT(*) AS n FROM sites WHERE domain LIKE ? ESCAPE '\\' AND domain != ?`,
      )
      .get(`%.${domain.replace(/[%_]/g, "\\$&")}`, domain) as { n: number }
  ).n;

  const [siteStatus, records, allZones] = await Promise.all([
    site
      ? agent.sites.status(domain).catch<SiteStatus>(() => ({
          cert_exists: false,
          cert_not_after: null,
          days_remaining: null,
        }))
      : Promise.resolve(null),
    dnsZone ? agent.dns.listRecords(domain).catch(() => []) : Promise.resolve([]),
    agent.dns.listZones().catch(() => []),
  ]);
  const zoneExistsInPdns = dnsZone
    ? true
    : allZones.some((z) => z.name === domain);
  const managedZones = allZones.map((z) => z.name);
  const isSubdomain =
    domain.split(".").filter((l) => l.length > 0).length >= 3;
  const parentManagedZone = (() => {
    if (!isSubdomain) return null;
    const labels = domain.split(".");
    const zoneSet = new Set(managedZones);
    for (let i = 1; i < labels.length; i++) {
      const candidate = labels.slice(i).join(".");
      if (zoneSet.has(candidate)) return candidate;
    }
    return null;
  })();

  // Reconcile SSL status: the agent issues certs in a background task and
  // never writes back to panel.db, so we self-heal here. If a cert is on
  // disk but the row still shows pending, mark it issued for this and
  // future renders.
  if (
    site &&
    site.ssl_requested === 1 &&
    site.ssl_issued_at === null &&
    site.last_ssl_error === null &&
    siteStatus?.cert_exists === true
  ) {
    const now = new Date().toISOString();
    db()
      .prepare(`UPDATE sites SET ssl_issued_at = ? WHERE domain = ?`)
      .run(now, domain);
    site.ssl_issued_at = now;
  }

  return (
    <div>
      <div className={styles.head}>
        <div>
          <h1 className={styles.title}>{domain}</h1>
          <div className={styles.subtitle}>
            <Link href="/domains">All domains</Link>
          </div>
        </div>
      </div>

      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>HTTP routing</h2>
        {site ? (
          <RoutingEditor site={site} status={siteStatus} boundApp={boundApp} />
        ) : (
          <div>
            <div className={styles.muted} style={{ marginBottom: 10 }}>
              No nginx vhost is currently serving this domain.
            </div>
            <AddDomainForm
              action={siteFormDispatchAction}
              enableRecovery
              lockedDomain={domain}
              submitLabel="Add HTTP routing"
              managedZones={managedZones}
            />
          </div>
        )}
      </div>

      <div className={styles.section}>
        <div
          style={{
            display: "flex",
            justifyContent: "space-between",
            alignItems: "center",
            marginBottom: 12,
          }}
        >
          <h2 className={styles.sectionTitle} style={{ margin: 0 }}>
            DNS records
          </h2>
          {dnsZone ? (
            <div style={{ display: "flex", gap: 6, alignItems: "flex-start" }}>
              <ReconcileButton zone={domain} action={reconcileZoneAction} />
              {dnsZone.dnssec === 0 && (
                <form action={signZoneAction.bind(null, domain)}>
                  <button type="submit" className={styles.btnGhost}>
                    DNSSEC sign
                  </button>
                </form>
              )}
              {dnsZone.dnssec === 1 && (
                <StatusBadge text="DNSSEC signed" tone="ok" />
              )}
              <form action={deleteZoneAction.bind(null, domain)}>
                <ConfirmSubmitButton
                  className={styles.btnDanger}
                  message={`Disable DNS for ${domain}?\n\nDrops the PowerDNS zone and ALL its records (MX, SPF, DKIM, A). The nginx vhost is not touched.`}
                  pendingLabel="Disabling..."
                >
                  Disable DNS
                </ConfirmSubmitButton>
              </form>
            </div>
          ) : null}
        </div>
        {dnsZone ? (
          <>
            <div style={{ marginBottom: 16 }}>
              <RecordForm
                action={createRecordAction.bind(null, domain)}
                zone={domain}
              />
            </div>
            {/*
              `layout="auto"` so Name sizes to its content (longest FQDN in
              the zone). Type/Prio/TTL/Actions keep their explicit widths.
              Content has NO width — browser gives it the remaining row
              width. Long TXT values (DKIM, SPF) still need an explicit
              max-width on the inner wrapper to force wrapping; otherwise
              auto layout would let the column grow unbounded and push
              everything else off the right side of the viewport.

              `scrollX` adds mobile-only horizontal scroll inside the card
              so the table doesn't push the whole page wider than the
              viewport on phones. EditRecordRow portals its edit popover
              to document.body to escape the wrap's clipping context.
            */}
            <Table
              layout="auto"
              scrollX
              columns={[
                { key: "name", label: "Name" },
                { key: "type", label: "Type", width: "70px" },
                { key: "content", label: "Content" },
                { key: "prio", label: "Prio", width: "60px" },
                { key: "ttl", label: "TTL", width: "80px" },
                { key: "actions", label: "", width: "140px" },
              ]}
              rowKey={(_row, i) => records[i].id}
              rows={records.map((r) => ({
                name: (
                  <span
                    className={styles.code}
                    style={{ whiteSpace: "nowrap" }}
                  >
                    {r.name}
                  </span>
                ),
                type: <strong>{r.type}</strong>,
                content: (
                  <span
                    className={styles.code}
                    style={{
                      display: "block",
                      // Hard cap so a 1000-char DKIM TXT doesn't widen
                      // its table column. Operators see the full value
                      // wrapped inside the cell, with the raw string also
                      // exposed via the `title` tooltip for quick copy.
                      maxWidth: "min(560px, 50vw)",
                      overflowWrap: "anywhere",
                      wordBreak: "break-all",
                      whiteSpace: "normal",
                    }}
                    title={r.content}
                  >
                    {r.content}
                  </span>
                ),
                prio: <span className={styles.muted}>{r.prio}</span>,
                ttl: r.ttl,
                actions: (
                  <EditRecordRow
                    record={r}
                    zone={domain}
                    updateAction={updateRecordAction.bind(null, domain, r.id)}
                    deleteAction={deleteRecordAction.bind(null, domain, r.id)}
                  />
                ),
              }))}
              empty="No records in this zone."
            />
          </>
        ) : zoneExistsInPdns ? (
          <EnableDnsForm
            domain={domain}
            action={adoptZoneActionForm}
            variant="adopt"
          />
        ) : isSubdomain ? (
          <div className={styles.muted}>
            {parentManagedZone ? (
              <>
                DNS records for this subdomain live in the parent zone{" "}
                <Link href={`/domains/${parentManagedZone}`} className={styles.code}>
                  {parentManagedZone}
                </Link>
                . Add or edit records there.
              </>
            ) : (
              <>
                DNS for this subdomain is not managed by the panel. Records
                must be set at the external DNS provider for the parent zone.
              </>
            )}
          </div>
        ) : (
          <EnableDnsForm domain={domain} action={enableZoneAction} />
        )}
      </div>

      {(site || dnsZone) && (
        <div className={styles.section}>
          <h2 className={styles.sectionTitle}>Danger zone</h2>
          <p className={styles.muted} style={{ marginBottom: 10 }}>
            Wipes the PowerDNS zone (and every record inside it) plus the
            nginx vhost for this domain and any sub-site vhosts under it.
            Mailboxes, DKIM keys, and Let&apos;s Encrypt certs are kept on
            disk so re-adding the domain later picks up where you left off.
          </p>
          <DomainDeleteButton
            zone={domain}
            action={deleteDomainAction}
            subSiteCount={subSiteCount}
            buttonLabel="Delete domain (cascade)"
          />
        </div>
      )}
    </div>
  );
}

function RoutingEditor({
  site,
  status,
  boundApp,
}: {
  site: SiteRow;
  status: SiteStatus | null;
  boundApp: string | null;
}) {
  return (
    <div>
      {boundApp && (
        <div className={styles.notice} style={{ marginBottom: 14 }}>
          Routing slot of app{" "}
          <Link href={`/apps/${encodeURIComponent(boundApp)}`} className={styles.code}>
            {boundApp}
          </Link>
          . Port changes and &quot;Remove HTTP routing&quot; are refused while
          it is bound - move or remove the domain on the app page first.
        </div>
      )}
      <div style={{ display: "flex", gap: 12, marginBottom: 14, flexWrap: "wrap" }}>
        <StatusBadge text={site.mode} tone="ok" />
        {site.mode === "port" && site.upstream_port !== null && (
          <span className={styles.code}>127.0.0.1:{site.upstream_port}</span>
        )}
        {site.mode === "folder" && site.folder_path !== null && (
          <span className={styles.code}>{site.folder_path}</span>
        )}
        {renderSslStatus(site, status)}
        {site.force_www_redirect === 1 && (
          <StatusBadge text="www → non-www" tone="ok" />
        )}
      </div>
      <EditRoutingForm
        domain={site.domain}
        initialMode={site.mode}
        initialUpstreamPort={site.upstream_port}
        initialFolderPath={site.folder_path}
        initialIssueSsl={site.ssl_requested === 1}
        initialForceWwwRedirect={site.force_www_redirect === 1}
        action={updateSiteRouteAction.bind(null, site.domain)}
      />
      <div style={{ marginTop: 12 }}>
        <form action={deleteSiteAction.bind(null, site.domain)}>
          <button type="submit" className={styles.btnDanger}>
            Remove HTTP routing
          </button>
        </form>
      </div>
    </div>
  );
}

function formatCertDate(notAfter: string | null): string | null {
  if (!notAfter) return null;
  const d = new Date(notAfter);
  if (Number.isNaN(d.getTime())) return null;
  return d.toLocaleDateString("en-US", {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
}

function renderSslStatus(site: SiteRow, status: SiteStatus | null) {
  const s = status ?? {
    cert_exists: false,
    cert_not_after: null,
    days_remaining: null,
  };
  if (s.cert_exists && (s.days_remaining ?? -1) >= 0) {
    const dateLabel = formatCertDate(s.cert_not_after);
    return (
      <div style={{ display: "inline-flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
        <StatusBadge text={`SSL · ${s.days_remaining}d`} tone="ok" />
        {dateLabel && <span className={styles.muted}>expires {dateLabel}</span>}
        <form action={renewSslAction.bind(null, site.domain)}>
          <PendingButton
            className={styles.btnGhost}
            pendingLabel="Renewing..."
          >
            Renew
          </PendingButton>
        </form>
      </div>
    );
  }
  if (s.cert_exists && (s.days_remaining ?? 0) < 0) {
    return (
      <div style={{ display: "inline-flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
        <StatusBadge text="SSL expired" tone="danger" />
        <form action={renewSslAction.bind(null, site.domain)}>
          <PendingButton className={styles.btnGhost} pendingLabel="Renewing...">
            Renew
          </PendingButton>
        </form>
      </div>
    );
  }
  if (site.ssl_requested === 1 && site.last_ssl_error === null) {
    return <StatusBadge text="SSL issuing..." tone="warn" />;
  }
  if (site.ssl_requested === 1 && site.last_ssl_error !== null) {
    return (
      <form
        action={retrySslAction.bind(null, site.domain)}
        style={{ display: "inline-flex", gap: 6, alignItems: "center" }}
      >
        <StatusBadge text="SSL failed" tone="danger" />
        <PendingButton
          className={styles.btnGhost}
          pendingLabel="Retrying..."
        >
          Retry
        </PendingButton>
        <span className={styles.muted} style={{ fontSize: 12, flexBasis: "100%" }}>
          {site.last_ssl_error}
        </span>
      </form>
    );
  }
  return <StatusBadge text="SSL off" tone="muted" />;
}

