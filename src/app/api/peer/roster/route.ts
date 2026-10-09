import type { NextRequest } from "next/server";
import { peerError, readSignedRequest, signedJson } from "@/lib/networks/peer";
import { getNetwork } from "@/lib/networks/store";
import { takeRoster } from "@/lib/networks/sync";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

/**
 * POST /api/peer/roster - the network's owner hands over a new member list
 * right after a change (a join, a removal, a leave, the network deleted).
 * Taken only when the pinned owner key signed it and it is newer.
 */
export async function POST(req: NextRequest) {
  const r = await readSignedRequest(req, 120);
  if (r instanceof Response) return r;
  const net = typeof r.body.network === "string" ? getNetwork(r.body.network) : undefined;
  if (!net) return peerError(404, "this server is not in that network");
  if (r.key !== net.owner_key) return peerError(403, "only the network's owner sends member lists");
  const result = takeRoster(r.body.roster, r.body.sig, "owner");
  if (!result) return peerError(400, "the member list was refused");
  return signedJson({ ok: true, result }, r.nonce);
}
