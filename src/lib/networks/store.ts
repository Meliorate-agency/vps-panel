import "server-only";
import crypto from "node:crypto";
import { db } from "../db";
import { nodeIdentity, ownName } from "./identity";
import {
  INVITE_TTL_MS,
  OFFLINE_MS,
  ONLINE_MS,
  cleanStats,
  fingerprint,
  parseRoster,
  peerStatus,
  randomToken,
  removalMessage,
  rosterMessage,
  sha256,
  verifySig,
  type PeerStats,
  type PeerStatus,
  type Roster,
} from "./protocol";

export type NetworkRow = {
  id: string;
  name: string;
  owner_key: string;
  roster_version: number;
  roster_json: string;
  roster_sig: string;
  created_at: string;
};

export type MemberRow = {
  network_id: string;
  public_key: string;
  name: string;
  url: string;
  joined_at: number;
  live_name: string | null;
  last_in_at: number | null;
  last_out_ok_at: number | null;
  last_rtt_ms: number | null;
  out_error: string | null;
  stats_json: string | null;
  alerted_at: number | null;
};

export type InviteRow = {
  id: string;
  network_id: string;
  expires_at: number;
  used_at: number | null;
  used_by: string | null;
};

export function listNetworks(): NetworkRow[] {
  return db().prepare(`SELECT * FROM networks ORDER BY created_at, name`).all() as NetworkRow[];
}

export function getNetwork(id: string): NetworkRow | undefined {
  return db().prepare(`SELECT * FROM networks WHERE id = ?`).get(id) as NetworkRow | undefined;
}

export function listMembers(networkId: string): MemberRow[] {
  return db()
    .prepare(`SELECT * FROM network_members WHERE network_id = ? ORDER BY joined_at, name`)
    .all(networkId) as MemberRow[];
}

export function getMember(networkId: string, key: string): MemberRow | undefined {
  return db()
    .prepare(`SELECT * FROM network_members WHERE network_id = ? AND public_key = ?`)
    .get(networkId, key) as MemberRow | undefined;
}

export function isOwner(net: NetworkRow): boolean {
  return net.owner_key === nodeIdentity().publicKey;
}

export function memberStats(m: MemberRow): PeerStats | null {
  if (!m.stats_json) return null;
  try {
    return cleanStats(JSON.parse(m.stats_json));
  } catch {
    return null;
  }
}

/** The last ping either way that got through. */
export function lastSeen(m: MemberRow): number | null {
  return Math.max(m.last_in_at ?? 0, m.last_out_ok_at ?? 0) || null;
}

function signRoster(roster: Roster): { json: string; sig: string } {
  const json = JSON.stringify(roster);
  return { json, sig: nodeIdentity().sign(rosterMessage(json)) };
}

/** Owner: sign the current member list as the next version and keep it. */
export function publishRoster(networkId: string): { json: string; sig: string } {
  const d = db();
  return d
    .transaction(() => {
      const net = getNetwork(networkId);
      if (!net) throw new Error("no such network");
      const signed = signRoster({
        v: 1,
        network: net.id,
        name: net.name,
        owner: net.owner_key,
        version: net.roster_version + 1,
        dissolved: false,
        members: listMembers(net.id).map((m) => ({
          key: m.public_key,
          name: m.name,
          url: m.url,
          joined: m.joined_at,
        })),
      });
      d.prepare(`UPDATE networks SET roster_version = roster_version + 1, roster_json = ?, roster_sig = ? WHERE id = ?`).run(
        signed.json,
        signed.sig,
        net.id,
      );
      return signed;
    })
    .immediate();
}

/** Owner: the last list, which tells every member the network is gone. */
export function dissolvedRoster(net: NetworkRow): { json: string; sig: string } {
  return signRoster({
    v: 1,
    network: net.id,
    name: net.name,
    owner: net.owner_key,
    version: net.roster_version + 1,
    dissolved: true,
    members: [],
  });
}

export function createNetwork(name: string, self: { name: string; url: string }): NetworkRow {
  const id = randomToken(16);
  const d = db();
  d.transaction(() => {
    d.prepare(`INSERT INTO networks (id, name, owner_key) VALUES (?, ?, ?)`).run(id, name, nodeIdentity().publicKey);
    d.prepare(
      `INSERT INTO network_members (network_id, public_key, name, url, joined_at) VALUES (?, ?, ?, ?, ?)`,
    ).run(id, nodeIdentity().publicKey, self.name, self.url, Date.now());
  })();
  publishRoster(id);
  return getNetwork(id)!;
}

