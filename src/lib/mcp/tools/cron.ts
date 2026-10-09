import "server-only";
import { agent } from "@/lib/agent";
import { type Tool } from "../types";

// SECURITY: cron.create is intentionally NOT exposed over MCP. It writes an
// arbitrary command into /etc/cron.d that cron runs as the chosen Linux user
// (root by default) — i.e. it is root-RCE by design. The MCP bearer token is a
// single, passwordless, internet-reachable credential with no second factor, so
// exposing cron.create there makes the token root-equivalent. Scheduling a job
// stays a panel-session, admin-only operation (see createCronAction, which now
// calls requireAdmin). Mirrors the deliberate read-only exclusion already
// applied to the SSH write tools.
export const TOOLS: Tool[] = [
  {
    page: "Cron jobs",
    name: "cron.list",
    description: "List cron jobs the panel manages (written into /etc/cron.d/vps-panel-*).",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    call: async () => agent.cron.list(),
  },
];
