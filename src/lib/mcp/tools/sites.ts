import "server-only";
import { agent, type CreateSite, type CreateSiteResponse } from "@/lib/agent";
import { db } from "@/lib/db";
import { pollSslTaskAndReconcile } from "@/lib/sslReconciler";
import {
  optionalBoolean,
  optionalNumber,
  optionalString,
  requireString,
  type Tool,
} from "../types";

function routeArgs(args: Record<string, unknown>): Omit<CreateSite, "domain"> {
  return {
    mode: requireString(args, "mode") as CreateSite["mode"],
    upstream_port: optionalNumber(args, "upstream_port") ?? null,
    folder_path: optionalString(args, "folder_path") ?? null,
    force_www_redirect: optionalBoolean(args, "force_www_redirect") ?? false,
    issue_ssl: optionalBoolean(args, "issue_ssl") ?? true,
  };
}

/**
 * The panel.db row for a site the agent just created or changed, written as
 * the /domains forms write it. Without it a site made over MCP showed on
 * /domains as a vhost the panel does not manage, and apps.set_domain refused
 * it as a routing slot; a changed port left the slot pickers on the old one.
 */
function recordSite(domain: string, r: Omit<CreateSite, "domain">, resp: CreateSiteResponse): void {
  db()
    .prepare(
      `INSERT INTO sites (domain, mode, upstream_port, folder_path, force_www_redirect, ssl_requested)
       VALUES (?, ?, ?, ?, ?, ?)
       ON CONFLICT(domain) DO UPDATE SET
         mode = excluded.mode, upstream_port = excluded.upstream_port,
         folder_path = excluded.folder_path, force_www_redirect = excluded.force_www_redirect,
         ssl_requested = excluded.ssl_requested`,
    )
    .run(
      domain,
      r.mode,
      r.mode === "port" ? r.upstream_port : null,
      r.mode === "folder" ? r.folder_path : null,
      r.force_www_redirect ? 1 : 0,
      r.issue_ssl ? 1 : 0,
    );
  if (resp.task_id) {
    db().prepare(`UPDATE sites SET last_ssl_error = NULL WHERE domain = ?`).run(domain);
    pollSslTaskAndReconcile(domain, resp.task_id, null);
  }
}

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
    call: async (args) => {
      const domain = requireString(args, "domain");
      const route = routeArgs(args);
      const resp = await agent.sites.create({ domain, ...route });
      recordSite(domain, route, resp);
      return resp;
    },
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
    call: async (args) => {
      const domain = requireString(args, "domain");
      const route = routeArgs(args);
      // As on /domains: the bound app's unit carries this port, so the slot
      // keeps it until the app is moved.
      const bound = db().prepare(`SELECT name, port FROM apps WHERE subdomain = ?`).get(domain) as
        | { name: string; port: number }
        | undefined;
      if (bound && (route.mode !== "port" || route.upstream_port !== bound.port)) {
        throw new Error(
          `${domain} is bound to app "${bound.name}" on port ${bound.port} - move the app with apps.set_domain first, or keep the port`,
        );
      }
      const resp = await agent.sites.update(domain, route);
      recordSite(domain, route, resp);
      return resp;
    },
  },
];
