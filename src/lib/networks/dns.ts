import type { LookupAddress } from "node:dns";
import { lookup as osLookup } from "node:dns/promises";
import { isIP } from "node:net";

/**
 * Name lookups for other servers' addresses, at most one per name at a time.
 *
 * Node resolves names with getaddrinfo on libuv's small thread pool (4
 * threads), the same pool that gzips the panel's pages, hashes passwords
 * and reads files. A lookup cannot be cancelled: a timed-out fetch leaves
 * its lookup holding a thread until the resolver gives up. When a member's
 * DNS stops answering (its domain's name servers are down), every 10 s ping
 * started one more such lookup, they piled up, and the pool ran out: pings
 * to the healthy members timed out too and the panel stalled.
 *
 * Here a caller joins the lookup already running for that name instead of
 * starting another, and stops waiting at its own deadline. A name whose
 * DNS hangs costs one thread, never more.
 *
 * No server-only import: plain Node, so it can be tried outside Next.
 */

export type Lookup = (host: string, opts: { all: true }) => Promise<LookupAddress[]>;

/** The name did not resolve, or not before the caller's deadline. */
export class DnsError extends Error {}

export const DNS_TIMEOUT_MESSAGE =
  "its name did not resolve in time (no DNS answer: are its domain's name servers down?)";
export const DNS_FAILED_MESSAGE = "its name does not resolve (DNS)";

export type HostResolver = {
  /** The addresses `host` resolves to. An IP address is returned as it is. */
  resolve(host: string, signal: AbortSignal): Promise<LookupAddress[]>;
  /** Lookups still running, by name. */
  inFlight(): number;
};

export function createHostResolver(lookup: Lookup = osLookup): HostResolver {
  const pending = new Map<string, Promise<LookupAddress[]>>();

  return {
    resolve(host, signal) {
      const name = host.replace(/^\[|\]$/g, "").toLowerCase();
      const family = isIP(name);
      if (family) return Promise.resolve([{ address: name, family }]);
      if (signal.aborted) return Promise.reject(new DnsError(DNS_TIMEOUT_MESSAGE));
      let running = pending.get(name);
      if (!running) {
        const started = lookup(name, { all: true }).finally(() => {
          if (pending.get(name) === started) pending.delete(name);
        });
        // Every caller may have stopped waiting by the time it fails.
        started.catch(() => {});
        pending.set(name, started);
        running = started;
      }
      return untilAborted(running, signal);
    },
    inFlight: () => pending.size,
  };
}

function untilAborted(lookup: Promise<LookupAddress[]>, signal: AbortSignal): Promise<LookupAddress[]> {
  if (signal.aborted) return Promise.reject(new DnsError(DNS_TIMEOUT_MESSAGE));
  return new Promise((resolve, reject) => {
    const onAbort = () => reject(new DnsError(DNS_TIMEOUT_MESSAGE));
    signal.addEventListener("abort", onAbort, { once: true });
    lookup.then(
      (addresses) => {
        signal.removeEventListener("abort", onAbort);
        if (addresses.length > 0) resolve(addresses);
        else reject(new DnsError(DNS_FAILED_MESSAGE));
      },
      () => {
        signal.removeEventListener("abort", onAbort);
        reject(new DnsError(DNS_FAILED_MESSAGE));
      },
    );
  });
}

/**
 * The one resolver for the heartbeat loop, the peer routes and the page. On
 * globalThis: they can be separate module instances, and they must share
 * the running lookups.
 */
export function peerHosts(): HostResolver {
  const g = globalThis as { __vpsnetHosts?: HostResolver };
  return (g.__vpsnetHosts ??= createHostResolver());
}
