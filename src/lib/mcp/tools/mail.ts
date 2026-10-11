import "server-only";
import { agent } from "@/lib/agent";
import { optionalNumber, optionalString, requireString, type Tool } from "../types";

export const TOOLS: Tool[] = [
  {
    page: "Mailboxes",
    name: "mail.list_domains",
    description: "List mail domains (one row per domain currently routed through postfix + dovecot).",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    call: async () => agent.mail.listDomains(),
  },
  {
    page: "Mailboxes",
    name: "mail.list_mailboxes",
    description: "List every mailbox across all mail domains, with quota in MB. Also returns the server's primary mail hostname.",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    call: async () => agent.mail.listMailboxes(),
  },
  {
    page: "Mailboxes",
    name: "mail.mailbox_usage",
    description: "Disk usage for a single mailbox (du -sb /var/vmail/<domain>/<local>). Returns usage_bytes and usage_mb.",
    inputSchema: {
      type: "object",
      properties: { email: { type: "string", description: "Full email address." } },
      required: ["email"],
      additionalProperties: false,
    },
    call: async (args) => agent.mail.mailboxUsage(requireString(args, "email")),
  },
  {
    page: "Mailboxes",
    name: "mail.create_domain",
    description:
      "Register a mail domain. Generates a DKIM keypair, publishes MX/SPF/DMARC + A/AAAA for mail.<domain>, writes an ACME helper vhost, and orders a cert covering mail.<domain> (ssl_task_id in the response) so IMAP/SMTP clients connecting to mail.<domain> get a matching certificate.",
    inputSchema: {
      type: "object",
      properties: { name: { type: "string", description: "Apex domain to enable mail for (e.g. example.com)." } },
      required: ["name"],
      additionalProperties: false,
    },
    call: async (args) => agent.mail.createDomain(requireString(args, "name")),
  },
  {
    page: "Mailboxes",
    name: "mail.ensure_ssl",
    description:
      "Make sure a valid Let's Encrypt cert covers mail.<name> and Postfix/Dovecot present it: re-issues the tenant's site cert with the mail SAN, or orders a standalone mail.<name> cert, then rewires the SNI maps. Returns {task_id, already_covered}; task_id is null when nothing had to be ordered. Use this when a mail client reports an SSL/certificate error for mail.<name>.",
    inputSchema: {
      type: "object",
      properties: { name: { type: "string", description: "Mail domain (apex, e.g. example.com) - the cert is for mail.<name>." } },
      required: ["name"],
      additionalProperties: false,
    },
    call: async (args) => agent.mail.ensureSsl(requireString(args, "name")),
  },
  {
    page: "Mailboxes",
    name: "mail.create_mailbox",
    description:
      "Create a mailbox. If the domain isn't already a mail domain it's auto-registered first. Fails with a conflict if the address already exists (use mail.update_mailbox to change an existing mailbox's password or quota). Password is generated when omitted and returned in initial_password — copy it immediately, it's not stored in plaintext anywhere afterwards.",
    inputSchema: {
      type: "object",
      properties: {
        email: { type: "string", description: "Full email address (local@domain)." },
        password: {
          type: "string",
          description: "Optional password. Generated when omitted. Min 10 chars when supplied.",
          sensitive: true,
        },
        quota_mb: {
          type: "integer",
          description: "Mailbox quota in MB. Default 1024.",
          minimum: 1,
          default: 1024,
        },
      },
      required: ["email"],
      additionalProperties: false,
    },
    call: async (args) =>
      agent.mail.createMailbox({
        email: requireString(args, "email"),
        password: optionalString(args, "password"),
        quota_mb: optionalNumber(args, "quota_mb"),
      }),
  },
  {
    page: "Mailboxes",
    name: "mail.update_mailbox",
    description:
      "Reset a mailbox password and/or change its quota. Both fields optional but at least one must be supplied. New quota must be >= current usage + 1 MB.",
    inputSchema: {
      type: "object",
      properties: {
        email: { type: "string", description: "Mailbox to update." },
        password: { type: "string", description: "New password (min 10 chars). Omit to leave unchanged.", sensitive: true },
        quota_mb: { type: "integer", description: "New quota in MB. Omit to leave unchanged.", minimum: 1 },
      },
      required: ["email"],
      additionalProperties: false,
    },
    call: async (args) => {
      const email = requireString(args, "email");
      return agent.mail.updateMailbox(email, {
        password: optionalString(args, "password"),
        quota_mb: optionalNumber(args, "quota_mb"),
      });
    },
  },
  {
    page: "Mailboxes",
    name: "mail.list_aliases",
    description:
      "List forwarders, one row per (source, destination) with its id. A source of @domain is that domain's catch-all.",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    call: async () => agent.mail.listAliases(),
  },
  {
    page: "Mailboxes",
    name: "mail.create_alias",
    description:
      "Add a forwarder: mail to `source` also goes to each destination. `source` is an address on a mail domain of this server, or @domain for a catch-all (any address there without a mailbox or forwarder). A forwarder on a mailbox's own address keeps a copy in the mailbox. Returns how many destinations were added.",
    inputSchema: {
      type: "object",
      properties: {
        source: { type: "string", description: "Address to forward (e.g. sales@example.com), or @example.com for the catch-all." },
        destinations: { type: "array", items: { type: "string" }, description: "Addresses to forward to (1-20)." },
      },
      required: ["source", "destinations"],
      additionalProperties: false,
    },
    call: async (args) => {
      const destinations = Array.isArray(args.destinations)
        ? args.destinations.filter((d): d is string => typeof d === "string")
        : [];
      return agent.mail.createAlias(requireString(args, "source"), destinations);
    },
  },
  {
    page: "Mailboxes",
    name: "mail.track_delivery",
    description:
      "What the mail server did with mail to or from one address over the last 1-14 days, from Postfix's log, newest first: one row per recipient with status delivered (to a mailbox here), sent (handed to another server), deferred (retrying), bounced, expired, rejected or queued, plus the receiving server's answer.",
    inputSchema: {
      type: "object",
      properties: {
        address: { type: "string", description: "Full email address, local or external." },
        days: { type: "integer", description: "How far back (1-14).", minimum: 1, maximum: 14, default: 3 },
      },
      required: ["address"],
      additionalProperties: false,
    },
    call: async (args) =>
      agent.mail.trackDelivery(requireString(args, "address"), optionalNumber(args, "days") ?? 3),
  },
];
