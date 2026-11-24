import "server-only";
import { agent } from "@/lib/agent";
import { db } from "@/lib/db";
import {
  optionalBoolean,
  optionalNumber,
  optionalString,
  requireNumber,
  requireString,
  type Tool,
} from "../types";

export const TOOLS: Tool[] = [
  {
    page: "Domains",
    name: "dns.list_zones",
    description:
      "List PowerDNS zones the agent knows about (id, name, type, record count, ipv6: whether IPv6 is on for the zone).",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    call: async () => agent.dns.listZones(),
  },
  {
    page: "Domains",
    name: "dns.list_records",
    description: "List every DNS record in a zone.",
    inputSchema: {
      type: "object",
      properties: { zone: { type: "string", description: "Zone name (e.g. example.com)." } },
      required: ["zone"],
      additionalProperties: false,
    },
    call: async (args) => agent.dns.listRecords(requireString(args, "zone")),
  },
  {
    page: "Domains",
    name: "dns.create_zone",
    description:
      "Create a new PowerDNS zone with the panel's standard seed records: SOA, NS ns1/ns2, A @/www/mail/ns1/ns2, CAA letsencrypt.org, and with IPv6 on also AAAA records for the server's IPv6 address. Optional seed_ip overrides the resolved server IP (useful when DNS hasn't propagated yet). Optional ipv6 switches IPv6 on or off for the new zone; left out, the server default for new domains applies (see ipv6.status).",
    inputSchema: {
      type: "object",
      properties: {
        name: { type: "string", description: "Zone name to create (e.g. newdomain.com)." },
        seed_ip: { type: "string", description: "Optional IPv4 to seed A records with. Falls back to PANEL_HOST resolution." },
        ipv6: {
          type: "boolean",
          description: "Publish IPv6 records for the new zone. Omit to use the server default for new domains.",
        },
      },
      required: ["name"],
      additionalProperties: false,
    },
    call: async (args) => {
      const zone = await agent.dns.createZone(requireString(args, "name"), optionalString(args, "seed_ip"), {
        ipv6: optionalBoolean(args, "ipv6"),
      });
      // Listed as one of the panel's zones, as when it is added on /domains
      // (it showed there as a foreign zone to adopt, and Add mailbox did not
      // offer it).
      db().prepare(`INSERT OR IGNORE INTO domains (name) VALUES (?)`).run(zone.name);
      return zone;
    },
  },
  {
    page: "Domains",
    name: "dns.create_record",
    description: "Add a DNS record to a zone (A, AAAA, CNAME, MX, TXT, CAA, etc.).",
    inputSchema: {
      type: "object",
      properties: {
        zone: { type: "string", description: "Zone the record belongs to." },
        name: {
          type: "string",
          description: "Record name. Use '@' or empty string for the apex. Bare label becomes <label>.<zone>.",
        },
        type: {
          type: "string",
          description: "Record type.",
          enum: ["A", "AAAA", "CNAME", "MX", "TXT", "CAA", "SRV", "NS", "PTR"],
        },
        content: {
          type: "string",
          description: "Record content: IP for A/AAAA, target hostname for CNAME and MX (just the hostname; put the MX priority in prio, not here), quoted text for TXT. A CNAME is refused at the zone apex or next to other records at the same name.",
        },
        ttl: { type: "integer", description: "TTL in seconds. Default 3600.", minimum: 60, default: 3600 },
        prio: { type: "integer", description: "Priority (for MX/SRV records).", minimum: 0 },
      },
      required: ["zone", "name", "type", "content"],
      additionalProperties: false,
    },
    call: async (args) =>
      agent.dns.createRecord(requireString(args, "zone"), {
        name: requireString(args, "name"),
        type: requireString(args, "type"),
        content: requireString(args, "content"),
        ttl: optionalNumber(args, "ttl"),
        prio: optionalNumber(args, "prio"),
      }),
  },
  {
    page: "Domains",
    name: "dns.update_record",
    description: "Edit an existing DNS record by id (from dns.list_records).",
    inputSchema: {
      type: "object",
      properties: {
        id: { type: "integer", description: "Record id from dns.list_records." },
        content: { type: "string", description: "New content. Omit to keep." },
        ttl: { type: "integer", description: "New TTL. Omit to keep.", minimum: 60 },
        prio: { type: "integer", description: "New priority. Omit to keep.", minimum: 0 },
        name: { type: "string", description: "New name. Omit to keep." },
        disabled: { type: "boolean", description: "Disable the record without deleting it." },
      },
      required: ["id"],
      additionalProperties: false,
    },
    call: async (args) =>
      agent.dns.updateRecord(requireNumber(args, "id"), {
        content: optionalString(args, "content"),
        ttl: optionalNumber(args, "ttl"),
        prio: optionalNumber(args, "prio"),
        name: optionalString(args, "name"),
        disabled: optionalBoolean(args, "disabled"),
      }),
  },
  {
    page: "Domains",
    name: "domains.ipv6",
    description:
      "One zone's IPv6: enabled (on or off), the server's IPv6 address in effect (address; null = none), can_enable (false only when the server has no IPv6 address), records (what on means for this zone: the AAAA records and the SPF TXT record with ip6:, each with present = in the zone now), checks (will IPv6 work for this domain: address on an interface, the web server listens on IPv6, the name servers answer over IPv6, reverse DNS for a mail domain; each ok / warn / fail with a plain explanation) and notes. Slow: live DNS lookups, several seconds.",
    inputSchema: {
      type: "object",
      properties: { name: { type: "string", description: "Zone name (e.g. example.com)." } },
      required: ["name"],
      additionalProperties: false,
    },
    call: async (args) => agent.dns.zoneIpv6(requireString(args, "name")),
  },
  {
    page: "Domains",
    name: "domains.set_ipv6",
    description:
      "Switch IPv6 on or off for one zone. On publishes AAAA records for the apex, www, mail, the in-zone name servers and the zone's sites on this server, and adds ip6: to the SPF record; off removes the ones that point at this server (AAAA records pointing elsewhere stay). Warnings from the checks do not block switching on: read them with domains.ipv6 first and tell the operator. Refused with a conflict only when the server has no IPv6 address. Returns the zone's IPv6 state afterwards.",
    inputSchema: {
      type: "object",
      properties: {
        name: { type: "string", description: "Zone name (e.g. example.com)." },
        enabled: { type: "boolean", description: "true = on, false = off." },
      },
      required: ["name", "enabled"],
      additionalProperties: false,
    },
    call: async (args) => {
      const enabled = optionalBoolean(args, "enabled");
      if (enabled === undefined) throw new Error("missing required boolean argument: enabled");
      return agent.dns.setZoneIpv6(requireString(args, "name"), enabled);
    },
  },
];
