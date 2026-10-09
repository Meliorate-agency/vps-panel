import Link from "next/link";
import { agent, type SiteStatus, type RecordRow } from "@/lib/agent";
import { db } from "@/lib/db";
import StatusBadge from "@/components/StatusBadge/StatusBadge";
import InfoTooltip from "@/components/InfoTooltip/InfoTooltip";
import PendingButton from "@/components/PendingButton/PendingButton";
import styles from "@/components/page.module.css";
import DomainsGroupedTable, {
  type GroupedRow,
} from "./DomainsGroupedTable";
import {
  deleteSiteAction,
  installSslAction,
  removeAgentOnlySiteAction,
  renewSslTaskAction,
  retrySslAction,
  siteFormDispatchAction,
} from "@/server-actions/sites";
import { adoptZoneAction, deleteDomainAction } from "@/server-actions/domains";
import { ensureMailSslAction } from "@/server-actions/mail";
import AddDomainForm from "./AddDomainForm";
import DomainDeleteButton from "./DomainDeleteButton";
import SslIssuingBadge from "./SslIssuingBadge";
import SslTaskButton from "./SslTaskButton";

export const dynamic = "force-dynamic";

type DomainRow = {
  domain: string;
  mode: "port" | "folder" | null;
  upstream_port: number | null;
  folder_path: string | null;
  ssl_requested: number | null;
  ssl_issued_at: string | null;
  last_ssl_error: string | null;
  has_dns: number;
};

const FQDN_RE_PREFILL = /^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)+$/;

