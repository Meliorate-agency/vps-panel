import "server-only";
import { pruneByRetention } from "@/lib/housekeeping";
import {
  applyPrivacy,
  eraseSubject,
  loadPrivacyStatus,
  parseRetention,
  privacySettings,
  retentionIsLower,
  savePrivacySettings,
  searchSubject,
  type PrivacyChange,
} from "@/lib/privacy";
import { optionalBoolean, optionalString, requireString, type Tool } from "../types";

// Settings > Privacy over MCP. The same code as the panel's forms
// (lib/privacy.ts), with no panel user as the actor. Messages thrown here go
// to the audit log, so none of them repeats an email address or IP address.

export const TOOLS: Tool[] = [
  {
    page: "Settings",
    name: "privacy.status",
    description:
      "The privacy settings and what really holds on the server. settings: log_ip (full or masked: visitor addresses in the web server's access logs), log_query (keep or strip: query strings in those logs), retention_days (logs with personal data are deleted after this many days; null = no limit) and server_geo (on or off: My Networks looks up server locations at ipinfo.io / get.geojs.io). parts: for the web server's log format, the system journal and the rotated log files, what the settings ask for (wanted), what the system does now (actual), ok, and detail when not ok. agent_error is set when the server could not be asked.",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    call: async () => {
      const status = await loadPrivacyStatus();
      return {
        settings: privacySettings(),
        parts: "error" in status ? [] : status.parts,
        notes: "error" in status ? [] : status.notes,
        agent_error: "error" in status ? status.error : null,
      };
    },
  },
  {
    page: "Settings",
    name: "privacy.set",
    description:
      "Change privacy settings; only the ones given are changed. log_ip masked writes 203.0.113.87 as 203.0.113.0 and 2001:db8:85a3:8d3::7 as 2001:db8:85a3:: in the web server's access logs. log_query strip leaves out everything after the ? in logged URLs and referrers. retention_days deletes web server logs, the system journal (mail, SSH and service logs), the other system logs in /var/log (mail, sign-in, firewall and ban logs), the panel's audit log, its sign-in records and the task history once they are older than that; 0 = no limit. A lower retention than the current one deletes data at once and cannot be undone: it is refused unless confirm_delete is true, which is only for when the operator has decided that. server_geo off stops the server-location lookups of My Networks. Returns the settings and what holds now; applied false with apply_error means the settings are saved but the server could not be brought to them.",
    inputSchema: {
      type: "object",
      properties: {
        log_ip: { type: "string", enum: ["full", "masked"], description: "Visitor addresses in the web server's access logs." },
        log_query: { type: "string", enum: ["keep", "strip"], description: "Query strings in logged URLs and referrers." },
        retention_days: {
          type: "integer",
          description: "Keep logs for this many days (7-3650). 0 = no limit.",
          minimum: 0,
          maximum: 3650,
        },
        server_geo: { type: "string", enum: ["on", "off"], description: "Server-location lookups for My Networks." },
        confirm_delete: {
          type: "boolean",
          description: "Save a retention lower than the current one, which deletes older logs right away.",
          default: false,
        },
      },
      additionalProperties: false,
    },
    call: async (args) => {
      const change: PrivacyChange = {};
      const logIp = optionalString(args, "log_ip");
      const logQuery = optionalString(args, "log_query");
      const serverGeo = optionalString(args, "server_geo");
      if (logIp !== undefined) change.log_ip = logIp;
      if (logQuery !== undefined) change.log_query = logQuery;
      if (serverGeo !== undefined) change.server_geo = serverGeo;
      const rawDays = args.retention_days;
      if (rawDays !== undefined && rawDays !== null) {
        const next = parseRetention(rawDays === 0 ? null : rawDays);
        if ("error" in next) throw new Error(next.error);
        if (
          retentionIsLower(privacySettings().retention_days, next.days) &&
          optionalBoolean(args, "confirm_delete") !== true
        ) {
          throw new Error(
            `retention_days ${next.days} is lower than the current setting: logs older than ${next.days} days would be deleted at once. Pass confirm_delete=true only when the operator has decided that.`,
          );
        }
        change.retention_days = next.days;
      }
      const saved = savePrivacySettings(change, null);
      if (!saved.ok) throw new Error(saved.error);

      const deleted = change.retention_days !== undefined ? pruneByRetention() : null;
      // server_geo is the panel's alone; the other three are the agent's to apply.
      const forAgent =
        change.log_ip !== undefined || change.log_query !== undefined || change.retention_days !== undefined;
      const applied = forAgent ? await applyPrivacy(null) : null;
      return {
        settings: saved.settings,
        applied: applied === null ? null : applied.ok,
        apply_error: applied !== null && !applied.ok ? applied.error : null,
        parts: applied?.ok ? applied.status.parts : [],
        notes: applied?.ok ? applied.status.notes : [],
        deleted_from_panel: deleted
          ? { audit_entries: deleted.audit, sign_in_records: deleted.logins, finished_tasks: deleted.tasks }
          : null,
      };
    },
  },
  {
    page: "Settings",
    name: "privacy.subject_search",
    description:
      "Personal data request: what this server holds about one email address or one IP address. Returns sources (nginx's access and error logs, the system journal, the other text logs in /var/log, fail2ban's bans, mailboxes, forwarders, stored mail, the mail queue, task logs, and the panel's account, sign-in records and audit log), each with a count, up to 20 sample records, whether privacy.subject_erase can act on it (erasable) and a note saying what it is. agent_error is set when only the panel's own records could be searched. Not covered: what the apps keep in their own files and databases, and backups. The result contains personal data. Can take minutes on a server with large logs.",
    inputSchema: {
      type: "object",
      properties: {
        kind: { type: "string", enum: ["email", "ip"], description: "What value is." },
        value: { type: "string", description: "One email address, or one IPv4 or IPv6 address.", sensitive: true },
      },
      required: ["kind", "value"],
      additionalProperties: false,
    },
    call: async (args) => {
      const r = await searchSubject(requireString(args, "kind"), requireString(args, "value"), null);
      if (!r.ok) throw new Error(r.error);
      return r.report;
    },
  },
  {
    page: "Settings",
    name: "privacy.subject_erase",
    description:
      "Erase one email address or IP address from the listed sources (ids from privacy.subject_search, erasable ones only). Log lines and audit entries stay, with the address replaced: an IP by its masked form, an email by [erased]. The panel's sign-in records for it are deleted. Never deletes a mailbox, its mail or a panel account. Cannot be undone: only when the operator has decided it. Returns per source ok, changed (records) and detail.",
    inputSchema: {
      type: "object",
      properties: {
        kind: { type: "string", enum: ["email", "ip"], description: "What value is." },
        value: { type: "string", description: "One email address, or one IPv4 or IPv6 address.", sensitive: true },
        sources: {
          type: "array",
          items: { type: "string" },
          description: "Source ids to erase from, e.g. nginx_access, panel_logins, panel_audit.",
        },
      },
      required: ["kind", "value", "sources"],
      additionalProperties: false,
    },
    call: async (args) => {
      const r = await eraseSubject(
        requireString(args, "kind"),
        requireString(args, "value"),
        Array.isArray(args.sources) ? args.sources : [],
        null,
      );
      if (!r.ok) throw new Error(r.error);
      return { results: r.results };
    },
  },
];
