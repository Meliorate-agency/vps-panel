import "server-only";
import { agent } from "@/lib/agent";
import type { Tool } from "../types";

/**
 * Services are READ-ONLY in v1. Restart/stop are intentionally NOT exposed
 * — stopping `vps-panel.service` or `nginx.service` over MCP would take
 * the panel offline and lock the AI out of recovering from its own
 * mistake. Once we have per-tool unit allowlists this can be reopened
 * for non-critical units (e.g. quick-install Docker containers).
 */
export const TOOLS: Tool[] = [
  {
    page: "System services",
    name: "services.list",
    description: "Every systemd unit known to the agent (output of `systemctl list-units --type=service --all`).",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    call: async () => agent.services.list(),
  },
  {
    page: "System services",
    name: "system.services",
    description:
      "Core service health for the first-run wizard: nginx, mariadb, postfix, dovecot, pdns, rspamd, fail2ban, vps-panel-agent, vps-panel. Reports installed/active/enabled per unit + the quick-install slug to repair it.",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    call: async () => agent.system.services(),
  },
];
