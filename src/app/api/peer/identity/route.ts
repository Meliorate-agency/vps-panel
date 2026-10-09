import type { NextRequest } from "next/server";
import { nodeIdentity } from "@/lib/networks/identity";
import { clientIp, overLimit, peerError } from "@/lib/networks/peer";
import { identityMessage } from "@/lib/networks/protocol";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

/**
 * GET /api/peer/identity?c=<challenge> - this server's public key, signed
 * over the challenge. A network's owner calls it on a server that asks to
 * join, to check that the address it gave leads back to its key. Public:
 * the /api/peer/ routes skip the panel access list (servers in a network
 * reach each other from anywhere) and authenticate by signature instead.
 */
export async function GET(req: NextRequest) {
  if (overLimit("identity", clientIp(req), 60, 60_000)) return peerError(429, "too many requests");
  const c = req.nextUrl.searchParams.get("c") ?? "";
  if (!/^[A-Za-z0-9_-]{16,64}$/.test(c)) return peerError(400, "missing challenge");
  // The key alone: the name and anything else stay for the network's members.
  const id = nodeIdentity();
  return Response.json(
    { v: 1, key: id.publicKey, sig: id.sign(identityMessage(c)) },
    { headers: { "cache-control": "no-store" } },
  );
}