/** Owner: add a server, or update the address of one joining again. */
export function upsertMember(networkId: string, m: { key: string; name: string; url: string }): void {
  db()
    .prepare(
      `INSERT INTO network_members (network_id, public_key, name, url, joined_at) VALUES (?, ?, ?, ?, ?)
       ON CONFLICT(network_id, public_key) DO UPDATE SET name = excluded.name, url = excluded.url`,
    )
    .run(networkId, m.key, m.name, m.url, Date.now());
}

export function removeMember(networkId: string, key: string): boolean {
  return (
    db().prepare(`DELETE FROM network_members WHERE network_id = ? AND public_key = ?`).run(networkId, key)
      .changes > 0
  );
}

export function deleteNetworkLocal(id: string): void {
  db().prepare(`DELETE FROM networks WHERE id = ?`).run(id);
}

export type RosterResult = "applied" | "stale" | "removed" | "dissolved" | "unknown";

/**
 * Take a member list from whoever hands one over. It counts only when the
 * network's pinned owner key signed it and it is newer than the one here.
 * `pinnedOwner` is for joining, before there is a network row: the key
 * comes from the join code.
 */
export function applyRoster(
  json: unknown,
  sig: unknown,
  pinnedOwner?: string,
): { result: RosterResult; roster: Roster } {
  const roster = parseRoster(json);
  const me = nodeIdentity().publicKey;
  const d = db();
  const result = d
    .transaction((): RosterResult => {
      const existing = getNetwork(roster.network);
      const owner = existing?.owner_key ?? pinnedOwner;
      if (!owner) return "unknown";
      if (roster.owner !== owner || !verifySig(owner, rosterMessage(json as string), sig)) {
        throw new Error("the member list is not signed by the network's owner");
      }
      // The owner's own list is the original.
      if (owner === me) return "stale";
      if (existing && roster.version <= existing.roster_version) return "stale";
      if (roster.dissolved || !roster.members.some((m) => m.key === me)) {
        if (existing) deleteNetworkLocal(roster.network);
        return roster.dissolved ? "dissolved" : "removed";
      }
      d.prepare(
        `INSERT INTO networks (id, name, owner_key, roster_version, roster_json, roster_sig)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET name = excluded.name, roster_version = excluded.roster_version,
           roster_json = excluded.roster_json, roster_sig = excluded.roster_sig`,
      ).run(roster.network, roster.name, owner, roster.version, json as string, sig as string);
      const keep = new Set(roster.members.map((m) => m.key));
      for (const m of listMembers(roster.network)) {
        if (!keep.has(m.public_key)) removeMember(roster.network, m.public_key);
      }
      const up = d.prepare(
        `INSERT INTO network_members (network_id, public_key, name, url, joined_at) VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(network_id, public_key) DO UPDATE SET name = excluded.name, url = excluded.url,
           joined_at = excluded.joined_at`,
      );
      for (const m of roster.members) up.run(roster.network, m.key, m.name, m.url, m.joined);
      return "applied";
    })
    .immediate();
  return { result, roster };
}

/** Owner: the signed notice for a server that pings but is no member (any more). */
export function removalNotice(net: NetworkRow, key: string): { version: number; sig: string } {
  return { version: net.roster_version, sig: nodeIdentity().sign(removalMessage(net.id, key, net.roster_version)) };
}

/**
 * A removal notice for this server, from the network's owner: drop the
 * network. Only when it is newer than the member list here, so an old one
 * cannot undo joining again.
 */
export function applyRemoval(networkId: string, notice: unknown): boolean {
  const net = getNetwork(networkId);
  if (!net || !notice || typeof notice !== "object") return false;
  const { version, sig } = notice as { version?: unknown; sig?: unknown };
  if (typeof version !== "number" || version <= net.roster_version) return false;
  if (!verifySig(net.owner_key, removalMessage(net.id, nodeIdentity().publicKey, version), sig)) return false;
  deleteNetworkLocal(net.id);
  return true;
}

export function createInvite(networkId: string): { id: string; secret: string; expires: number } {
  const id = randomToken(16);
  const secret = randomToken(32);
  const expires = Date.now() + INVITE_TTL_MS;
  const d = db();
  d.prepare(`INSERT INTO network_invites (id, network_id, secret_hash, expires_at) VALUES (?, ?, ?, ?)`).run(
    id,
    networkId,
    sha256(secret),
    expires,
  );
  d.prepare(`DELETE FROM network_invites WHERE expires_at < ?`).run(Date.now() - 86_400_000);
  return { id, secret, expires };
}

export function listOpenInvites(networkId: string): InviteRow[] {
  return db()
    .prepare(
      `SELECT id, network_id, expires_at, used_at, used_by FROM network_invites
       WHERE network_id = ? AND used_at IS NULL AND expires_at > ? ORDER BY expires_at`,
    )
    .all(networkId, Date.now()) as InviteRow[];
}

export function revokeInvite(networkId: string, id: string): void {
  db().prepare(`DELETE FROM network_invites WHERE id = ? AND network_id = ?`).run(id, networkId);
}

