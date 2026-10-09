import "server-only";
import type { Tool } from "./types";
import { TOOLS as DASHBOARD } from "./tools/dashboard";
import { TOOLS as APPS } from "./tools/apps";
import { TOOLS as MAIL } from "./tools/mail";
import { TOOLS as DATABASES } from "./tools/databases";
import { TOOLS as DOMAINS } from "./tools/domains";
import { TOOLS as SITES } from "./tools/sites";
import { TOOLS as NETWORKS } from "./tools/networks";
import { TOOLS as SSL } from "./tools/ssl";
import { TOOLS as SERVICES } from "./tools/services";
import { TOOLS as FIREWALL } from "./tools/firewall";
import { TOOLS as CRON } from "./tools/cron";
import { TOOLS as BACKUPS } from "./tools/backups";
import { TOOLS as SSH } from "./tools/ssh";
import { TOOLS as TASKS } from "./tools/tasks";

/**
 * Tools a read-only token may call: they read state and return no secrets.
 * Anything not listed needs a full-access token, so a new tool is
 * write-level until someone decides otherwise. Left out on purpose:
 * apps.list_env_files (returns .env contents) and tasks.get (task logs can
 * hold build output and env values).
 */
const READ_ONLY_TOOLS = new Set([
  "sysinfo",
  "health",
  "apps.list",
  "apps.get",
  "apps.git_status",
  "apps.source_state",
  "db.list_users",
  "db.list_databases",
  "dns.list_zones",
  "dns.list_records",
  "sites.list",
  "sites.status",
  "networks.status",
  "ssl.list",
  "mail.list_domains",
  "mail.list_mailboxes",
  "mail.mailbox_usage",
  "services.list",
  "system.services",
  "firewall.status",
  "firewall.jails",
  "firewall.failed_by_ip",
  "cron.list",
  "backups.list",
  "ssh.list_keys",
  "ssh.get_server_config",
  "tasks.list",
]);

/**
 * Order matters for the /ai-agent preview UI — sections render in this
 * order. Matches sidebar order so the page-by-page browse is intuitive.
 */
const ALL: Tool[] = [
  ...DASHBOARD,
  ...APPS,
  ...DOMAINS,
  ...SITES,
  ...NETWORKS,
  ...SSL,
  ...MAIL,
  ...DATABASES,
  ...SERVICES,
  ...FIREWALL,
  ...CRON,
  ...BACKUPS,
  ...SSH,
  ...TASKS,
];

// Sanity-check at module load: every tool name must be unique. A duplicate
// would silently shadow the earlier registration in the lookup map below.
const seen = new Set<string>();
for (const t of ALL) {
  if (seen.has(t.name)) {
    throw new Error(`MCP tool registry: duplicate tool name "${t.name}"`);
  }
  seen.add(t.name);
}

for (const t of ALL) t.readOnly = READ_ONLY_TOOLS.has(t.name);
for (const name of READ_ONLY_TOOLS) {
  if (!seen.has(name)) throw new Error(`MCP tool registry: READ_ONLY_TOOLS names unknown tool "${name}"`);
}

const BY_NAME = new Map<string, Tool>(ALL.map((t) => [t.name, t]));

/** Every registered tool, in display order (grouped by page in section
 *  order matching the sidebar). */
export function listTools(): Tool[] {
  return ALL;
}

/** Group tools by page in the registry's natural order. The first time a
 *  page appears wins its display slot — useful for the /ai-agent UI's
 *  per-page <details> sections. */
export function listToolsByPage(): { page: string; tools: Tool[] }[] {
  const groups = new Map<string, Tool[]>();
  for (const t of ALL) {
    let bucket = groups.get(t.page);
    if (!bucket) {
      bucket = [];
      groups.set(t.page, bucket);
    }
    bucket.push(t);
  }
  return Array.from(groups, ([page, tools]) => ({ page, tools }));
}

/** Look up a tool by its dotted JSON-RPC name. Returns null when missing
 *  — the route handler turns that into a JSON-RPC -32601 (method not
 *  found) error per spec. */
export function getTool(name: string): Tool | null {
  return BY_NAME.get(name) ?? null;
}
