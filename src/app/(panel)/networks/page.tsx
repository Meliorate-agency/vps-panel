import Table from "@/components/Table/Table";
import StatusBadge from "@/components/StatusBadge/StatusBadge";
import styles from "@/components/page.module.css";
import { config } from "@/lib/config";
import { getSetting } from "@/lib/settings";
import { nodeIdentity, ownUrl } from "@/lib/networks/identity";
import { HEARTBEAT_MS, type PeerStats } from "@/lib/networks/protocol";
import { ownStats } from "@/lib/networks/stats";
import { listOpenInvites, networksView, type MemberView, type NetworkView } from "@/lib/networks/store";
import {
  createInviteAction,
  createNetworkAction,
  deleteNetworkAction,
  joinNetworkAction,
  leaveNetworkAction,
  previewInviteAction,
  removeMemberAction,
  revokeInviteAction,
  saveNodeNameAction,
} from "@/server-actions/networks";
import ActionButton, { ActionDialogs } from "./ActionButton";
import AutoRefresh from "./AutoRefresh";
import InviteButton from "./InviteButton";
import NetworkSetup from "./NetworkSetup";
import NodeNameForm from "./NodeNameForm";
import local from "./networks.module.css";

export const dynamic = "force-dynamic";

const STATUS = {
  online: { text: "online", tone: "ok" },
  late: { text: "late", tone: "warn" },
  offline: { text: "offline", tone: "danger" },
  waiting: { text: "waiting", tone: "muted" },
  self: { text: "this server", tone: "info" },
} as const;

function ago(ms: number | null, now: number): string {
  if (!ms) return "never";
  const s = Math.max(0, Math.round((now - ms) / 1000));
  if (s < 60) return `${s} s ago`;
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
  return `${Math.floor(s / 86400)} d ago`;
}

