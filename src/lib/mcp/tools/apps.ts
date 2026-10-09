import "server-only";
import { agent, type CreateApp } from "@/lib/agent";
import { applyAppRouting } from "@/lib/appRouting";
import { recordAppDeploy, slotConflict } from "@/lib/appRows";
import {
  optionalBoolean,
  optionalString,
  requireNumber,
  requireString,
  type Tool,
} from "../types";

// Hard-coded mirror of agent's APPS_BASE_DIR. We construct source_dir
// from the app name here so the AI doesn't have to know the convention.
const APPS_BASE_DIR = "/srv/apps";

export const TOOLS: Tool[] = [
  {
    page: "Apps",
    name: "apps.list",
    description:
      "List every deployed Node.js app. Returns one row per app with name, subdomain, upstream port, source folder, systemd active/sub-state, and source-folder state ('missing' / 'empty' / 'populated'). Includes drift-detected external apps (folders under /srv/apps not created by the panel).",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    call: async () => agent.apps.list(),
  },
  {
    page: "Apps",
    name: "apps.get",
    description: "Fetch full details for one app by name.",
    inputSchema: {
      type: "object",
      properties: {
        name: { type: "string", description: "App name (matches /srv/apps/<name>)." },
      },
      required: ["name"],
      additionalProperties: false,
    },
    call: async (args) => agent.apps.get(requireString(args, "name")),
  },
  {
    page: "Apps",
    name: "apps.git_status",
    description:
      "For an app whose source folder is a git checkout, report whether the local HEAD is ahead/behind the upstream. Cheap — runs `git fetch` then compares SHAs.",
    inputSchema: {
      type: "object",
      properties: { name: { type: "string", description: "App name." } },
      required: ["name"],
      additionalProperties: false,
    },
    call: async (args) => agent.apps.gitStatus(requireString(args, "name")),
  },
  {
    page: "Apps",
    name: "apps.source_state",
    description:
      "Check whether /srv/apps/<name>/ exists and contains code. Returns 'missing' | 'empty' | 'populated'. Useful to confirm a deploy is safe to run.",
    inputSchema: {
      type: "object",
      properties: { name: { type: "string", description: "App name." } },
      required: ["name"],
      additionalProperties: false,
    },
    call: async (args) => agent.apps.sourceState(requireString(args, "name")),
  },
  {
    page: "Apps",
    name: "apps.create",
    description:
      "Deploy a new Node app or re-run a stage on an existing one. Optionally clones a git repo into /srv/apps/<name>, runs npm ci + npm run build, writes the systemd unit (vps-app-<name>.service), and starts it. Returns a task_id whose log can be streamed via tasks.get.",
    inputSchema: {
      type: "object",
      properties: {
        name: {
          type: "string",
          description: "App identifier (lowercase, digits, hyphens; 2-32 chars). Becomes the folder name under /srv/apps/ and the systemd unit suffix.",
        },
        subdomain: {
          type: "string",
          description: "Fully-qualified hostname the panel should route to this app (e.g. myapp.example.com). Must already exist as a port-mode site under /domains.",
        },
        port: {
          type: "integer",
          description: "Loopback port the app should bind to (PORT env var). Must match the upstream_port of the chosen port-mode site.",
          minimum: 1024,
          maximum: 65535,
        },
        runtime: {
          type: "string",
          description: "Runtime tag. Only 'node' is supported in v1.",
          enum: ["node"],
          default: "node",
        },
        exec_start: {
          type: "string",
          description: "systemd ExecStart command line, run from inside source_dir. Omit to keep the app's current one on a re-deploy (a new app gets /usr/bin/npm start).",
        },
        env: {
          type: "array",
          description: "Environment variables for the systemd unit's EnvironmentFile; they are also exported to npm run build. Replaces the whole file. Omit to keep the current file on a re-deploy (use apps.patch_env to change single keys).",
          items: {
            type: "object",
            properties: {
              key: { type: "string" },
              value: { type: "string" },
            },
            required: ["key", "value"],
          },
        },
        repo_url: {
          type: "string",
          description: "Optional git remote to clone into source_dir before deploying. Use git@<alias>:owner/repo for private repos where <alias> matches an entry in root's ~/.ssh/config.",
        },
        step: {
          type: "string",
          description: "Run only one stage of the pipeline instead of the full deploy. Default 'all' runs pull→install→build→deploy.",
          enum: ["pull", "install", "build", "deploy", "all"],
          default: "all",
        },
        issue_ssl: {
          type: "boolean",
          description: "Issue Let's Encrypt cert for the subdomain if the port-mode site doesn't already own one. Default false (the site usually owns SSL).",
          default: false,
        },
      },
      required: ["name", "subdomain", "port"],
      additionalProperties: false,
    },
    call: async (args) => {
      const name = requireString(args, "name");
      const body: CreateApp = {
        name,
        source_dir: `${APPS_BASE_DIR}/${name}`,
        subdomain: requireString(args, "subdomain"),
        port: requireNumber(args, "port"),
        runtime: optionalString(args, "runtime") ?? "node",
        // Omitted = keep what the app already has (a re-run used to reset a
        // custom entrypoint and wipe the env file).
        exec_start: optionalString(args, "exec_start"),
        env: Array.isArray(args.env)
          ? (args.env as { key: string; value: string }[]).filter(
              (e) => typeof e?.key === "string" && typeof e?.value === "string",
            )
          : undefined,
        issue_ssl: optionalBoolean(args, "issue_ssl") ?? false,
        skip_dns_check: true,
        repo_url: optionalString(args, "repo_url"),
        step: (optionalString(args, "step") as CreateApp["step"]) ?? "all",
      };
      const persistsRow = body.step === "deploy" || body.step === "all";
      if (persistsRow) {
        const conflict = slotConflict(body.name, body.port, body.subdomain);
        if (conflict) throw new Error(conflict);
      }
      const accepted = await agent.apps.create(body);
      if (persistsRow) recordAppDeploy(body, accepted.task_id);
      return accepted;
    },
  },
  {
    page: "Apps",
    name: "apps.start",
    description: "Start a stopped app (systemctl start vps-app-<name>.service). Refused (409) while the app has no domain assigned - use apps.set_domain first.",
    inputSchema: {
      type: "object",
      properties: { name: { type: "string", description: "App name." } },
      required: ["name"],
      additionalProperties: false,
    },
    call: async (args) => agent.apps.start(requireString(args, "name")),
  },
  {
    page: "Apps",
    name: "apps.stop",
    description: "Stop a running app (systemctl stop vps-app-<name>.service). Reversible via apps.start.",
    inputSchema: {
      type: "object",
      properties: { name: { type: "string", description: "App name." } },
      required: ["name"],
      additionalProperties: false,
    },
    call: async (args) => agent.apps.stop(requireString(args, "name")),
  },
  {
    page: "Apps",
    name: "apps.restart",
    description: "Restart an app's systemd unit. Brief interruption of whatever it's serving. Refused (409) while the app has no domain assigned.",
    inputSchema: {
      type: "object",
      properties: { name: { type: "string", description: "App name." } },
      required: ["name"],
      additionalProperties: false,
    },
    call: async (args) => agent.apps.restart(requireString(args, "name")),
  },
  {
    page: "Apps",
    name: "apps.set_domain",
    description:
      "Move an app to another routing slot (a port-mode site under /domains that no other app is bound to) or take it off its slot by omitting domain. Moving rewrites PORT and restarts the app; unassigning stops AND disables it until a slot is assigned again. Returns { state, domain, port, bound, log } - bound=false means the app did not come up on the new port (log holds the journal tail).",
    inputSchema: {
      type: "object",
      properties: {
        name: { type: "string", description: "App name." },
        domain: {
          type: "string",
          description:
            "FQDN of a free port-mode site (see sites.list / the /apps/new picker). Omit to unassign.",
        },
      },
      required: ["name"],
      additionalProperties: false,
    },
    call: async (args) =>
      applyAppRouting(
        null,
        requireString(args, "name"),
        optionalString(args, "domain") ?? null,
      ),
  },
  {
    page: "Apps",
    name: "apps.update",
    description:
      "Pull latest code, reinstall deps (npm ci or install), run build (if defined in package.json), then restart. Returns a task_id; stream the log via tasks.get.",
    inputSchema: {
      type: "object",
      properties: { name: { type: "string", description: "App name." } },
      required: ["name"],
      additionalProperties: false,
    },
    call: async (args) => agent.apps.update(requireString(args, "name")),
  },
  {
    page: "Apps",
    name: "apps.patch_env",
    description:
      "Add or change entries in the app's EnvironmentFile; keys not in the list keep their values. Restarts the app if it has a domain assigned (response.restarted). Keys must match `[A-Za-z_][A-Za-z0-9_]*`. Variables read at build time (NEXT_PUBLIC_*, next.config) only take effect after apps.update rebuilds the app.",
    inputSchema: {
      type: "object",
      properties: {
        name: { type: "string", description: "App name." },
        env: {
          type: "array",
          description: "Key/value pairs to upsert into the env file.",
          items: {
            type: "object",
            properties: {
              key: { type: "string" },
              value: { type: "string" },
            },
            required: ["key", "value"],
          },
        },
      },
      required: ["name", "env"],
      additionalProperties: false,
    },
    call: async (args) => {
      const name = requireString(args, "name");
      const env = Array.isArray(args.env)
        ? (args.env as { key: string; value: string }[]).filter(
            (e) => typeof e?.key === "string" && typeof e?.value === "string",
          )
        : [];
      return agent.apps.patchEnv(name, env);
    },
  },
  {
    page: "Apps",
    name: "apps.list_env_files",
    description:
      "List every file whose name starts with .env inside the app's source folder (/srv/apps/<name>), e.g. .env.local - the files the app's own framework loads at build/run time. Returns each file's relative path, absolute path, size, and content (content omitted for files over 256 KB). Distinct from apps.patch_env, which edits the systemd EnvironmentFile.",
    inputSchema: {
      type: "object",
      properties: { name: { type: "string", description: "App name." } },
      required: ["name"],
      additionalProperties: false,
    },
    call: async (args) => agent.apps.listEnvFiles(requireString(args, "name")),
  },
  {
    page: "Apps",
    name: "apps.save_env_file",
    description:
      "Create or overwrite a .env* file inside the app's source folder, then optionally restart the app. `path` is relative to the source folder (default convention: .env at the root); the file name must start with .env and the path cannot escape the folder. Returns { ok, restarted } where restarted is the systemd unit that was restarted (null when restart was false or no unit exists yet).",
    inputSchema: {
      type: "object",
      properties: {
        name: { type: "string", description: "App name." },
        path: {
          type: "string",
          description:
            "Relative path under /srv/apps/<name>. The file name must start with .env (e.g. .env, .env.local, apps/web/.env).",
        },
        content: { type: "string", description: "Full file body to write." },
        restart: {
          type: "boolean",
          description:
            "Restart the app after writing so it picks up the new values. Default false.",
          default: false,
        },
      },
      required: ["name", "path", "content"],
      additionalProperties: false,
    },
    call: async (args) =>
      agent.apps.saveEnvFile(requireString(args, "name"), {
        path: requireString(args, "path"),
        content: requireString(args, "content"),
        restart: optionalBoolean(args, "restart") ?? false,
      }),
  },
];
