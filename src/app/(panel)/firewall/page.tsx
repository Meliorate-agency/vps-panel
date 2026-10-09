import { agent } from "@/lib/agent";
import Table from "@/components/Table/Table";
import StatusBadge from "@/components/StatusBadge/StatusBadge";
import PendingButton from "@/components/PendingButton/PendingButton";
import styles from "@/components/page.module.css";
import {
  enableUfwAction,
  fetchFailedByIpAction,
  firewallPortAction,
  restartFail2banAction,
  toggleFail2banAction,
  unbanAllAction,
  unbanIpAction,
} from "@/server-actions/firewall";
import {
  getFail2banPolicy,
  setFail2banPolicyAction,
} from "@/server-actions/setup";
import ConfirmSubmitButton from "@/components/ConfirmSubmitButton/ConfirmSubmitButton";
import PortForm from "./PortForm";
import JailsTable from "./JailsTable";
import Fail2banPolicyForm from "./Fail2banPolicyForm";

export const dynamic = "force-dynamic";

export default async function FirewallPage() {
  const [status, jails, fail2banPolicy] = await Promise.all([
    agent.firewall.status().catch(() => null),
    agent.firewall.jails().catch(() => []),
    getFail2banPolicy(),
  ]);

  return (
    <div>
      <div className={styles.head}>
        <div>
          <h1 className={styles.title}>Firewall and bans</h1>
          <div className={styles.subtitle}>UFW rules and fail2ban jails.</div>
        </div>
      </div>

      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>UFW</h2>
        <p style={{ display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap" }}>
          <span>
            Status:{" "}
            <StatusBadge
              text={status?.ufw_enabled ? "active" : "inactive"}
              tone={status?.ufw_enabled ? "ok" : "danger"}
            />
          </span>
          {status && !status.ufw_enabled && (
            <form action={enableUfwAction}>
              <button type="submit" className={styles.btn}>Enable UFW</button>
            </form>
          )}
        </p>
        {status && !status.ufw_enabled && (
          <div className={styles.subtitle} style={{ marginTop: 4 }}>
            UFW is inactive — added rules below are queued but will not take effect until you enable it. Enabling auto-allows the port SSH listens on.
          </div>
        )}
        <RulesByProto rules={status?.rules ?? []} />
        {(!status || status.rules.length === 0) && (
          <div className={styles.empty} style={{ marginTop: 12 }}>No rules added.</div>
        )}
        <div style={{ marginTop: 16 }}>
          <PortForm action={firewallPortAction} />
        </div>
      </div>

      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>Open ports</h2>
        <div className={styles.subtitle} style={{ marginBottom: 10 }}>
          Sockets currently bound on this machine. Public means reachable on a non-loopback address.
        </div>
        <Table
          scrollX="always"
          maxRows={12}
          columns={[
            // All columns get explicit widths so the table has a real
            // intrinsic width (~870px) and triggers horizontal scroll
            // inside its purple card on mobile instead of squeezing
            // the bound-address / process strings.
            { key: "port", label: "Port", width: "90px" },
            { key: "proto", label: "Proto", width: "80px" },
            { key: "address", label: "Bound to", width: "220px" },
            { key: "exposure", label: "Exposure", width: "110px" },
            { key: "process", label: "Process", width: "320px" },
          ]}
          rows={[...(status?.listening_ports ?? [])]
            .sort((a, b) => {
              // Smallest port first; same port → tcp before udp for a
              // stable visual order across renders.
              if (a.port !== b.port) return a.port - b.port;
              return a.proto.localeCompare(b.proto);
            })
            .map((p) => ({
              port: <strong className={styles.code}>{p.port}</strong>,
              proto: <StatusBadge text={p.proto} tone="muted" />,
              address: <span className={styles.code}>{p.address}</span>,
              exposure: isLoopback(p.address) ? (
                <StatusBadge text="local" tone="muted" />
              ) : (
                <StatusBadge text="public" tone="warn" />
              ),
              process: p.process ? (
                <span className={styles.code}>{p.process}</span>
              ) : (
                <span className={styles.muted}>—</span>
              ),
            }))}
          empty="No listening sockets detected."
        />
      </div>

      <div className={styles.section}>
        <div
          style={{
            display: "flex",
            justifyContent: "space-between",
            alignItems: "center",
            gap: 12,
            marginBottom: 12,
            flexWrap: "wrap",
          }}
        >
          <div>
            <h2 className={styles.sectionTitle} style={{ marginBottom: 4 }}>
              fail2ban jails
            </h2>
            <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
              <span className={styles.muted}>Status:</span>
              <StatusBadge
                text={status?.fail2ban_active ? "active" : "inactive"}
                tone={status?.fail2ban_active ? "ok" : "muted"}
              />
            </div>
          </div>
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
            <form
              action={toggleFail2banAction.bind(null, !!status?.fail2ban_active)}
            >
              {status?.fail2ban_active ? (
                <ConfirmSubmitButton
                  className={styles.btnDanger}
                  message={
                    "Stop fail2ban?\n\nNew brute-force attempts will NOT be banned until you turn it back on. Existing bans in iptables persist until the next fail2ban restart or VM reboot. The service is also disabled so it won't come back on boot.\n\nLeave this on unless you're debugging a lockout."
                  }
                  pendingLabel="Stopping..."
                >
                  Stop fail2ban
                </ConfirmSubmitButton>
              ) : (
                <PendingButton className={styles.btn} pendingLabel="Starting...">
                  Start fail2ban
                </PendingButton>
              )}
            </form>
            <form action={restartFail2banAction}>
              <ConfirmSubmitButton
                className={styles.btnGhost}
                message={
                  "Restart fail2ban?\n\nIn-memory failure counters reset and all jails reload from disk. Bans persist (stored in fail2ban's sqlite DB). Use this after editing jail configs by hand or when fail2ban gets confused."
                }
                pendingLabel="Restarting..."
              >
                Restart fail2ban
              </ConfirmSubmitButton>
            </form>
          </div>
        </div>
        <div style={{ marginTop: 16, marginBottom: 16 }}>
          <h3
            className={styles.sectionTitle}
            style={{ fontSize: 14, marginBottom: 8 }}
          >
            Policy
          </h3>
          <Fail2banPolicyForm
            action={setFail2banPolicyAction}
            initial={fail2banPolicy}
          />
        </div>

        <div style={{ marginTop: 12 }}>
          <h3
            className={styles.sectionTitle}
            style={{ fontSize: 14, marginBottom: 8 }}
          >
            Active jails
          </h3>
          <JailsTable
            jails={jails}
            unbanIpAction={unbanIpAction}
            unbanAllAction={unbanAllAction}
            fetchFailedByIp={fetchFailedByIpAction}
          />
        </div>
      </div>
    </div>
  );
}

function isLoopback(addr: string) {
  return addr === "127.0.0.1" || addr === "::1" || addr === "localhost";
}

type ParsedRule = { proto: "tcp" | "udp" | "other"; display: string };

function parseRule(rule: string): ParsedRule {
  let display = rule.replace(/^ufw\s+/, "").replace(/^allow\s+/, "");
  const simple = display.match(/^(\d+(?::\d+)?)\/(tcp|udp)$/);
  if (simple) {
    return { proto: simple[2] as "tcp" | "udp", display: simple[1] };
  }
  const complex = display.match(/\bproto\s+(tcp|udp)\b/);
  if (complex) {
    display = display.replace(/\s*\bproto\s+(tcp|udp)\b/, "").trim();
    return { proto: complex[1] as "tcp" | "udp", display };
  }
  const inline = display.match(/^(\d+(?::\d+)?)$/);
  if (inline) return { proto: "other", display: inline[1] };
  return { proto: "other", display };
}

function RulesByProto({ rules }: { rules: string[] }) {
  if (rules.length === 0) return null;
  const buckets: Record<"tcp" | "udp" | "other", ParsedRule[]> = {
    tcp: [],
    udp: [],
    other: [],
  };
  for (const r of rules) {
    const p = parseRule(r);
    buckets[p.proto].push(p);
  }
  // Sort each bucket numerically ascending by port (or by the start of a
  // range like "8000:9000"). Falls back to a lexicographic tiebreak so
  // entries that aren't pure numbers (e.g. "from 1.2.3.4 to any port 22")
  // still land in a stable order.
  const byPort = (a: ParsedRule, b: ParsedRule) => {
    const pa = portKey(a.display);
    const pb = portKey(b.display);
    if (pa !== pb) return pa - pb;
    return a.display.localeCompare(b.display);
  };
  buckets.tcp.sort(byPort);
  buckets.udp.sort(byPort);
  buckets.other.sort(byPort);
  return (
    <div style={{ marginTop: 12, display: "flex", flexDirection: "column", gap: 14 }}>
      <ProtoSection label="TCP" rules={buckets.tcp} />
      <ProtoSection label="UDP" rules={buckets.udp} />
      <ProtoSection label="Other" rules={buckets.other} />
    </div>
  );
}

function portKey(display: string): number {
  // Pull the first integer out of the display string. Handles "22", "80",
  // "8000:9000" (sorts by 8000), and odd forms like "to any port 22"
  // (sorts by 22). Non-port entries land at +Infinity so they cluster at
  // the end of their bucket rather than at port 0.
  const m = display.match(/(\d{1,5})/);
  return m ? Number(m[1]) : Number.POSITIVE_INFINITY;
}

function ProtoSection({ label, rules }: { label: string; rules: ParsedRule[] }) {
  if (rules.length === 0) return null;
  return (
    <div>
      <div className={styles.subtitle} style={{ marginBottom: 6, fontWeight: 600 }}>
        {label}
      </div>
      <div style={{ display: "flex", flexWrap: "wrap", gap: 6 }}>
        {rules.map((r, i) => (
          <span key={i} className={styles.code} title={r.display}>
            {r.display}
          </span>
        ))}
      </div>
    </div>
  );
}