export default async function DomainsPage({
  searchParams,
}: {
  searchParams: Promise<{ prefill?: string; port?: string; error?: string }>;
}) {
  const sp = await searchParams;
  const prefillRaw = (sp.prefill ?? "").trim().toLowerCase();
  const prefillDomain = FQDN_RE_PREFILL.test(prefillRaw) ? prefillRaw : undefined;
  // ?port=<n> lets cards on /databases and /apps deep-link operators here
  // with mode=port pre-selected, so a one-click "Route subdomain" flow on
  // the QuickInstallStrip lands them on a half-completed form (just type
  // the subdomain and submit). Strictly validated as 1..65535 — anything
  // else is dropped so a malformed URL doesn't break the form.
  const prefillPortRaw = sp.port ?? "";
  const prefillPortNum = Number(prefillPortRaw);
  const prefillUpstreamPort =
    Number.isInteger(prefillPortNum) &&
    prefillPortNum >= 1 &&
    prefillPortNum <= 65535
      ? prefillPortNum
      : undefined;
  const rows = db()
    .prepare(
      `SELECT s.domain          AS domain,
              s.mode             AS mode,
              s.upstream_port    AS upstream_port,
              s.folder_path      AS folder_path,
              s.ssl_requested    AS ssl_requested,
              s.ssl_issued_at    AS ssl_issued_at,
              s.last_ssl_error   AS last_ssl_error,
              CASE WHEN d.name IS NULL THEN 0 ELSE 1 END AS has_dns
         FROM sites s
         LEFT JOIN domains d ON d.name = s.domain
        UNION
       SELECT d.name AS domain, NULL, NULL, NULL, NULL, NULL, NULL, 1 AS has_dns
         FROM domains d
        WHERE d.name NOT IN (SELECT domain FROM sites)
        ORDER BY domain ASC`,
    )
    .all() as DomainRow[];

  const httpDomains = rows.filter((r) => r.mode !== null);
  // Port slots that currently serve a panel app, so the Target cell can say
  // which one (and why a delete / port change on that row is refused).
  const appBySite = new Map(
    (
      db()
        .prepare(`SELECT name, subdomain FROM apps WHERE subdomain != ''`)
        .all() as { name: string; subdomain: string }[]
    ).map((a) => [a.subdomain, a.name]),
  );
  const agentList = await agent.sites.list().catch(() => []);
  // Mail tenants: their `mail.<d>` ACME-helper vhosts surface as agent-only
  // rows, and those get an "Issue SSL" control when nothing covers the name.
  const mailTenants = new Set(
    (await agent.mail.listDomains().catch(() => [])).map((d) => d.name),
  );
  const httpDomainSet = new Set(httpDomains.map((r) => r.domain));
  const agentOnly = agentList.filter((a) => !httpDomainSet.has(a.domain));
  const agentOnlyDomains = agentOnly.map((a) => a.domain);
  const kindByDomain = new Map(agentOnly.map((a) => [a.domain, a.kind] as const));

  const knownDomains = new Set(rows.map((r) => r.domain));
  const pdnsZones = await agent.dns.listZones().catch(() => []);
  const untrackedZones = pdnsZones
    .filter((z) => !knownDomains.has(z.name) && !httpDomainSet.has(z.name))
    .map((z) => z.name)
    .sort();

  // Surface subdomain A/AAAA/CNAME records that live inside a managed
  // PowerDNS zone but don't have their own site/vhost/zone entry, so adding
  // `mail` or `members` under `example.com` from the zone detail page makes
  // `mail.example.com` / `members.example.com` appear here too instead of
  // being invisible at the apex level. CNAMEs need this just as much as
  // A/AAAA — a subdomain pointing at Vercel/Netlify is still "a domain you
  // own" that deserves a row.
  const recordsByZone = await Promise.all(
    pdnsZones.map(async (z) => ({
      zone: z.name,
      records: await agent.dns.listRecords(z.name).catch<RecordRow[]>(() => []),
    })),
  );
  const subdomainMap = new Map<
    string,
    { ipv4: string[]; ipv6: string[]; cname: string[]; parentZone: string }
  >();
  for (const { zone, records } of recordsByZone) {
    for (const r of records) {
      if (r.disabled) continue;
      if (r.type !== "A" && r.type !== "AAAA" && r.type !== "CNAME") continue;
      const name = r.name.toLowerCase().replace(/\.$/, "");
      if (name === zone) continue;
      if (!name.endsWith(`.${zone}`)) continue;
      const entry = subdomainMap.get(name) ?? {
        ipv4: [],
        ipv6: [],
        cname: [],
        parentZone: zone,
      };
      if (r.type === "A") entry.ipv4.push(r.content);
      else if (r.type === "AAAA") entry.ipv6.push(r.content);
      else entry.cname.push(r.content.replace(/\.$/, ""));
      subdomainMap.set(name, entry);
    }
  }
  const alreadyListed = new Set<string>([
    ...rows.map((r) => r.domain),
    ...agentOnlyDomains,
    ...untrackedZones,
  ]);
  const dnsSubdomains = Array.from(subdomainMap.entries())
    .filter(([name]) => !alreadyListed.has(name))
    .map(([domain, info]) => ({ domain, ...info }))
    .sort((a, b) => a.domain.localeCompare(b.domain));

  const statusDomains = [...httpDomains.map((r) => r.domain), ...agentOnlyDomains];
  const statuses = await Promise.all(
    statusDomains.map(async (domain) => {
      const s = await agent.sites
        .status(domain)
        .catch<SiteStatus>(() => ({
          cert_exists: false,
          cert_not_after: null,
          days_remaining: null,
        }));
      return { domain, status: s };
    }),
  );
  const statusByDomain = new Map(statuses.map((s) => [s.domain, s.status]));

  // Per-zone count of tracked sub-sites (anything in `sites` that's a
  // subdomain of a managed zone). Powers the "deleting will also wipe N
  // sub-sites" line in the cascade-delete dialog.
  const managedZoneNames = new Set(
    rows.filter((r) => r.has_dns === 1).map((r) => r.domain),
  );
  const subSiteCountByZone = new Map<string, number>();
  for (const r of rows) {
    if (r.mode === null) continue; // DNS-only row, not a "site"
    const apex = r.domain.split(".").slice(-2).join(".");
    for (const z of managedZoneNames) {
      if (r.domain !== z && r.domain.endsWith(`.${z}`)) {
        subSiteCountByZone.set(z, (subSiteCountByZone.get(z) ?? 0) + 1);
      }
    }
    void apex; // suppress unused
  }

  return (
    <div>
      <div className={styles.head}>
        <div>
          <h1 className={styles.title}>Domains</h1>
          <div className={styles.subtitle}>
            HTTP routing and DNS for your domains.
          </div>
        </div>
      </div>

      {/* Delete actions redirect here with the refusal reason (a bound app
          keeps its slot until the app page releases it). */}
      {sp.error && (
        <div className={styles.error} style={{ marginBottom: 16 }}>
          Refused: {sp.error.slice(0, 300)}
        </div>
      )}

      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>Add domain</h2>
        <AddDomainForm
          action={siteFormDispatchAction}
          enableRecovery
          managedZones={pdnsZones.map((z) => z.name)}
          prefillDomain={prefillDomain}
          prefillUpstreamPort={prefillUpstreamPort}
        />
      </div>

      <DomainsGroupedTable
        scrollX
        columns={[
          // Pin every column so the table has a real intrinsic width
          // (~1170px) and triggers inner horizontal scroll on mobile
          // instead of squeezing the SSL cell into multi-line wrap.
          { key: "domain", label: "Domain", width: "260px" },
          { key: "mode", label: "Mode", width: "110px" },
          { key: "target", label: "Target", width: "240px" },
          { key: "ssl", label: "SSL", width: "320px" },
          { key: "dns", label: "DNS", width: "110px" },
          { key: "actions", label: "", width: "180px" },
        ]}
        rows={buildGroupedRows({
          rows,
          agentOnly: agentOnlyDomains,
          kindByDomain,
          untrackedZones,
          dnsSubdomains,
          statusByDomain,
          subSiteCountByZone,
          appBySite,
          mailTenants,
        })}
        empty="No domains yet. Add one above."
      />
    </div>
  );
}

