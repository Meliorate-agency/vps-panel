import { after, type NextRequest } from "next/server";
import { writeAudit } from "@/lib/audit";
import { clientIp, peerError, readSignedRequest, signedJson } from "@/lib/networks/peer";
import { getMember, getNetwork, isOwner, publishRoster, removeMember } from "@/lib/networks/store";
import { membersExcept, pushRoster } from "@/lib/networks/sync";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

/**
 * POST /api/peer/leave - a member leaves a network this server owns. The
 * request is signed with the leaving server's key, so only it can take
 * itself out.
 */
export async function POST(req: NextRequest) {
  const r = await readSignedRequest(req, 60);
  if (r instanceof Response) return r;
  const net = typeof r.body.network === "string" ? getNetwork(r.body.network) : undefined;
  if (!net || !isOwner(net)) return peerError(404, "this server does not own that network");
  if (r.key === net.owner_key) return peerError(400, "the owner deletes the network instead");
  const member = getMember(net.id, r.key);
  if (member) {
    removeMember(net.id, r.key);
    const roster = publishRoster(net.id);
    writeAudit({
      actorId: null,
      action: "networks.leave",
      target: net.name,
      result: "ok",
      message: `${member.live_name || member.name} ${member.url} left (via=peer ip=${clientIp(req)})`,
    });
    after(() => pushRoster(net.id, roster, membersExcept(net.id)));
  }
  return signedJson({ ok: true }, r.nonce);
}
