import "server-only";
import { agent } from "@/lib/agent";
import { requireString, type Tool } from "../types";

export const TOOLS: Tool[] = [
  {
    page: "Domains",
    name: "ssl.list",
    description: "List all Let's Encrypt certs the panel has issued with expiry + days_remaining.",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    call: async () => agent.ssl.list(),
  },
  {
    page: "Domains",
    name: "ssl.renew",
    description:
      "Re-issue the certificate for a single domain with its full SAN set (www when forced, mail.<domain> when it is a mail tenant). Returns {task_id}; follow it with tasks.get. Reversible - at worst it consumes a Let's Encrypt rate-limit slot.",
    inputSchema: {
      type: "object",
      properties: { domain: { type: "string", description: "Cert domain to renew." } },
      required: ["domain"],
      additionalProperties: false,
    },
    call: async (args) => agent.ssl.renew(requireString(args, "domain")),
  },
];
