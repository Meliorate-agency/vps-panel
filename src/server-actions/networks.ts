"use server";

import { revalidatePath } from "next/cache";
import { requireAdmin } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";
import { setSetting } from "@/lib/settings";
import { nodeIdentity, ownName, ownUrl } from "@/lib/networks/identity";
import { PeerError, fetchIdentity, peerRequest } from "@/lib/networks/peer";
import { cleanName, decodeInvite, encodeInvite, fingerprint, type Invite } from "@/lib/networks/protocol";
import {
  applyRoster,
  createInvite,
  createNetwork,
  deleteNetworkLocal,
  dissolvedRoster,
  getMember,
  getNetwork,
  isOwner,
  publishRoster,
  removeMember,
  revokeInvite,
} from "@/lib/networks/store";
import { membersExcept, pushRoster } from "@/lib/networks/sync";

export type ActionResult = { ok: true; message?: string } | { ok: false; error: string };

const errorText = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** This server's address, after checking it answers there with its own key:
 *  the other servers will use it. */
async function reachableSelf(): Promise<string> {
  const own = ownUrl();
  if (own.url === null) throw new Error(own.problem);
  let key: string;
  try {
    key = (await fetchIdentity(own.url)).key;
  } catch (e) {
    throw new Error(`Other servers could not reach this one at ${own.url}: ${errorText(e)}`);
  }
  if (key !== nodeIdentity().publicKey) {
    throw new Error(`${own.url} is answered by another server, not this one; check where its DNS points`);
  }
  return own.url;
}

/** The name other servers see; the page saves it as the operator types.
 *  Empty goes back to the host name. */
export async function saveNodeNameAction(raw: string): Promise<ActionResult> {
  const s = await requireAdmin();
  try {
    const trimmed = String(raw ?? "").trim();
    const name = trimmed ? cleanName(trimmed, "Server name") : "";
    setSetting("networks.node_name", name);
    writeAudit({ actorId: s.userId, action: "networks.rename_self", target: name || ownName(), result: "ok" });
    revalidatePath("/networks");
    return { ok: true };
  } catch (e) {
    return { ok: false, error: errorText(e) };
  }
}

export async function createNetworkAction(_prev: ActionResult | null, fd: FormData): Promise<ActionResult> {
  const s = await requireAdmin();
  let name = "";
  try {
    name = cleanName(fd.get("name"), "Network name");
    const url = await reachableSelf();
    createNetwork(name, { name: ownName(), url });
    writeAudit({ actorId: s.userId, action: "networks.create", target: name, result: "ok" });
    revalidatePath("/networks");
    return { ok: true };
  } catch (e) {
    writeAudit({ actorId: s.userId, action: "networks.create", target: name || null, result: "error", message: errorText(e) });
    return { ok: false, error: errorText(e) };
  }
}

export type InvitePreview = { name: string; url: string; ownerFingerprint: string; expires: number };

function usableInvite(code: string): Invite {
  const invite = decodeInvite(code);
  if (invite.expires <= Date.now()) throw new Error("this join code expired; ask the network's owner for a new one");
  if (invite.owner === nodeIdentity().publicKey) throw new Error("this code is for a network this server owns");
  if (getNetwork(invite.network)) throw new Error(`this server is already in "${invite.name}"`);
  return invite;
}

/** What a join code leads to, shown before joining. Nothing is sent yet. */
export async function previewInviteAction(
  code: string,
): Promise<{ ok: true; invite: InvitePreview } | { ok: false; error: string }> {
  await requireAdmin();
  try {
    const i = usableInvite(code);
    return { ok: true, invite: { name: i.name, url: i.url, ownerFingerprint: fingerprint(i.owner), expires: i.expires } };
  } catch (e) {
    return { ok: false, error: errorText(e) };
  }
}

export async function joinNetworkAction(code: string): Promise<ActionResult> {
  const s = await requireAdmin();
  let invite: Invite | null = null;
  try {
    invite = usableInvite(code);
    const url = await reachableSelf();
    let data: Record<string, unknown>;
    try {
      ({ data } = await peerRequest({
        url: invite.url,
        path: "/api/peer/join",
        to: invite.owner,
        body: { network: invite.network, invite: invite.id, secret: invite.secret, name: ownName(), url },
        // The owner calls back this server's address before it answers.
        timeoutMs: 25_000,
      }));
    } catch (e) {
      if (e instanceof PeerError && e.status === 0) {
        throw new Error(`could not reach the network's owner at ${invite.url}: ${e.message}`);
      }
      throw e;
    }
    const { result } = applyRoster(data.roster, data.sig, invite.owner);
    if (result !== "applied") throw new Error(`the owner's answer did not add this server (${result})`);
    writeAudit({
      actorId: s.userId,
      action: "networks.join",
      target: invite.name,
      result: "ok",
      message: `owner ${invite.url} ${fingerprint(invite.owner)}`,
    });
    revalidatePath("/networks");
    return { ok: true, message: `Joined "${invite.name}". The first pings arrive within 10 seconds.` };
  } catch (e) {
    writeAudit({
      actorId: s.userId,
      action: "networks.join",
      target: invite?.name ?? null,
      result: "error",
      message: errorText(e),
    });
    return { ok: false, error: errorText(e) };
  }
}

