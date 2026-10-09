import "server-only";
import { agent } from "@/lib/agent";
import type { Tool } from "../types";

export const TOOLS: Tool[] = [
  {
    page: "Backups",
    name: "backups.list",
    description: "List restic snapshots with id, timestamp, included paths, and hostname.",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    call: async () => agent.backups.list(),
  },
  {
    page: "Backups",
    name: "backups.run",
    description:
      "Trigger the standard full restic backup (the panel's default path set). Returns a task_id; stream the log via tasks.get.",
    // SECURITY: the `paths` argument is intentionally NOT accepted over MCP.
    // Arbitrary paths let a caller back up /etc/shadow, /root/.ssh, the agent
    // token, and TLS keys into the restic repo for later exfiltration. The MCP
    // backup always runs the server-side default path set; choosing custom
    // paths stays a panel-session, admin-only operation.
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    call: async () => agent.backups.run(),
  },
];
