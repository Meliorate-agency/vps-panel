import "server-only";
import { agent } from "../agent";
import { writeAudit } from "../audit";
import { nodeIdentity, ownName, ownUrl } from "./identity";
import { PeerError, fetchIdentity, peerRequest } from "./peer";
import { HEARTBEAT_MS, OFFLINE_MS, ONLINE_MS, cleanStats, nameOrNull, type PeerStats } from "./protocol";
import { ownStats } from "./stats";
import {
  applyRemoval,
  getMember,
  lastSeen,
  listMembers,
  listNetworks,
  publishRoster,
  recordOutbound,
  setAlerted,
  upsertMember,
  type MemberRow,
  type NetworkRow,
} from "./store";
import { membersExcept, pushRoster, takeRoster } from "./sync";

/**
 * Every 10 s: ping every member of every network this server is in, then
 * send the offline / back online alerts (Notifications). Started once per
 * panel process, from src/instrumentation.ts.
 */
export function startHeartbeat(): void {
  const g = globalThis as { __vpsnetHeartbeat?: ReturnType<typeof setInterval> };
  if (g.__vpsnetHeartbeat) return;
  const startedAt = Date.now();
  let busy = false;
  let beats = 0;
  g.__vpsnetHeartbeat = setInterval(() => {
    // A slow round (every ping waits up to 5 s) skips a beat, never stacks.
    if (busy) return;
    busy = true;
    tick(startedAt, beats++ % 6 === 0)
      .catch((e) => console.error("[networks] heartbeat:", e))
      .finally(() => {
        busy = false;
      });
  }, HEARTBEAT_MS);
  g.__vpsnetHeartbeat.unref();
}

async function tick(startedAt: number, checkAddress: boolean): Promise<void> {
  const networks = listNetworks();
  if (networks.length === 0) return;
  const me = nodeIdentity().publicKey;
  const name = ownName();
  if (checkAddress) await Promise.allSettled(networks.map((net) => readdress(net, me, name)));
  const stats = await ownStats();
  const pings: Promise<void>[] = [];
  for (const net of networks) {
    for (const m of listMembers(net.id)) {
      if (m.public_key !== me) pings.push(ping(net, m, name, stats));
    }
  }
  await Promise.allSettled(pings);
  checkAlerts(startedAt, me, name);
}

async function ping(net: NetworkRow, m: MemberRow, name: string, stats: PeerStats | null): Promise<void> {
  try {
    const { data, rttMs } = await peerRequest({
      url: m.url,
      path: "/api/peer/heartbeat",
      to: m.public_key,
      body: { network: net.id, rosterVersion: net.roster_version, name, stats },
      timeoutMs: 5_000,
    });
    recordOutbound(net.id, m.public_key, {
      ok: true,
      rttMs,
      name: nameOrNull(data.name),
      stats: cleanStats(data.stats),
    });
    // It holds a newer member list than this server.
    if (data.roster !== undefined) takeRoster(data.roster, data.sig, `heartbeat to ${m.url}`);
    // It missed a change (it could not reach the owner): the owner hands
    // the list over.
    if (
      net.owner_key === nodeIdentity().publicKey &&
      typeof data.rosterVersion === "number" &&
      data.rosterVersion < net.roster_version
    ) {
      await pushRoster(net.id, { json: net.roster_json, sig: net.roster_sig }, [{ key: m.public_key, url: m.url }]);
    }
  } catch (e) {
    recordOutbound(net.id, m.public_key, { ok: false, error: e instanceof Error ? e.message : String(e) });
    // The owner removed this server while it could not be told.
    if (
      e instanceof PeerError &&
      e.status === 403 &&
      m.public_key === net.owner_key &&
      applyRemoval(net.id, e.body?.removed)
    ) {
      writeAudit({
        actorId: null,
        action: "networks.removed",
        target: net.name,
        result: "ok",
        message: `the network's owner removed this server (via=heartbeat to ${m.url})`,
      });
    }
  }
}

/**
 * The panel's host name changed since this server joined (Settings > Panel
 * hostname): the others still ping the old address. Once it answers at the
 * new one with this server's key, the owner updates its own entry; a member
 * asks the owner to (a join with its key and no code, which the owner
 * checks the same way).
 */
async function readdress(net: NetworkRow, me: string, name: string): Promise<void> {
  const self = getMember(net.id, me);
  const own = ownUrl();
  if (!self || !own.url || self.url === own.url) return;
  try {
    if ((await fetchIdentity(own.url)).key !== me) return;
    if (net.owner_key === me) {
      upsertMember(net.id, { key: me, name: self.name, url: own.url });
      const roster = publishRoster(net.id);
      await pushRoster(net.id, roster, membersExcept(net.id));
    } else {
      const owner = getMember(net.id, net.owner_key);
      if (!owner) return;
      const { data } = await peerRequest({
        url: owner.url,
        path: "/api/peer/join",
        to: owner.public_key,
        body: { network: net.id, name, url: own.url },
        timeoutMs: 25_000,
      });
      takeRoster(data.roster, data.sig, `address change at ${owner.url}`);
    }
    writeAudit({
      actorId: null,
      action: "networks.readdress",
      target: net.name,
      result: "ok",
      message: `this server's address ${self.url} -> ${own.url}`,
    });
  } catch (e) {
    console.warn(`[networks] address change in "${net.name}" not taken yet:`, e instanceof Error ? e.message : e);
  }
}

function checkAlerts(startedAt: number, me: string, self: string): void {
  const now = Date.now();
  for (const net of listNetworks()) {
    for (const m of listMembers(net.id)) {
      if (m.public_key === me) continue;
      const seen = lastSeen(m);
      // Nobody had a chance to ping in the first minute after a restart
      // or after joining.
      const down = now - Math.max(seen ?? 0, m.joined_at, startedAt) >= OFFLINE_MS;
      const name = m.live_name || m.name;
      if (down && m.alerted_at === null) {
        setAlerted(net.id, m.public_key, now);
        alert(
          "network.peer_down",
          `${name} is offline`,
          [
            `${name} (${m.url}) in the network "${net.name}" has not answered ${self} for over a minute.`,
            seen ? `Last seen: ${new Date(seen).toISOString()}.` : "It has not answered since it joined.",
            m.out_error ? `Last error: ${m.out_error}` : "",
          ]
            .filter(Boolean)
            .join("\n"),
        );
      } else if (!down && m.alerted_at !== null && seen && now - seen < ONLINE_MS) {
        setAlerted(net.id, m.public_key, null);
        const minutes = Math.max(1, Math.round((now - m.alerted_at + OFFLINE_MS) / 60_000));
        alert(
          "network.peer_up",
          `${name} is back online`,
          `${name} (${m.url}) in the network "${net.name}" answers ${self} again, after about ${minutes} min offline.`,
        );
      }
    }
  }
}

function alert(event: string, subject: string, body: string): void {
  agent.notifications
    .send(event, subject, body)
    .catch((e) => console.error(`[networks] ${event} alert not sent:`, e instanceof Error ? e.message : e));
}
