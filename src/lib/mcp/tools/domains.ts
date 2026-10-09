import "server-only";
import { agent } from "@/lib/agent";
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
    description: "List PowerDNS zones the agent knows about (id, name, type, record count).",
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
      "Create a new PowerDNS zone with the panel's standard seed records: SOA, NS ns1/ns2, A @/www/mail/ns1/ns2, CAA letsencrypt.org. Optional seed_ip overrides the resolved server IP (useful when DNS hasn't propagated yet).",
    inputSchema: {
      type: "object",
      properties: {
        name: { type: "string", description: "Zone name to create (e.g. newdomain.com)." },
        seed_ip: { type: "string", description: "Optional IPv4 to seed A records with. Falls back to PANEL_HOST resolution." },
      },
      required: ["name"],
      additionalProperties: false,
    },
    call: async (args) =>
      agent.dns.createZone(requireString(args, "name"), optionalString(args, "seed_ip")),
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
];