function getApex(domain: string): string {
  // Naive registered-domain heuristic: last 2 labels. Sufficient for .com,
  // .management, .education etc. Does not handle public-suffix cases like
  // .co.uk - acceptable trade-off; revisit if those become real.
  const labels = domain.split(".");
  if (labels.length <= 2) return domain;
  return labels.slice(-2).join(".");
}

type DnsSubdomainItem = {
  domain: string;
  ipv4: string[];
  ipv6: string[];
  cname: string[];
  parentZone: string;
};

type BuildArgs = {
  rows: DomainRow[];
  agentOnly: string[];
  kindByDomain: Map<string, "site" | "app" | "panel-host" | "external">;
  untrackedZones: string[];
  dnsSubdomains: DnsSubdomainItem[];
  statusByDomain: Map<string, SiteStatus>;
  /** Count of tracked sites whose name is `*.<zone>` (excluding the apex itself), per zone. */
  subSiteCountByZone: Map<string, number>;
  /** Panel app bound to each port-mode site (site domain -> app name). */
  appBySite: Map<string, string>;
  /** Mail tenants (apex names). Their `mail.<d>` helper vhosts get an
   *  "Issue SSL" control when no cert covers the name. */
  mailTenants: Set<string>;
};

function buildGroupedRows({
  rows,
  agentOnly,
  kindByDomain,
  untrackedZones,
  dnsSubdomains,
  statusByDomain,
  subSiteCountByZone,
  mailTenants,
  appBySite,
}: BuildArgs): GroupedRow[] {
  type Item =
    | { kind: "tracked"; domain: string; data: DomainRow }
    | { kind: "agentOnly"; domain: string }
    | { kind: "untracked"; domain: string }
    | { kind: "dnsSubdomain"; domain: string; data: DnsSubdomainItem }
    | { kind: "syntheticApex"; domain: string };

  const items: Item[] = [
    ...rows.map<Item>((r) => ({ kind: "tracked", domain: r.domain, data: r })),
    ...agentOnly.map<Item>((d) => ({ kind: "agentOnly", domain: d })),
    ...untrackedZones.map<Item>((d) => ({ kind: "untracked", domain: d })),
    ...dnsSubdomains.map<Item>((d) => ({
      kind: "dnsSubdomain",
      domain: d.domain,
      data: d,
    })),
  ];

  const itemDomains = new Set(items.map((i) => i.domain));
  const apexesWithKids = new Set<string>();
  for (const i of items) {
    const a = getApex(i.domain);
    if (i.domain !== a) apexesWithKids.add(a);
  }
  for (const apex of apexesWithKids) {
    if (!itemDomains.has(apex)) {
      items.push({ kind: "syntheticApex", domain: apex });
    }
  }

  items.sort((a, b) => {
    const aApex = getApex(a.domain);
    const bApex = getApex(b.domain);
    if (aApex !== bApex) return aApex.localeCompare(bApex);
    const aIsApex = a.domain === aApex;
    const bIsApex = b.domain === bApex;
    if (aIsApex !== bIsApex) return aIsApex ? -1 : 1;
    return a.domain.localeCompare(b.domain);
  });

  const childCountByApex = new Map<string, number>();
  for (const i of items) {
    const a = getApex(i.domain);
    if (i.domain !== a) {
      childCountByApex.set(a, (childCountByApex.get(a) ?? 0) + 1);
    }
  }

  return items.map<GroupedRow>((item) => {
    const apex = getApex(item.domain);
    const isApex = item.domain === apex;
    const hasChildren = isApex && (childCountByApex.get(apex) ?? 0) > 0;

    let cells: Record<string, React.ReactNode>;
    switch (item.kind) {
      case "tracked": {
        const r = item.data;
        cells = {
          domain: (
            <Link
              href={`/domains/${encodeURIComponent(r.domain)}`}
              className={styles.code}
            >
              <strong>{r.domain}</strong>
            </Link>
          ),
          mode: renderModeBadge(r),
          target: renderTarget(r, appBySite.get(r.domain)),
          ssl: renderSslCell(r, statusByDomain.get(r.domain)),
          dns:
            r.has_dns === 1 ? (
              <span style={{ display: "inline-flex", alignItems: "center" }}>
                <StatusBadge text="managed" tone="ok" />
                <InfoTooltip
                  label="What does Managed mean?"
                  text="The PowerDNS zone for this domain lives on this VPS and is tracked by the panel. The DNS records you see on the domain detail page are what's actually served — provided the registrar's nameservers point here."
                />
              </span>
            ) : (
              <span style={{ display: "inline-flex", alignItems: "center" }}>
                <StatusBadge text="external" tone="muted" />
                <InfoTooltip
                  label="What does External DNS mean?"
                  text="The panel doesn't manage DNS for this domain. Whatever authoritative nameservers the registrar points at (GoDaddy, Cloudflare, etc.) is what's serving the records. Add the zone here to bring it under panel control."
                />
              </span>
            ),
          actions: (
            <div style={{ display: "flex", gap: 6 }}>
              <Link
                href={`/domains/${encodeURIComponent(r.domain)}`}
                className={styles.btnGhost}
              >
                Edit
              </Link>
              {r.has_dns === 1 ? (
                // Managed zone — cascade-delete wipes the PowerDNS zone (and
                // every record under it), the apex nginx vhost, and every
                // tracked sub-site under the zone. Mailboxes / DKIM / certs
                // are preserved on disk.
                <DomainDeleteButton
                  zone={r.domain}
                  action={deleteDomainAction}
                  subSiteCount={subSiteCountByZone.get(r.domain) ?? 0}
                />
              ) : (
                r.mode !== null && (
                  <form action={deleteSiteAction.bind(null, r.domain)}>
                    <PendingButton
                      className={styles.btnDanger}
                      pendingLabel="Deleting..."
                    >
                      Delete
                    </PendingButton>
                  </form>
                )
              )}
            </div>
          ),
        };
        break;
      }
      case "agentOnly": {
        const domain = item.domain;
        const vhostKind = kindByDomain.get(domain) ?? "external";
        const modeBadge =
          vhostKind === "panel-host" ? (
            <span style={{ display: "inline-flex", alignItems: "center" }}>
              <StatusBadge text="panel host" tone="ok" />
              <InfoTooltip
                label="What is a panel-host vhost?"
                text="This nginx vhost serves the panel UI itself (the page you're looking at). It's protected — removing it would lock you out of the panel."
              />
            </span>
          ) : vhostKind === "app" ? (
            <span style={{ display: "inline-flex", alignItems: "center" }}>
              <StatusBadge text="app vhost" tone="ok" />
              <InfoTooltip
                label="What is an app vhost?"
                text="This nginx vhost was generated by the panel for an app deployed via the Apps page. Manage it from /apps, not here."
              />
            </span>
          ) : (
            <span style={{ display: "inline-flex", alignItems: "center" }}>
              <StatusBadge text="agent only" tone="warn" />
              <InfoTooltip
                label="What does Agent only mean?"
                text="The agent sees an nginx vhost on disk for this domain, but the panel has no record of it in its database. Probably created by hand or by an earlier install. Click Remove from agent to delete the vhost; the SSL cert is kept."
              />
            </span>
          );
        // Destructive "Remove from agent" only makes sense for plain external
        // vhosts. Removing the panel-host vhost would lock the user out;
        // app vhosts are managed by the Apps page (not here). A live mail
        // tenant's mail.<d> helper vhost answers the HTTP-01 challenge that
        // renews its mail cert (and any site cert carrying mail.<d>), so it is
        // not removable either.
        const isMailHelper =
          domain.startsWith("mail.") && mailTenants.has(domain.slice("mail.".length));
        const canRemove = (vhostKind === "external" || vhostKind === "site") && !isMailHelper;
        cells = {
          domain: (
            <Link
              href={`/domains/${encodeURIComponent(domain)}`}
              className={styles.code}
            >
              <strong>{domain}</strong>
            </Link>
          ),
          mode: modeBadge,
          target: <span className={styles.muted}>—</span>,
          ssl: renderAgentOnlySslCell(
            domain,
            vhostKind,
            statusByDomain.get(domain),
            domain.startsWith("mail.") && mailTenants.has(domain.slice("mail.".length)),
          ),
          dns: (
            <span style={{ display: "inline-flex", alignItems: "center" }}>
              <StatusBadge text="external" tone="muted" />
              <InfoTooltip
                label="What does External DNS mean?"
                text="The panel doesn't manage DNS for this domain. The registrar's nameservers (GoDaddy, Cloudflare, etc.) serve the records — not this VPS."
              />
            </span>
          ),
          actions: canRemove ? (
            <form action={removeAgentOnlySiteAction.bind(null, domain)}>
              <PendingButton
                className={styles.btnDanger}
                pendingLabel="Removing..."
              >
                Remove from agent
              </PendingButton>
            </form>
          ) : vhostKind === "app" ? (
            <Link
              href={`/apps`}
              className={styles.btnGhost}
              title="Managed on the Apps page"
            >
              Open Apps
            </Link>
          ) : (
            <span className={styles.muted} title="Panel UI vhost; not deletable from here">
              protected
            </span>
          ),
        };
        break;
      }
      case "untracked": {
        const domain = item.domain;
        cells = {
          domain: (
            <Link
              href={`/domains/${encodeURIComponent(domain)}`}
              className={styles.code}
            >
              <strong>{domain}</strong>
            </Link>
          ),
          mode: (
            <span style={{ display: "inline-flex", alignItems: "center" }}>
              <StatusBadge text="dns only" tone="muted" />
              <InfoTooltip
                label="What does DNS only mean?"
                text="This domain has a PowerDNS zone on the VPS but no nginx vhost. Nothing on this server is serving HTTP for it — only DNS queries. Useful for domains where the website lives elsewhere but you still want the panel to manage DNS."
              />
            </span>
          ),
          target: <span className={styles.muted}>—</span>,
          ssl: <span className={styles.muted}>—</span>,
          dns: (
            <span style={{ display: "inline-flex", alignItems: "center" }}>
              <StatusBadge text="untracked" tone="warn" />
              <InfoTooltip
                label="What does Untracked DNS mean?"
                text="A PowerDNS zone exists for this domain on the VPS, but the panel's database has no record of it. Click Adopt to start tracking it via the panel. Until you do, you can't edit records here."
              />
            </span>
          ),
          actions: (
            <form action={adoptZoneAction.bind(null, domain)}>
              <PendingButton
                className={styles.btnGhost}
                pendingLabel="Adopting..."
              >
                Adopt
              </PendingButton>
            </form>
          ),
        };
        break;
      }
      case "dnsSubdomain": {
        const d = item.data;
        const ips = [...d.ipv4, ...d.ipv6];
        const isCnameOnly = ips.length === 0 && d.cname.length > 0;
        const targetText = isCnameOnly
          ? d.cname.map((c) => `→ ${c}`).join(", ")
          : ips.join(", ") || "—";
        cells = {
          domain: (
            <Link
              href={`/domains/${encodeURIComponent(d.parentZone)}`}
              className={styles.code}
              title={`Edit records in parent zone ${d.parentZone}`}
            >
              <strong>{d.domain}</strong>
            </Link>
          ),
          mode: (
            <span style={{ display: "inline-flex", alignItems: "center" }}>
              <StatusBadge
                text={isCnameOnly ? "cname record" : "dns record"}
                tone="muted"
              />
              <InfoTooltip
                label={
                  isCnameOnly
                    ? "What does 'CNAME record' mean?"
                    : "What does 'DNS record' mean?"
                }
                text={
                  isCnameOnly
                    ? `This subdomain has a CNAME record in the parent zone ${d.parentZone} pointing at an external host (e.g. Vercel, Netlify). The panel doesn't issue SSL for it — whoever owns the target host does. Click to edit the record from the parent zone's detail page.`
                    : `This subdomain has an A or AAAA record in the parent zone ${d.parentZone}, but no nginx site of its own. Click to edit the record from the parent zone's detail page.`
                }
              />
            </span>
          ),
          target: (
            <span className={styles.code}>{targetText}</span>
          ),
          ssl: isCnameOnly ? (
            <span className={styles.muted} title="CNAME target handles its own TLS">
              external
            </span>
          ) : (
            <form action={installSslAction.bind(null, d.domain)}>
              <PendingButton
                className={styles.btn}
                pendingLabel="Installing..."
              >
                Install SSL
              </PendingButton>
            </form>
          ),
          dns: <StatusBadge text="in parent" tone="ok" />,
          actions: (
            <Link
              href={`/domains/${encodeURIComponent(d.parentZone)}`}
              className={styles.btnGhost}
            >
              Edit in parent
            </Link>
          ),
        };
        break;
      }
      case "syntheticApex": {
        const count = childCountByApex.get(item.domain) ?? 0;
        cells = {
          domain: (
            <span>
              <strong>{item.domain}</strong>{" "}
              <span className={styles.muted}>
                ({count} subdomain{count === 1 ? "" : "s"})
              </span>
            </span>
          ),
          mode: <StatusBadge text="parent only" tone="muted" />,
          target: <span className={styles.muted}>—</span>,
          ssl: <span className={styles.muted}>—</span>,
          dns: <span className={styles.muted}>—</span>,
          actions: null,
        };
        break;
      }
    }

    return { key: item.domain, apex, isApex, hasChildren, cells };
  });
}