export async function createInviteAction(
  networkId: string,
): Promise<{ ok: true; code: string; expires: number } | { ok: false; error: string }> {
  const s = await requireAdmin();
  const net = getNetwork(networkId);
  if (!net || !isOwner(net)) return { ok: false, error: "only the network's owner invites servers" };
  const own = ownUrl();
  if (own.url === null) return { ok: false, error: own.problem };
  const inv = createInvite(net.id);
  const code = encodeInvite({
    network: net.id,
    name: net.name,
    url: own.url,
    owner: net.owner_key,
    id: inv.id,
    secret: inv.secret,
    expires: inv.expires,
  });
  writeAudit({
    actorId: s.userId,
    action: "networks.invite",
    target: net.name,
    result: "ok",
    message: `code ${inv.id}, valid until ${new Date(inv.expires).toISOString()}`,
  });
  revalidatePath("/networks");
  return { ok: true, code, expires: inv.expires };
}

export async function revokeInviteAction(networkId: string, inviteId: string): Promise<void> {
  const s = await requireAdmin();
  const net = getNetwork(networkId);
  if (!net || !isOwner(net)) return;
  revokeInvite(net.id, inviteId);
  writeAudit({ actorId: s.userId, action: "networks.revoke_invite", target: net.name, result: "ok", message: `code ${inviteId}` });
  revalidatePath("/networks");
}

export async function removeMemberAction(networkId: string, key: string): Promise<ActionResult> {
  const s = await requireAdmin();
  const net = getNetwork(networkId);
  if (!net || !isOwner(net)) return { ok: false, error: "only the network's owner removes servers" };
  if (key === nodeIdentity().publicKey) return { ok: false, error: "delete the network instead" };
  const member = getMember(net.id, key);
  if (!member) return { ok: false, error: "that server is not in the network" };
  removeMember(net.id, key);
  const roster = publishRoster(net.id);
  // The removed server hears it too, so it stops pinging.
  await pushRoster(net.id, roster, [...membersExcept(net.id), { key, url: member.url }]);
  writeAudit({
    actorId: s.userId,
    action: "networks.remove",
    target: net.name,
    result: "ok",
    message: `${member.live_name || member.name} ${member.url} ${fingerprint(key)}`,
  });
  revalidatePath("/networks");
  return { ok: true };
}

export async function leaveNetworkAction(networkId: string): Promise<ActionResult> {
  const s = await requireAdmin();
  const net = getNetwork(networkId);
  if (!net) return { ok: false, error: "no such network" };
  if (isOwner(net)) return { ok: false, error: "this server owns the network: delete it instead" };
  const owner = getMember(net.id, net.owner_key);
  let note = "";
  try {
    if (!owner) throw new Error("the owner is not in the member list");
    await peerRequest({ url: owner.url, path: "/api/peer/leave", to: owner.public_key, body: { network: net.id }, timeoutMs: 8_000 });
  } catch (e) {
    note = `Left "${net.name}", but its owner could not be told (${errorText(e)}), so the owner shows this server offline until it removes it.`;
  }
  deleteNetworkLocal(net.id);
  writeAudit({ actorId: s.userId, action: "networks.leave", target: net.name, result: "ok", message: note || undefined });
  revalidatePath("/networks");
  // A message only when there is more to say than the card disappearing.
  return note ? { ok: true, message: note } : { ok: true };
}

export async function deleteNetworkAction(networkId: string): Promise<ActionResult> {
  const s = await requireAdmin();
  const net = getNetwork(networkId);
  if (!net || !isOwner(net)) return { ok: false, error: "only the network's owner deletes it" };
  // Tell every member first: they drop it when the signed final list arrives.
  await pushRoster(net.id, dissolvedRoster(net), membersExcept(net.id));
  deleteNetworkLocal(net.id);
  writeAudit({ actorId: s.userId, action: "networks.delete", target: net.name, result: "ok" });
  revalidatePath("/networks");
  return { ok: true };
}
