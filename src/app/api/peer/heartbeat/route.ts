import type { NextRequest } from "next/server";
import { nodeIdentity, ownName } from "@/lib/networks/identity";
import { peerError, readSignedRequest, signedJson } from "@/lib/networks/peer";
import { cleanStats, nameOrNull } from "@/lib/networks/protocol";
import { ownStats } from "@/lib/networks/stats";
import { getMember, getNetwork, recordInbound, removalNotice } from "@/lib/networks/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

/**
 * POST /api/peer/heartbeat - another member's ping (every 10 s), signed
 * with its key. It carries its name and stats; the answer carries this
 * server's, plus the signed member list when the sender's is older.
 */
export async function POST(req: NextRequest) {
  // Every member pings every 10 s: 6 a minute per network it shares.
  const r = await readSignedRequest(req, 600);
  if (r instanceof Response) return r;
  const { key, nonce, body } = r;

  const net = typeof body.network === "string" ? getNetwork(body.network) : undefined;
  if (!net) return peerError(404, "this server is not in that network (it left, or the network was deleted)");
  if (!getMember(net.id, key)) {
    // A server the owner removed keeps pinging until it hears. The owner
    // hands it a signed notice naming its key (not the member list), and it
    // drops the network.
    if (net.owner_key === nodeIdentity().publicKey) {
      return peerError(403, "not a member of this network", { removed: removalNotice(net, key) });
    }
    return peerError(403, "not a member of this network");
  }
  recordInbound(net.id, key, nameOrNull(body.name), cleanStats(body.stats));

  const answer: Record<string, unknown> = {
    network: net.id,
    rosterVersion: net.roster_version,
    name: ownName(),
    stats: await ownStats(),
  };
  const theirs = typeof body.rosterVersion === "number" ? body.rosterVersion : 0;
  if (net.roster_version > theirs && net.roster_json) {
    answer.roster = net.roster_json;
    answer.sig = net.roster_sig;
  }
  return signedJson(answer, nonce);
}
