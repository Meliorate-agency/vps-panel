import "server-only";
import { networksView } from "@/lib/networks/store";
import type { Tool } from "../types";

export const TOOLS: Tool[] = [
  {
    page: "My Networks",
    name: "networks.status",
    description:
      "List the server networks this panel is in (My Networks) with every member's status (online / late / offline / waiting / self), address, key fingerprint, last seen time, latency, last error and reported load, memory %, disk % and uptime. Members ping each other every 10 s.",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    call: async () =>
      networksView().map((net) => ({
        id: net.id,
        name: net.name,
        owned_by_this_server: net.owned,
        owner: net.ownerName,
        members: net.members.map((m) => ({
          name: m.name,
          url: m.url,
          fingerprint: m.fingerprint,
          this_server: m.self,
          owner: m.owner,
          status: m.status,
          last_seen: m.lastSeen ? new Date(m.lastSeen).toISOString() : null,
          latency_ms: m.rttMs,
          error: m.error,
          stats: m.stats,
        })),
      })),
  },
];
