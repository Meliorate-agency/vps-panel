import { agent } from "@/lib/agent";
import { getServerAddress } from "@/lib/serverAddress";
import HealthChart from "@/components/HealthChart/HealthChart";
import Table from "@/components/Table/Table";
import styles from "@/components/page.module.css";
import grid from "./grid.module.css";
import { fetchLoadOne, fetchMemUsedPct } from "./samples";

export const dynamic = "force-dynamic";

export default async function DashboardPage() {
  const [info, apps, certs, address] = await Promise.all([
    agent.sysinfo().catch(() => null),
    // null = the agent did not answer, which must not read as "0 apps" or
    // "all healthy".
    agent.apps.list().catch(() => null),
    agent.ssl.list().catch(() => null),
    getServerAddress().catch(() => ({ domain: null, ip: null })),
  ]);
  const hasAddress = !!(address.domain || address.ip);

  const runningApps = apps?.filter((a) => a.sub === "running").length ?? 0;
  // Orphan certs (site removed, cert kept) are not "expiring" in any sense
  // the operator can act on - the agent flags them serving=false.
  const servingCerts = certs?.filter((c) => c.serving !== false) ?? [];
  const expiredCerts = servingCerts.filter((c) => c.days_remaining < 0).length;
  const expiringCerts = servingCerts.filter(
    (c) => c.days_remaining >= 0 && c.days_remaining < 30,
  ).length;
  const certSub = !certs
    ? "unavailable"
    : [
        expiredCerts ? `${expiredCerts} expired` : null,
        expiringCerts ? `${expiringCerts} expiring` : null,
      ]
        .filter(Boolean)
        .join(", ") || "all healthy";
  const certTone: "ok" | "warn" | "danger" | undefined = !certs
    ? undefined
    : expiredCerts
      ? "danger"
      : expiringCerts
        ? "warn"
        : "ok";

  const rootDisk =
    info?.disk?.find((d) => d.mount === "/") ?? info?.disk?.[0] ?? null;
  const diskUsedPct = rootDisk
    ? ((rootDisk.total_bytes - rootDisk.free_bytes) / rootDisk.total_bytes) * 100
    : 0;
  const diskTone: "ok" | "warn" | "danger" | undefined = !rootDisk
    ? undefined
    : diskUsedPct >= 90
      ? "danger"
      : diskUsedPct >= 75
        ? "warn"
        : "ok";

  return (
    <div>
      <div className={styles.head}>
        <div>
          <h1 className={styles.title}>Dashboard</h1>
          <div className={styles.subtitle}>
            {hasAddress && (
              <>
                <span className={grid.serverLabel}>Server</span>{" "}
                {address.domain && (
                  <span className={grid.serverDomain}>{address.domain}</span>
                )}
                {address.domain && address.ip && " "}
                {address.ip && (
                  <span className={grid.serverIp}>
                    {address.domain ? `(${address.ip})` : address.ip}
                  </span>
                )}
                {!address.domain && address.ip && (
                  <>
                    {" "}
                    <span className={grid.serverHint}>
                      (point your domain's nameservers here to activate)
                    </span>
                  </>
                )}
              </>
            )}
            {info && (
              <>
                {hasAddress && <span className={grid.serverDot}>·</span>}
                Uptime {formatUptime(info.uptime_secs)}
              </>
            )}
            {!info && <span>Agent unreachable</span>}
          </div>
        </div>
      </div>

      <div className={grid.statRow}>
        <Stat
          label="Apps"
          value={apps ? `${runningApps}/${apps.length}` : "-"}
          sub={apps ? "running" : "unavailable"}
        />
        <Stat
          label="Certs"
          value={certs ? String(certs.length) : "-"}
          sub={certSub}
          tone={certTone}
        />
        <Stat
          label="Load avg"
          value={info ? info.load_1.toFixed(2) : "-"}
          sub={info ? `${info.cpu_count} cpu` : ""}
        />
        <Stat
          label="Mem free"
          value={info ? `${(info.mem_available_kb / 1024 / 1024).toFixed(1)} GB` : "-"}
          sub={info ? `${(info.mem_total_kb / 1024 / 1024).toFixed(1)} GB total` : ""}
        />
        <Stat
          label="Storage"
          value={rootDisk ? `${formatBytes(rootDisk.free_bytes)} free` : "-"}
          sub={
            rootDisk
              ? `${diskUsedPct.toFixed(0)}% used of ${formatBytes(rootDisk.total_bytes)}`
              : ""
          }
          tone={diskTone}
        />
      </div>

      <div className={grid.chartRow}>
        <HealthChart label="Load 1m" fetchValue={fetchLoadOne} />
        {/* Memory is a percentage: the chart's top is 100, not the highest
            sample so far, which drew a flat line along the top edge. */}
        <HealthChart
          label="Memory used"
          fetchValue={fetchMemUsedPct}
          color="var(--ok)"
          scaleMax={100}
          unit="%"
        />
      </div>

      {info && (info.disk.length > 0 || info.net.length > 0) && (
        <div className={grid.infoRow}>
          {info.disk.length > 0 && (
            <div className={styles.section}>
              <h2 className={styles.sectionTitle}>Disks</h2>
              <Table
                layout="fixed"
                columns={[
                  { key: "mount", label: "Mount" },
                  { key: "used", label: "Used" },
                  { key: "size", label: "Size", width: "90px" },
                ]}
                rowKey={(_r, i) => info.disk[i].mount}
                rows={info.disk.map((d) => {
                  const used = d.total_bytes - d.free_bytes;
                  return {
                    mount: <span className={styles.code}>{d.mount}</span>,
                    used: renderUsageBar(used, d.total_bytes),
                    size: formatBytes(d.total_bytes),
                  };
                })}
                empty="No disks reported."
              />
            </div>
          )}

          {info.net.length > 0 && (
            <div className={styles.section}>
              <h2 className={styles.sectionTitle}>Network</h2>
              <Table
                layout="fixed"
                columns={[
                  { key: "iface", label: "Interface", width: "110px" },
                  { key: "rx", label: "RX bytes" },
                  { key: "tx", label: "TX bytes" },
                ]}
                rows={info.net.map((n) => ({
                  iface: <span className={styles.code}>{n.iface}</span>,
                  rx: n.rx_bytes.toLocaleString(),
                  tx: n.tx_bytes.toLocaleString(),
                }))}
                empty="No network counters."
              />
            </div>
          )}
        </div>
      )}

    </div>
  );
}

function Stat({
  label,
  value,
  sub,
  tone,
}: {
  label: string;
  value: string;
  sub?: string;
  tone?: "ok" | "warn" | "danger";
}) {
  return (
    <div className={`${grid.stat} ${tone ? grid[`tone_${tone}`] : ""}`}>
      <div className={grid.statLabel}>{label}</div>
      <div className={grid.statValue}>{value}</div>
      {sub && <div className={grid.statSub}>{sub}</div>}
    </div>
  );
}

function formatBytes(bytes: number): string {
  const gb = bytes / 1024 / 1024 / 1024;
  if (gb >= 1) return `${gb.toFixed(1)} GB`;
  const mb = bytes / 1024 / 1024;
  return `${mb.toFixed(0)} MB`;
}

function renderUsageBar(used: number, total: number): React.ReactNode {
  const pct =
    total > 0 ? Math.min(100, Math.round((used / total) * 100)) : 0;
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
        {formatBytes(used)}{" "}
        <span style={{ color: "var(--text-dim)" }}>({pct}%)</span>
      </span>
      <div
        style={{
          width: "100%",
          height: 6,
          background: "var(--bg-elev-2)",
          borderRadius: 3,
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

function formatUptime(secs: number): string {
  const days = Math.floor(secs / 86400);
  const h = Math.floor((secs % 86400) / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (days > 0) return `${days}d ${h}h`;
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m`;
}