function uptime(secs: number): string {
  const d = Math.floor(secs / 86400);
  const h = Math.floor((secs % 86400) / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (d > 0) return `${d} d ${h} h`;
  if (h > 0) return `${h} h ${m} min`;
  return `${m} min`;
}

function memberRow(net: NetworkView, m: MemberView, now: number, mine: PeerStats | null) {
  const stats = m.self ? mine : m.stats;
  const status = STATUS[m.status];
  return {
    status: <StatusBadge text={status.text} tone={status.tone} />,
    server: (
      <div className={local.server} title={`Key ${m.fingerprint}`}>
        <span>
          <strong>{m.name}</strong>
          {m.owner && <span className={local.tag}>owner</span>}
        </span>
        <span className={local.addr}>{m.url}</span>
        {m.error && (
          <span className={m.hearing ? local.hint : local.problem}>
            {m.hearing ? `Its pings arrive, but this server cannot reach it: ${m.error}` : m.error}
          </span>
        )}
        {m.oneWay && (
          <span className={local.hint}>
            Its pings stopped reaching this server (Panel access on this server, or a firewall between them?)
          </span>
        )}
      </div>
    ),
    latency: m.self ? "" : m.rttMs !== null && !m.error && m.status !== "offline" ? `${m.rttMs} ms` : "-",
    seen: m.self ? "" : <span className={local.nowrap}>{ago(m.lastSeen, now)}</span>,
    load: <span className={local.nowrap}>{stats ? `${stats.load1.toFixed(2)} / ${stats.cpus}` : "-"}</span>,
    mem: stats ? `${stats.mem}%` : "-",
    disk: stats ? `${stats.disk}%` : "-",
    uptime: <span className={local.nowrap}>{stats ? uptime(stats.uptime) : "-"}</span>,
    actions:
      net.owned && !m.self ? (
        <ActionButton
          label="Remove"
          title={`Remove ${m.name}?`}
          body={`${m.name} leaves "${net.name}" and stops pinging the other servers. It needs a new join code to come back.`}
          confirmLabel="Remove server"
          action={removeMemberAction.bind(null, net.id, m.key)}
        />
      ) : null,
  };
}

function NetworkCard({ net, now, mine, ownAddress }: { net: NetworkView; now: number; mine: PeerStats | null; ownAddress: string | null }) {
  const others = net.members.filter((m) => !m.self);
  const online = others.filter((m) => m.status === "online").length;
  const self = net.members.find((m) => m.self);
  const invites = net.owned ? listOpenInvites(net.id) : [];
  return (
    <div className={styles.section}>
      <div className={local.netHead}>
        <div>
          <h2 className={styles.sectionTitle}>{net.name}</h2>
          <div className={local.sub}>
            {others.length === 0
              ? "No other servers yet."
              : `${online} of ${others.length} other server${others.length === 1 ? "" : "s"} online.`}{" "}
            {net.owned
              ? "This server owns the network: it invites and removes servers."
              : `Owned by ${net.ownerName}, which invites and removes servers.`}
          </div>
        </div>
        <div className={styles.actions}>
          {net.owned && <InviteButton action={createInviteAction.bind(null, net.id)} />}
          {net.owned ? (
            <ActionButton
              label="Delete network"
              title={`Delete "${net.name}"?`}
              body="Every server in it drops the network and stops pinging the others. Its open join codes stop working."
              confirmLabel="Delete network"
              action={deleteNetworkAction.bind(null, net.id)}
            />
          ) : (
            <ActionButton
              label="Leave network"
              title={`Leave "${net.name}"?`}
              body={`This server stops pinging the others, and ${net.ownerName} takes it off the member list. Coming back needs a new join code from ${net.ownerName}.`}
              confirmLabel="Leave network"
              action={leaveNetworkAction.bind(null, net.id)}
            />
          )}
        </div>
      </div>
      {self && ownAddress && self.url !== ownAddress && (
        <div className={local.warnBox}>
          The others know this server as {self.url}, but its address is now {ownAddress}. It moves over by itself
          within a minute once {ownAddress} answers.
        </div>
      )}
      <Table
        scrollX="always"
        columns={[
          { key: "status", label: "Status", width: "110px" },
          { key: "server", label: "Server", width: "260px" },
          { key: "latency", label: "Latency", width: "90px" },
          { key: "seen", label: "Last seen", width: "110px" },
          { key: "load", label: "Load / CPUs", width: "120px" },
          { key: "mem", label: "Memory", width: "90px" },
          { key: "disk", label: "Disk", width: "80px" },
          { key: "uptime", label: "Uptime", width: "100px" },
          { key: "actions", label: "", width: "110px" },
        ]}
        rows={net.members.map((m) => memberRow(net, m, now, mine))}
        rowKey={(_, i) => net.members[i].key}
      />
      {invites.length > 0 && (
        <div className={local.invites}>
          {invites.map((inv) => (
            <div key={inv.id}>
              Unused join code, valid {Math.max(1, Math.round((inv.expires_at - now) / 60_000))} more min.{" "}
              <form action={revokeInviteAction.bind(null, net.id, inv.id)}>
                <button type="submit" className={styles.btnGhost}>
                  Revoke
                </button>
              </form>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

export default async function NetworksPage() {
  const id = nodeIdentity();
  const own = ownUrl();
  const now = Date.now();
  const networks = networksView(now);
  const mine = networks.length > 0 ? await ownStats() : null;

  return (
    <ActionDialogs>
      <AutoRefresh everyMs={HEARTBEAT_MS} />
      <div className={styles.head}>
        <div>
          <h1 className={styles.title}>My Networks</h1>
          <div className={styles.subtitle}>
            Servers in a network ping each other every 10 seconds over HTTPS, so each one shows the others&apos;
            status, latency and load. Every message is signed with the sending server&apos;s own key. A server joins
            with a one-time code from the network&apos;s owner, and only the owner adds or removes servers. A server
            that stops answering for a minute is reported to your Notifications channels.
          </div>
        </div>
      </div>

      {!own.url && <div className={styles.error}>{own.problem}</div>}

      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>This server</h2>
        <div className={local.serverCard}>
          <dl className={local.facts}>
            <dt>Address</dt>
            <dd>{own.url ? <span className={styles.code}>{own.url}</span> : <span className={styles.muted}>none yet</span>}</dd>
            <dt>Key fingerprint</dt>
            <dd>
              <span className={styles.code}>{id.fingerprint}</span>
            </dd>
          </dl>
          <NodeNameForm
            current={getSetting("networks.node_name", "")}
            fallback={config.panelHost || "the host name"}
            save={saveNodeNameAction}
          />
        </div>
      </div>

      <NetworkSetup
        createAction={createNetworkAction}
        previewAction={previewInviteAction}
        joinAction={joinNetworkAction}
        disabled={!own.url}
      />

      {networks.length === 0 ? (
        <div className={styles.empty}>
          This server is not in a network yet. Create one, or join one with a code from another server.
        </div>
      ) : (
        networks.map((net) => <NetworkCard key={net.id} net={net} now={now} mine={mine} ownAddress={own.url} />)
      )}
    </ActionDialogs>
  );
}