function renderModeBadge(r: DomainRow) {
  if (r.mode === "folder") return <StatusBadge text="folder" tone="ok" />;
  if (r.mode === "port") return <StatusBadge text="port" tone="ok" />;
  return <StatusBadge text="dns only" tone="muted" />;
}

function renderTarget(r: DomainRow, boundApp?: string) {
  if (r.mode === "folder" && r.folder_path) {
    return <span className={styles.code}>{r.folder_path}</span>;
  }
  if (r.mode === "port" && r.upstream_port) {
    return (
      <span style={{ display: "inline-flex", alignItems: "center", gap: 6, flexWrap: "wrap" }}>
        <span className={styles.code}>127.0.0.1:{r.upstream_port}</span>
        {boundApp && (
          <Link
            href={`/apps/${encodeURIComponent(boundApp)}`}
            className={styles.muted}
            style={{ fontSize: 12 }}
            title="Routing slot bound to this app - manage the binding on the app page"
          >
            app: {boundApp}
          </Link>
        )}
      </span>
    );
  }
  return <span className={styles.muted}>—</span>;
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

/** Badge + date + the right control for a live cert: Renew when the name
 *  owns the cert, "via <parent>" when it is a SAN on the parent's cert
 *  (mail.<d>, www.<d>) - the parent's Renew covers it. */
function renderValidCert(domain: string, s: SiteStatus, badge: string) {
  const dateLabel = formatCertDate(s.cert_not_after);
  return (
    <div style={{ display: "flex", gap: 6, alignItems: "center", flexWrap: "nowrap", whiteSpace: "nowrap" }}>
      <StatusBadge text={badge} tone="ok" />
      {dateLabel && <span className={styles.muted}>{dateLabel}</span>}
      {s.covered_by ? (
        <span
          className={styles.muted}
          title={`${domain} has no certificate of its own: it is listed on the ${s.covered_by} certificate. Renewing ${s.covered_by} renews this too.`}
        >
          via {s.covered_by}
        </span>
      ) : (
        <SslTaskButton
          label="Renew"
          pendingLabel="renewing..."
          action={renewSslTaskAction.bind(null, domain)}
        />
      )}
    </div>
  );
}

function renderAgentOnlySslCell(
  domain: string,
  vhostKind: "site" | "app" | "panel-host" | "external",
  status: SiteStatus | undefined,
  mailHelper: boolean,
) {
  const s = status ?? {
    cert_exists: false,
    cert_not_after: null,
    days_remaining: null,
  };
  // Panel-host without a cert: the operator cannot use the generic Install SSL
  // flow (it would create a colliding `site-<host>` vhost and could lock them
  // out). Send them to Settings -> Panel hostname which goes through the
  // dedicated /system/panel-host endpoint — issues the cert + re-renders the
  // panel vhost atomically and flips COOKIE_SECURE.
  if (vhostKind === "panel-host" && !s.cert_exists) {
    return (
      <Link
        href="/settings#panel-host"
        className={styles.btn}
        title="Issue a Let's Encrypt cert for the panel host (via Settings -> Panel hostname)"
      >
        Issue cert
      </Link>
    );
  }
  // mail.<d> of a mail tenant with nothing covering it: this is the hostname
  // mail clients connect to, so a dash here means "IMAP/SMTP show the wrong
  // cert". One click asks the agent to re-issue <d>'s cert with the mail SAN
  // (or a standalone mail.<d> cert) and rewire Postfix/Dovecot.
  if (mailHelper && !(s.cert_exists && (s.days_remaining ?? -1) >= 0)) {
    return (
      <SslTaskButton
        label="Issue SSL"
        pendingLabel="issuing..."
        className={styles.btn}
        action={ensureMailSslAction.bind(null, domain.replace(/^mail\./, ""))}
      />
    );
  }
  return renderCertOnlyCell(domain, status);
}

function renderCertOnlyCell(domain: string, status: SiteStatus | undefined) {
  const s = status ?? {
    cert_exists: false,
    cert_not_after: null,
    days_remaining: null,
  };
  if (s.cert_exists && (s.days_remaining ?? -1) >= 0) {
    return renderValidCert(domain, s, `Valid · ${s.days_remaining}d`);
  }
  // Expired: the cert is still on disk and the fix is a renewal, so Renew
  // stays reachable. This used to fall through to "issuing..." / "off" and
  // hide the button exactly when it was needed.
  if (s.cert_exists && (s.days_remaining ?? 0) < 0) {
    return (
      <div style={{ display: "flex", gap: 6, alignItems: "center", whiteSpace: "nowrap" }}>
        <StatusBadge text="expired" tone="danger" />
        <SslTaskButton
          label="Renew"
          pendingLabel="renewing..."
          action={renewSslTaskAction.bind(null, domain)}
        />
      </div>
    );
  }
  return <span className={styles.muted}>—</span>;
}

function renderSslCell(r: DomainRow, status: SiteStatus | undefined) {
  if (r.mode === null) {
    // DNS-only zone (apex or otherwise): no nginx vhost yet. One-click action
    // creates a default static-folder site at /var/www/<domain> with
    // issue_ssl=true and lets pollSslTaskAndReconcile flip the badge once the
    // cert lands.
    return (
      <form action={installSslAction.bind(null, r.domain)}>
        <PendingButton className={styles.btn} pendingLabel="Installing...">
          Install SSL
        </PendingButton>
      </form>
    );
  }
  const s = status ?? {
    cert_exists: false,
    cert_not_after: null,
    days_remaining: null,
  };
  if (s.cert_exists && (s.days_remaining ?? -1) >= 0) {
    return renderValidCert(r.domain, s, `SSL · ${s.days_remaining}d`);
  }
  // Expired: the cert is still on disk and the fix is a renewal, so Renew
  // stays reachable. This used to fall through to "issuing..." / "off" and
  // hide the button exactly when it was needed.
  if (s.cert_exists && (s.days_remaining ?? 0) < 0) {
    return (
      <div style={{ display: "flex", gap: 6, alignItems: "center", whiteSpace: "nowrap" }}>
        <StatusBadge text="expired" tone="danger" />
        <SslTaskButton
          label="Renew"
          pendingLabel="renewing..."
          action={renewSslTaskAction.bind(null, r.domain)}
        />
      </div>
    );
  }
  if (r.ssl_requested === 1 && r.last_ssl_error === null) {
    // Client badge: re-renders the page every few seconds until the
    // reconciler has flipped this row, so the operator never has to reload.
    return <SslIssuingBadge />;
  }
  if (r.ssl_requested === 1 && r.last_ssl_error !== null) {
    return (
      <form
        action={retrySslAction.bind(null, r.domain)}
        style={{ display: "flex", gap: 6, alignItems: "center" }}
      >
        <span title={r.last_ssl_error ?? undefined}>
          <StatusBadge text="failed" tone="danger" />
        </span>
        <PendingButton
          className={styles.btnGhost}
          pendingLabel="Retrying..."
        >
          Retry
        </PendingButton>
        <span
          className={styles.muted}
          title={r.last_ssl_error ?? undefined}
          style={{ maxWidth: 260, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", fontSize: 12 }}
        >
          {r.last_ssl_error}
        </span>
      </form>
    );
  }
  return <span className={styles.muted}>off</span>;
}