/** Why this join code cannot be used, or null. */
export function checkInvite(networkId: string, id: string, secret: string): string | null {
  const row = db()
    .prepare(`SELECT network_id, secret_hash, expires_at, used_at FROM network_invites WHERE id = ?`)
    .get(id) as { network_id: string; secret_hash: string; expires_at: number; used_at: number | null } | undefined;
  const given = Buffer.from(sha256(secret));
  const valid =
    !!row &&
    row.network_id === networkId &&
    given.length === Buffer.byteLength(row.secret_hash) &&
    crypto.timingSafeEqual(given, Buffer.from(row.secret_hash));
  if (!row || !valid) return "this join code is not valid (revoked, or made for another network)";
  if (row.used_at) return "this join code was already used; ask the network's owner for a new one";
  if (row.expires_at <= Date.now()) return "this join code expired; ask the network's owner for a new one";
  return null;
}

/** Mark the code used. False: another join got it first, or it just expired. */
export function claimInvite(id: string, usedBy: string): boolean {
  const now = Date.now();
  return (
    db()
      .prepare(
        `UPDATE network_invites SET used_at = ?, used_by = ? WHERE id = ? AND used_at IS NULL AND expires_at > ?`,
      )
      .run(now, usedBy, id, now).changes === 1
  );
}

export function recordInbound(networkId: string, key: string, name: string | null, stats: PeerStats | null): void {
  db()
    .prepare(
      `UPDATE network_members SET last_in_at = ?, live_name = COALESCE(?, live_name),
         stats_json = COALESCE(?, stats_json)
       WHERE network_id = ? AND public_key = ?`,
    )
    .run(Date.now(), name, stats ? JSON.stringify(stats) : null, networkId, key);
}

export function recordOutbound(
  networkId: string,
  key: string,
  r: { ok: true; rttMs: number; name: string | null; stats: PeerStats | null } | { ok: false; error: string },
): void {
  if (r.ok) {
    db()
      .prepare(
        `UPDATE network_members SET last_out_ok_at = ?, last_rtt_ms = ?, out_error = NULL,
           live_name = COALESCE(?, live_name), stats_json = COALESCE(?, stats_json)
         WHERE network_id = ? AND public_key = ?`,
      )
      .run(Date.now(), r.rttMs, r.name, r.stats ? JSON.stringify(r.stats) : null, networkId, key);
  } else {
    db()
      .prepare(`UPDATE network_members SET out_error = ? WHERE network_id = ? AND public_key = ?`)
      .run(r.error.slice(0, 300), networkId, key);
  }
}

export function setAlerted(networkId: string, key: string, at: number | null): void {
  db()
    .prepare(`UPDATE network_members SET alerted_at = ? WHERE network_id = ? AND public_key = ?`)
    .run(at, networkId, key);
}

export type MemberView = {
  key: string;
  fingerprint: string;
  name: string;
  url: string;
  self: boolean;
  owner: boolean;
  status: PeerStatus | "self";
  lastSeen: number | null;
  rttMs: number | null;
  error: string | null;
  /** Its pings reach this server (within the online window). */
  hearing: boolean;
  /** Its pings stopped reaching this server while ours still reach it. */
  oneWay: boolean;
  stats: PeerStats | null;
  joinedAt: number;
};

export type NetworkView = {
  id: string;
  name: string;
  owned: boolean;
  ownerName: string;
  members: MemberView[];
};

/** Networks as the page and the MCP tool show them. */
export function networksView(now = Date.now()): NetworkView[] {
  const me = nodeIdentity().publicKey;
  return listNetworks().map((net) => {
    const members = listMembers(net.id).map((m): MemberView => {
      const self = m.public_key === me;
      const seen = lastSeen(m);
      return {
        key: m.public_key,
        fingerprint: fingerprint(m.public_key),
        name: self ? ownName() : m.live_name || m.name,
        url: m.url,
        self,
        owner: m.public_key === net.owner_key,
        status: self ? "self" : peerStatus(seen, m.joined_at, now),
        lastSeen: seen,
        rttMs: m.last_rtt_ms,
        error: self ? null : m.out_error,
        hearing: !self && !!m.last_in_at && now - m.last_in_at < ONLINE_MS,
        oneWay:
          !self &&
          !!m.last_out_ok_at &&
          now - m.last_out_ok_at < ONLINE_MS &&
          now - (m.last_in_at ?? m.joined_at) > OFFLINE_MS,
        stats: memberStats(m),
        joinedAt: m.joined_at,
      };
    });
    return {
      id: net.id,
      name: net.name,
      owned: net.owner_key === me,
      ownerName: members.find((m) => m.owner)?.name ?? "unknown",
      members,
    };
  });
}
