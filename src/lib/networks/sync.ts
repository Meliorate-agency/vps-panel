import "server-only";
import { writeAudit } from "../audit";
import { nodeIdentity } from "./identity";
import { peerRequest } from "./peer";
import { applyRoster, listMembers, type RosterResult } from "./store";

export type Target = { key: string; url: string };

/** Members to tell about a change: everyone but this server and `skip`. */
export function membersExcept(networkId: string, ...skip: string[]): Target[] {
  const out = new Set([nodeIdentity().publicKey, ...skip]);
  return listMembers(networkId)
    .filter((m) => !out.has(m.public_key))
    .map((m) => ({ key: m.public_key, url: m.url }));
}

/**
 * Owner: hand a new member list to `targets` right away. Best effort: a
 * member that misses it gets it with its next ping to the owner.
 */
export async function pushRoster(networkId: string, roster: { json: string; sig: string }, targets: Target[]) {
  await Promise.allSettled(
    targets.map((t) =>
      peerRequest({
        url: t.url,
        path: "/api/peer/roster",
        to: t.key,
        body: { network: networkId, roster: roster.json, sig: roster.sig },
        timeoutMs: 5_000,
      }),
    ),
  );
}

/** A member list handed over by another server: apply it and log what it did to this one. */
export function takeRoster(json: unknown, sig: unknown, via: string): RosterResult | null {
  try {
    const { result, roster } = applyRoster(json, sig);
    if (result === "removed" || result === "dissolved") {
      writeAudit({
        actorId: null,
        action: result === "removed" ? "networks.removed" : "networks.dissolved",
        target: roster.name,
        result: "ok",
        message:
          result === "removed"
            ? `the network's owner removed this server (via=${via})`
            : `the network's owner deleted the network (via=${via})`,
      });
    }
    return result;
  } catch (e) {
    console.warn(`[networks] member list from ${via} refused:`, e instanceof Error ? e.message : e);
    return null;
  }
}
