import "server-only";
import { agent, type SiteMode } from "@/lib/agent";
import {
  optionalBoolean,
  optionalNumber,
  optionalString,
  requireString,
  type Tool,
} from "../types";

export const TOOLS: Tool[] = [
  {
    page: "Domains",
    name: "sites.list",
    description:
      "List nginx sites the agent owns. Each entry has a kind: 'site' (panel-managed), 'app' (panel-deployed app's vhost), 'panel-host' (the panel UI vhost), or 'external' (manually-written vhost in /etc/nginx/sites-available).",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    call: async () => agent.sites.list(),
  },
  {
    page: "Domains",
    name: "sites.status",
    description: "SSL certificate status for one site: cert_exists, cert_not_after, days_remaining.",
    inputSchema: {
      type: "object",
      properties: { domain: { type: "string", description: "Site domain (e.g. example.com)." } },
      required: ["domain"],
      additionalProperties: false,
    },
    call: async (args) => agent.sites.status(requireString(args, "domain")),
  },
  {
    page: "Domains",
    name: "sites.create",
    description:
      "Create an nginx vhost routing a domain to either an upstream port (reverse proxy) or a folder on disk (static site). Optionally issue a Let's Encrypt cert. For port mode set upstream_port; for folder mode set folder_path.",
    inputSchema: {
      type: "object",
      properties: {
        domain: { type: "string", description: "Hostname (e.g. app.example.com)." },
        mode: {
          type: "string",
          description: "'port' = reverse proxy to 127.0.0.1:<upstream_port>. 'folder' = serve static files from folder_path.",
          enum: ["port", "folder"],
        },
        upstream_port: {
          type: "integer",
          description: "For mode='port', the loopback port to proxy to. Required when mode='port'.",
          minimum: 1,
          maximum: 65535,
        },
        folder_path: {
          type: "string",
          description: "For mode='folder', the directory to serve. Required when mode='folder'.",
        },
        force_www_redirect: {
          type: "boolean",
          description: "If true, also write a www.<domain> vhost that 301-redirects to <domain>.",
          default: false,
        },
        issue_ssl: {
          type: "boolean",
          description: "Issue a Let's Encrypt cert via HTTP-01 after the vhost is up. Requires public DNS to already point at this host.",
          default: true,
        },
      },
      required: ["domain", "mode"],
      additionalProperties: false,
    },
    call: async (args) =>
      agent.sites.create({
        domain: requireString(args, "domain"),
        mode: requireString(args, "mode") as SiteMode,
        upstream_port: optionalNumber(args, "upstream_port") ?? null,
        folder_path: optionalString(args, "folder_path") ?? null,
        force_www_redirect: optionalBoolean(args, "force_www_redirect") ?? false,
        issue_ssl: optionalBoolean(args, "issue_ssl") ?? true,
      }),
  },
  {
    page: "Domains",
    name: "sites.update",
    description: "Modify an existing site's routing mode/target and optionally re-issue SSL.",
    inputSchema: {
      type: "object",
      properties: {
        domain: { type: "string", description: "Site to update." },
        mode: { type: "string", enum: ["port", "folder"] },
        upstream_port: { type: "integer", minimum: 1, maximum: 65535 },
        folder_path: { type: "string" },
        force_www_redirect: { type: "boolean", default: false },
        issue_ssl: { type: "boolean", default: true },
      },
      required: ["domain", "mode"],
      additionalProperties: false,
    },
    call: async (args) =>
      agent.sites.update(requireString(args, "domain"), {
        mode: requireString(args, "mode") as SiteMode,
        upstream_port: optionalNumber(args, "upstream_port") ?? null,
        folder_path: optionalString(args, "folder_path") ?? null,
        force_www_redirect: optionalBoolean(args, "force_www_redirect") ?? false,
        issue_ssl: optionalBoolean(args, "issue_ssl") ?? true,
      }),
  },
];
