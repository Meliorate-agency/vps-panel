import "server-only";
import { agent } from "@/lib/agent";
import { optionalString, requireNumber, requireString, type Tool } from "../types";

export const TOOLS: Tool[] = [
  {
    page: "Firewall & bans",
    name: "firewall.status",
    description:
      "UFW + fail2ban status snapshot: ufw_enabled flag, rules list, listening_ports (proto/address/port/process), fail2ban_active.",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    call: async () => agent.firewall.status(),
  },
  {
    page: "Firewall & bans",
    name: "firewall.jails",
    description: "List fail2ban jails with the IPs they have banned and total failed-login counters.",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    call: async () => agent.firewall.jails(),
  },
  {
    page: "Firewall & bans",
    name: "firewall.failed_by_ip",
    description:
      "Per-IP failed-login counts for a jail, parsed from /var/log/fail2ban.log. Returns up to 100 entries sorted by attempt count desc.",
    inputSchema: {
      type: "object",
      properties: { jail: { type: "string", description: "Jail name (e.g. sshd, dovecot)." } },
      required: ["jail"],
      additionalProperties: false,
    },
    call: async (args) => agent.firewall.failedByIp(requireString(args, "jail")),
  },
  {
    page: "Firewall & bans",
    name: "firewall.allow",
    description: "Open a port in UFW (default proto tcp). Reversible via firewall.deny.",
    inputSchema: {
      type: "object",
      properties: {
        port: { type: "integer", description: "Port to allow.", minimum: 1, maximum: 65535 },
        proto: { type: "string", description: "Protocol. Default 'tcp'.", enum: ["tcp", "udp"], default: "tcp" },
      },
      required: ["port"],
      additionalProperties: false,
    },
    call: async (args) =>
      agent.firewall.allow(requireNumber(args, "port"), optionalString(args, "proto") ?? "tcp"),
  },
  {
    page: "Firewall & bans",
    name: "firewall.deny",
    description: "Close a previously-allowed port in UFW.",
    inputSchema: {
      type: "object",
      properties: {
        port: { type: "integer", description: "Port to deny.", minimum: 1, maximum: 65535 },
        proto: { type: "string", description: "Protocol. Default 'tcp'.", enum: ["tcp", "udp"], default: "tcp" },
      },
      required: ["port"],
      additionalProperties: false,
    },
    call: async (args) =>
      agent.firewall.deny(requireNumber(args, "port"), optionalString(args, "proto") ?? "tcp"),
  },
  {
    page: "Firewall & bans",
    name: "firewall.unban",
    description: "Remove a single IP from a fail2ban jail (lets that IP retry SSH/mail login).",
    inputSchema: {
      type: "object",
      properties: {
        jail: { type: "string", description: "Jail name." },
        ip: { type: "string", description: "IPv4 or IPv6 to unban." },
      },
      required: ["jail", "ip"],
      additionalProperties: false,
    },
    call: async (args) =>
      agent.firewall.unban(requireString(args, "jail"), requireString(args, "ip")),
  },
];
