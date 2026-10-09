import { after, type NextRequest } from "next/server";
import { writeAudit } from "@/lib/audit";
import { nodeIdentity } from "@/lib/networks/identity";
import {
  clientIp,
  fetchIdentity,
  noteFailure,
  overLimit,
  peerError,
  readSignedRequest,
  signedJson,
} from "@/lib/networks/peer";
import { cleanName, fingerprint, normalizePeerUrl } from "@/lib/networks/protocol";
import {
  checkInvite,
  claimInvite,
  getMember,
  getNetwork,
  isOwner,
  listMembers,
  publishRoster,
  upsertMember,
} from "@/lib/networks/store";
import { membersExcept, pushRoster } from "@/lib/networks/sync";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

/**
 * POST /api/peer/join - a server joins a network this server owns, with a
 * one-time join code. The request is signed with the joining server's key;
 * before adding it, this server fetches /api/peer/identity at the address
 * it gave and checks that it answers with the same key. The answer is the
 * new member list, signed with the owner key the joiner pinned from the
 * code. A member moving to a new address comes through here too, without a
 * code.
 */
export async function POST(req: NextRequest) {
  const ip = clientIp(req);
  if (overLimit("join", ip, 20, 10 * 60_000)) {
    return peerError(429, "too many join attempts from this address; wait a few minutes");
  }
  const r = await readSignedRequest(req, 60);
  if (r instanceof Response) return r;
  const { key, nonce, body } = r;

  const net = typeof body.network === "string" ? getNetwork(body.network) : undefined;
  if (!net || !isOwner(net)) {
    noteFailure(ip);
    return peerError(404, "this server does not own that network");
  }
  // A member already in the network needs no code: it is moving to a new
  // address (its panel's host name changed), checked like a join below.
  const existing = getMember(net.id, key);
  if (!existing) {
    if (typeof body.invite !== "string" || typeof body.secret !== "string") {
      return peerError(400, "the join code is missing");
    }
    const refused = checkInvite(net.id, body.invite, body.secret);
    if (refused) {
      noteFailure(ip);
      return peerError(403, refused);
    }
  }
  let name: string;
  let url: string;
  try {
    name = cleanName(body.name, "Server name");
    url = normalizePeerUrl(body.url);
  } catch (e) {
    return peerError(400, e instanceof Error ? e.message : String(e));
  }
  if (key === nodeIdentity().publicKey) return peerError(400, "a server cannot join its own network");
  const clash = listMembers(net.id).find((m) => m.url === url && m.public_key !== key);
  if (clash) return peerError(409, `${url} is already in this network, as ${clash.live_name || clash.name}`);

  try {
    const found = await fetchIdentity(url);
    if (found.key !== key) {
      return peerError(400, `${url} answers with another server's key; is that really this server's panel address?`);
    }
  } catch (e) {
    return peerError(400, `the network's owner could not reach ${url}: ${e instanceof Error ? e.message : e}`);
  }

  if (!existing && !claimInvite(String(body.invite), key)) {
    return peerError(403, "this join code was just used, or it expired; ask for a new one");
  }
  upsertMember(net.id, { key, name, url });
  const roster = publishRoster(net.id);
  writeAudit({
    actorId: null,
    action: existing ? "networks.readdress" : "networks.join",
    target: net.name,
    result: "ok",
    message: existing
      ? `${name} ${fingerprint(key)} moved ${existing.url} -> ${url} (via=peer ip=${ip})`
      : `${name} ${url} ${fingerprint(key)} joined (via=peer ip=${ip})`,
  });
  after(() => pushRoster(net.id, roster, membersExcept(net.id, key)));
  return signedJson({ roster: roster.json, sig: roster.sig }, nonce);
}
