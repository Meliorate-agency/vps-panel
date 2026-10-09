import "server-only";
import { agent } from "../agent";
import type { PeerStats } from "./protocol";

type Cache = { at: number; stats: Promise<PeerStats | null> };

/**
 * What this server reports about itself in every heartbeat, from the
 * agent's /sysinfo. Cached for a few seconds: every ping in and out carries
 * it. Null while the agent does not answer; the pings still go.
 */
export function ownStats(): Promise<PeerStats | null> {
  // On globalThis: the heartbeat loop and the peer routes can be separate
  // module instances.
  const g = globalThis as { __vpsnetStats?: Cache };
  const c = g.__vpsnetStats;
  if (c && Date.now() - c.at < 8_000) return c.stats;
  const stats = Promise.race([
    read(),
    new Promise<null>((resolve) => setTimeout(() => resolve(null), 3_000).unref()),
  ]);
  g.__vpsnetStats = { at: Date.now(), stats };
  return stats;
}

async function read(): Promise<PeerStats | null> {
  try {
    const s = await agent.sysinfo();
    const root = s.disk.find((d) => d.mount === "/") ?? s.disk[0];
    return {
      uptime: s.uptime_secs,
      load1: s.load_1,
      cpus: s.cpu_count,
      mem: s.mem_total_kb > 0 ? Math.round((1 - s.mem_available_kb / s.mem_total_kb) * 100) : 0,
      disk: root && root.total_bytes > 0 ? Math.round((1 - root.free_bytes / root.total_bytes) * 100) : 0,
    };
  } catch {
    return null;
  }
}
