import "server-only";
import { agent } from "@/lib/agent";
import { requireString, type Tool } from "../types";

/**
 * SSH tools are READ-ONLY in v1. Adding / removing public keys, writing
 * authorized_keys, generating keypairs, and reading private keys are all
 * intentionally excluded — those are credential-issuance operations that
 * shouldn't be delegated to an AI without an explicit second-factor flow
 * we don't have yet.
 */
export const TOOLS: Tool[] = [
  {
    page: "SSH keys",
    name: "ssh.list_keys",
    description:
      "List the public keys in a Linux user's authorized_keys (authorized: true), then the keypairs in their ~/.ssh that are not login keys (authorized: false).",
    inputSchema: {
      type: "object",
      properties: {
        user: { type: "string", description: "Linux user (e.g. root, apps)." },
      },
      required: ["user"],
      additionalProperties: false,
    },
    call: async (args) => agent.ssh.list(requireString(args, "user")),
  },
  {
    page: "SSH keys",
    name: "ssh.get_server_config",
    description:
      "Read the effective sshd settings (sshd -T, drop-ins included): port, idle_timeout_minutes (0 = never), password_auth, permit_root_login (yes | prohibit-password | forced-commands-only | no).",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    call: async () => agent.ssh.getConfig(),
  },
];
