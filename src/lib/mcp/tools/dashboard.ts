import "server-only";
import { agent } from "@/lib/agent";
import type { Tool } from "../types";

/**
 * Read-only system-health probes. Safe to call any time. Map roughly to
 * the cards on the /dashboard page.
 */
export const TOOLS: Tool[] = [
  {
    page: "Dashboard",
    name: "sysinfo",
    description:
      "Return current system metrics for the panel host: uptime, CPU count, 1/5/15-min load averages, memory total/available, swap, and per-disk + per-network-interface counters.",
    inputSchema: {
      type: "object",
      properties: {},
      additionalProperties: false,
    },
    call: async () => agent.sysinfo(),
  },
  {
    page: "Dashboard",
    name: "health",
    description:
      "Liveness probe of the Rust agent (the privileged subprocess the panel talks to over a unix socket). Returns the agent's version + current time. Use as a sanity check before fan-out.",
    inputSchema: {
      type: "object",
      properties: {},
      additionalProperties: false,
    },
    call: async () => agent.health(),
  },
];
