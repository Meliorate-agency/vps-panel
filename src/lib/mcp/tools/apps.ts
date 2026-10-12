import "server-only";
import { agent, type CreateApp } from "@/lib/agent";
import { applyAppRouting } from "@/lib/appRouting";
import { recordAppDeploy, recordedApp, slotConflict } from "@/lib/appRows";
import {
  optionalBoolean,
  optionalNumber,
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
      "List every deployed app (Node.js from Git, or a Docker image). Returns one row per app with name, runtime ('node' / 'docker'), image (Docker apps), subdomain, upstream port, source folder, systemd active/sub-state, and source-folder state ('missing' / 'empty' / 'populated', 'none' for Docker apps). Includes drift-detected external apps (folders under /srv/apps not created by the panel).",
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
      "Deploy a new app or re-run a stage on an existing one. runtime 'node' (default): optionally clones a git repo into /srv/apps/<name>, runs npm ci + npm run build, writes the systemd unit (vps-app-<name>.service), and starts it. runtime 'docker': runs a container from the same unit, published on 127.0.0.1:<port> only, with the given env, kept folders (named volumes) and limits. Its image is either pulled from a public registry (image; steps pull / deploy / all) or built from the Dockerfile in repo_url (cloned into /srv/apps/<name> like a Node app; build_context, dockerfile, build_args; steps pull / build / deploy / all). Returns a task_id whose log can be streamed via tasks.get.",
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
          description:
            "'node' (code from Git in /srv/apps/<name>) or 'docker' (an image from a registry). Omit on a re-run to keep the app's runtime; an app never switches runtime (delete it first).",
          enum: ["node", "docker"],
        },
        image: {
          type: "string",
          description:
            "Docker apps: image reference, e.g. louislam/uptime-kuma:1 or ghcr.io/owner/app:1.4. Public registries only. Omit on a re-run to keep the saved Docker settings.",
        },
        build_context: {
          type: "string",
          description:
            "Docker apps built from Git: the repository folder sent to docker build, relative to its root. Default '.'. Giving it (or dockerfile / build_args) with repo_url and no image means a build.",
        },
        dockerfile: {
          type: "string",
          description: "Docker apps built from Git: the Dockerfile, relative to build_context (as in Docker Compose). Default 'Dockerfile'.",
        },
        build_args: {
          type: "array",
          description:
            "Docker apps built from Git: --build-arg values for the Dockerfile's ARG lines. They are stored in the image: no secrets (use env).",
          items: {
            type: "object",
            properties: { key: { type: "string" }, value: { type: "string" } },
            required: ["key", "value"],
          },
        },
        container_port: {
          type: "integer",
          description: "Docker apps: the port the app listens on inside the container (often the image's EXPOSE).",
          minimum: 1,
          maximum: 65535,
        },
        volumes: {
          type: "array",
          description:
            "Docker apps: folders inside the container to keep across restarts and updates (named volumes, included in backups), e.g. ['/app/data'].",
          items: { type: "string" },
        },
        memory_mb: {
          type: "integer",
          description: "Docker apps: memory limit in MB, 0 = none. Default 1024.",
          minimum: 0,
        },
        cpus: {
          type: "number",
          description: "Docker apps: CPU limit in cores (e.g. 0.5), 0 = none. Default 1.",
          minimum: 0,
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
          description: "Run only one stage of the pipeline instead of the full deploy. Default 'all' runs pull→install→build→deploy (Docker apps: pull→deploy; they have no install or build).",
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
      const image = optionalString(args, "image");
      const recorded = recordedApp(name);
      const repoUrl = optionalString(args, "repo_url");
      const buildArgs = Array.isArray(args.build_args)
        ? (args.build_args as { key: string; value: string }[]).filter(
            (e) => typeof e?.key === "string" && typeof e?.value === "string",
          )
        : undefined;
      const buildGiven =
        optionalString(args, "build_context") !== undefined ||
        optionalString(args, "dockerfile") !== undefined ||
        buildArgs !== undefined;
      const runtime =
        optionalString(args, "runtime") ??
        (image || buildGiven ? "docker" : (recorded?.runtime ?? "node"));
      // A build: no image, but build settings, or a repository for an app
      // that is not a Docker app yet (a re-run without settings keeps the
      // saved ones instead of resetting them to the defaults).
      const build =
        runtime === "docker" && !image && (buildGiven || (repoUrl && recorded?.runtime !== "docker"))
          ? {
              context: optionalString(args, "build_context") ?? ".",
              dockerfile: optionalString(args, "dockerfile") ?? "Dockerfile",
              args: buildArgs ?? [],
            }
          : null;
      const docker = runtime === "docker" && (image || build)
        ? {
            image: image ?? "",
            build,
            container_port: optionalNumber(args, "container_port") ?? 0,
            volumes: Array.isArray(args.volumes)
              ? (args.volumes as unknown[]).filter((v): v is string => typeof v === "string")
              : [],
            memory_mb: optionalNumber(args, "memory_mb") ?? 1024,
            cpus: optionalNumber(args, "cpus") ?? 1,
          }
        : undefined;
      const body: CreateApp = {
        name,
        // A build's checkout is where a Node app's is; an image has none; a
        // re-run on saved Docker settings keeps what panel.db recorded.
        source_dir:
          runtime !== "docker" || build
            ? `${APPS_BASE_DIR}/${name}`
            : image
              ? ""
              : (recorded?.source_dir ?? ""),
        subdomain: requireString(args, "subdomain"),
        port: requireNumber(args, "port"),
        runtime,
        docker,
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
      "Node apps: pull latest code, reinstall deps (npm ci or install), run build (if defined in package.json), then restart. Docker apps: pull the image tag again and, if it changed, restart on it (rolled back to the previous image if it does not come up). Returns a task_id; stream the log via tasks.get.",
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
