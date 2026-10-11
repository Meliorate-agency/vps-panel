import "server-only";
import http from "node:http";
import { config } from "./config";

export class AgentError extends Error {
  constructor(public status: number, public body: string) {
    super(`agent ${status}: ${body}`);
  }
}

type RequestOptions = {
  method: string;
  path: string;
  body?: unknown;
  query?: Record<string, string | number | boolean | undefined>;
};

function buildPath(p: string, q?: RequestOptions["query"]): string {
  if (!q) return p;
  const params = new URLSearchParams();
  for (const [k, v] of Object.entries(q)) {
    if (v === undefined) continue;
    params.set(k, String(v));
  }
  const qs = params.toString();
  return qs ? `${p}?${qs}` : p;
}

export function agentRequest<T = unknown>(opts: RequestOptions): Promise<T> {
  const data = opts.body === undefined ? null : JSON.stringify(opts.body);
  return new Promise<T>((resolve, reject) => {
    const req = http.request(
      {
        socketPath: config.agentSocket,
        method: opts.method,
        path: buildPath(opts.path, opts.query),
        headers: {
          "Content-Type": "application/json",
          Accept: "application/json",
          Authorization: `Bearer ${config.agentToken}`,
          ...(data ? { "Content-Length": String(Buffer.byteLength(data)) } : {}),
        },
      },
      (res) => {
        const chunks: Buffer[] = [];
        res.on("data", (c: Buffer) => chunks.push(c));
        res.on("end", () => {
          const body = Buffer.concat(chunks).toString("utf8");
          const status = res.statusCode ?? 500;
          if (status >= 400) {
            reject(new AgentError(status, body));
            return;
          }
          if (!body) {
            resolve(undefined as T);
            return;
          }
          try {
            resolve(JSON.parse(body) as T);
          } catch {
            reject(new AgentError(500, `non-json response: ${body.slice(0, 200)}`));
          }
        });
      },
    );
    req.on("error", reject);
    if (data) req.write(data);
    req.end();
  });
}

export const agent = {
  health: () => agentRequest<{ ok: boolean; version: string; now: string }>({ method: "GET", path: "/health" }),
  sysinfo: () => agentRequest<SysInfo>({ method: "GET", path: "/sysinfo" }),

  apps: {
    list: () => agentRequest<AppRow[]>({ method: "GET", path: "/apps" }),
    get: (name: string) => agentRequest<AppRow>({ method: "GET", path: `/apps/${encodeURIComponent(name)}` }),
    create: (body: CreateApp) => agentRequest<TaskAccepted>({ method: "POST", path: "/apps", body }),
    delete: (name: string, opts?: { wipe?: boolean }) =>
      agentRequest<Ok>({
        method: "DELETE",
        path: `/apps/${encodeURIComponent(name)}`,
        query: opts?.wipe ? { wipe: "true" } : undefined,
      }),
    start: (name: string) =>
      agentRequest<Ok>({ method: "POST", path: `/apps/${encodeURIComponent(name)}/start` }),
    stop: (name: string) =>
      agentRequest<Ok>({ method: "POST", path: `/apps/${encodeURIComponent(name)}/stop` }),
    restart: (name: string) =>
      agentRequest<Ok>({ method: "POST", path: `/apps/${encodeURIComponent(name)}/restart` }),
    /** One-click update: pull latest from git, reinstall deps, build, restart. */
    update: (name: string) =>
      agentRequest<TaskAccepted>({
        method: "POST",
        path: `/apps/${encodeURIComponent(name)}/update`,
      }),
    /** Upserts keys into the EnvironmentFile (others are kept) and restarts
     *  the app when it has a routing slot. */
    patchEnv: (name: string, env: { key: string; value: string }[]) =>
      agentRequest<{ ok: boolean; restarted: boolean }>({
        method: "PATCH",
        path: `/apps/${encodeURIComponent(name)}/env`,
        body: { env },
      }),
    gitStatus: (name: string) =>
      agentRequest<GitStatusResp>({
        method: "GET",
        path: `/apps/${encodeURIComponent(name)}/git-status`,
      }),
    sourceState: (name: string) =>
      agentRequest<{ state: "missing" | "empty" | "populated" }>({
        method: "GET",
        path: `/apps/${encodeURIComponent(name)}/source-state`,
      }),
    /** List every `.env*` file in the app's source dir (the files the app's
     *  own framework reads, e.g. Next.js `.env.local`). */
    listEnvFiles: (name: string) =>
      agentRequest<AppEnvFilesResp>({
        method: "GET",
        path: `/apps/${encodeURIComponent(name)}/env-files`,
      }),
    /** Create or overwrite a `.env*` file in the source dir, then optionally
     *  restart the app. `path` is relative to the source dir. */
    saveEnvFile: (
      name: string,
      body: { path: string; content: string; restart: boolean },
    ) =>
      agentRequest<{ ok: boolean; restarted: string | null }>({
        method: "PUT",
        path: `/apps/${encodeURIComponent(name)}/env-files`,
        body,
      }),
    /** Delete a `.env*` file from the source dir, then optionally restart. */
    deleteEnvFile: (
      name: string,
      body: { path: string; restart: boolean },
    ) =>
      agentRequest<{ ok: boolean; restarted: string | null }>({
        method: "DELETE",
        path: `/apps/${encodeURIComponent(name)}/env-files`,
        body,
      }),
    /** Rewrite the app's systemd ExecStart and restart so it takes effect. */
    setEntrypoint: (name: string, execStart: string) =>
      agentRequest<{ ok: boolean; restarted: boolean }>({
        method: "PATCH",
        path: `/apps/${encodeURIComponent(name)}/entrypoint`,
        body: { exec_start: execStart },
      }),
    /** Move the app onto another routing slot (port-mode site) - rewrites
     *  PORT and restarts - or, with both fields null, take it off its slot
     *  (stop + disable). */
    setRouting: (
      name: string,
      body: { subdomain: string | null; port: number | null },
    ) =>
      agentRequest<AppRoutingResp>({
        method: "PATCH",
        path: `/apps/${encodeURIComponent(name)}/routing`,
        body,
      }),
  },

  services: {
    list: () => agentRequest<ServiceRow[]>({ method: "GET", path: "/services" }),
    action: (unit: string, action: string) =>
      agentRequest<Ok>({
        method: "POST",
        path: `/services/${encodeURIComponent(unit)}/${encodeURIComponent(action)}`,
      }),
  },

  system: {
    services: () =>
      agentRequest<CoreServiceState[]>({ method: "GET", path: "/system/services" }),
    /**
     * List Linux users from /etc/passwd suitable for running cron jobs etc.
     * Filtered to root + UID >= 33 (excludes kernel pseudo-users + nobody),
     * sorted with root pinned first. `is_human` is true for root + UID >= 1000
     * so the /users page can split human accounts from service users; `sudo`
     * is true when the user is a member of the `sudo` group.
     */
    users: () =>
      agentRequest<SystemUserRow[]>({
        method: "GET",
        path: "/system/users",
      }),
    createUser: (body: {
      name: string;
      password: string;
      sudo?: boolean;
      shell?: string;
    }) =>
      agentRequest<{ name: string; uid: number; home: string }>({
        method: "POST",
        path: "/system/users",
        body,
      }),
    deleteUser: (name: string, opts?: { removeHome?: boolean }) =>
      agentRequest<Ok>({
        method: "DELETE",
        path: `/system/users/${encodeURIComponent(name)}`,
        query: opts?.removeHome ? { remove_home: "true" } : undefined,
      }),
    setUserPassword: (name: string, password: string) =>
      agentRequest<Ok>({
        method: "POST",
        path: `/system/users/${encodeURIComponent(name)}/password`,
        body: { password },
      }),
    aptUpgrade: () =>
      agentRequest<TaskAccepted>({ method: "POST", path: "/system/apt-upgrade" }),
    /** Panel access allowlist into nginx; [] lifts it. */
    setPanelAccess: (entries: string[]) =>
      agentRequest<Ok>({ method: "PUT", path: "/system/panel-access", body: { entries } }),
    setPanelHost: (
      host: string,
      opts?: { createParentZone?: boolean; issueSsl?: boolean },
    ) =>
      agentRequest<TaskAccepted>({
        method: "POST",
        path: "/system/panel-host",
        body: {
          host,
          create_parent_zone: opts?.createParentZone ?? true,
          issue_ssl: opts?.issueSsl ?? true,
        },
      }),
    setRootPassword: (password: string) =>
      agentRequest<{ ok: true }>({
        method: "POST",
        path: "/system/root-password",
        body: { password },
      }),
    updateCheck: () =>
      agentRequest<UpdateCheckResult>({ method: "GET", path: "/system/update-check" }),
    selfUpdate: () =>
      agentRequest<TaskAccepted>({ method: "POST", path: "/system/self-update" }),
    /** The swap file the panel manages (/swapfile), other swap, RAM and disk room. */
    swap: () => agentRequest<SwapStatus>({ method: "GET", path: "/system/swap" }),
    /** Create or resize (size_mb > 0) or remove (0) /swapfile and set
     *  vm.swappiness, as a task. Refused (409) when the disk or free RAM
     *  cannot take the change. */
    setSwap: (body: { size_mb: number; swappiness: number }) =>
      agentRequest<TaskAccepted>({ method: "PUT", path: "/system/swap", body }),
    /** The server's own hostname (not PANEL_HOST) and whether cloud-init would reset it. */
    hostname: () => agentRequest<HostnameStatus>({ method: "GET", path: "/system/hostname" }),
    /** hostnamectl + the 127.0.1.1 line in /etc/hosts + a cloud-init drop-in
     *  so the name survives reboots. `steps` says what changed. */
    setHostname: (hostname: string) =>
      agentRequest<{ hostname: string; steps: string[] }>({
        method: "PUT",
        path: "/system/hostname",
        body: { hostname },
      }),
  },

  ssl: {
    list: () => agentRequest<CertRow[]>({ method: "GET", path: "/ssl" }),
    /** Re-issue the cert for `domain` with the full SAN set (www when forced,
     *  mail.<domain> when it is a mail tenant). Returns the agent task id so
     *  the UI can follow it; the cert lands when the task succeeds. */
    renew: (domain: string) =>
      agentRequest<TaskAccepted>({ method: "POST", path: "/ssl/renew", body: { domain } }),
  },

  dns: {
    listZones: () => agentRequest<ZoneRow[]>({ method: "GET", path: "/dns/zones" }),
    createZone: (name: string, seedIp?: string) =>
      agentRequest<ZoneRow>({
        method: "POST",
        path: "/dns/zones",
        body: seedIp ? { name, seed_ip: seedIp } : { name },
      }),
    deleteZone: (zone: string) =>
      agentRequest<Ok>({ method: "DELETE", path: `/dns/zones/${encodeURIComponent(zone)}` }),
    signZone: (zone: string) =>
      agentRequest<Ok>({ method: "POST", path: `/dns/zones/${encodeURIComponent(zone)}/sign` }),
    reconcileZone: (zone: string) =>
      agentRequest<ReconcileZoneResp>({
        method: "POST",
        path: `/dns/zones/${encodeURIComponent(zone)}/reconcile`,
      }),
    listRecords: (zone: string) =>
      agentRequest<RecordRow[]>({
        method: "GET",
        path: `/dns/zones/${encodeURIComponent(zone)}/records`,
      }),
    createRecord: (zone: string, body: CreateRecord) =>
      agentRequest<RecordRow>({
        method: "POST",
        path: `/dns/zones/${encodeURIComponent(zone)}/records`,
        body,
      }),
    updateRecord: (id: number, body: UpdateRecord) =>
      agentRequest<Ok>({ method: "PATCH", path: `/dns/records/${id}`, body }),
    deleteRecord: (id: number) =>
      agentRequest<Ok>({ method: "DELETE", path: `/dns/records/${id}` }),
  },

  mail: {
    listDomains: () => agentRequest<MailDomainRow[]>({ method: "GET", path: "/mail/domains" }),
    /** `issueSsl` (default true) lets the agent get a cert covering
     *  mail.<name> right away. Pass false when the caller is about to issue
     *  the site cert itself, which will carry mail.<name> as a SAN. */
    createDomain: (name: string, opts?: { issueSsl?: boolean }) =>
      agentRequest<MailDomainCreateResp>({
        method: "POST",
        path: "/mail/domains",
        body: { name, issue_ssl: opts?.issueSsl ?? true },
      }),
    /** Make sure a valid cert covers mail.<name>: re-issues the tenant's site
     *  cert with the mail SAN, or a standalone mail.<name> cert. task_id is
     *  null when a cert already covers it. */
    ensureSsl: (name: string) =>
      agentRequest<MailSslResp>({
        method: "POST",
        path: `/mail/domains/${encodeURIComponent(name)}/ssl`,
      }),
    listMailboxes: () => agentRequest<MailboxListResp>({ method: "GET", path: "/mail/mailboxes" }),
    createMailbox: (body: { email: string; password?: string; quota_mb?: number }) =>
      agentRequest<{ email: string; initial_password: string }>({
        method: "POST",
        path: "/mail/mailboxes",
        body,
      }),
    updateMailbox: (email: string, body: { password?: string; quota_mb?: number }) =>
      agentRequest<Ok>({
        method: "PATCH",
        path: `/mail/mailboxes/${encodeURIComponent(email)}`,
        body,
      }),
    mailboxUsage: (email: string) =>
      agentRequest<MailboxUsage>({
        method: "GET",
        path: `/mail/mailboxes/${encodeURIComponent(email)}/usage`,
      }),
    // The maildir goes to Files > Trash (restorable for 30 days); null trash
    // id when the mailbox had no mail on disk.
    deleteMailbox: (email: string) =>
      agentRequest<{ ok: boolean; maildir_trash_id: string | null }>({
        method: "DELETE",
        path: `/mail/mailboxes/${encodeURIComponent(email)}`,
      }),
    /** Forwarders: one row per (source, destination). source `@domain` is
     *  the domain's catch-all. */
    listAliases: () => agentRequest<MailAliasRow[]>({ method: "GET", path: "/mail/aliases" }),
    createAlias: (source: string, destinations: string[]) =>
      agentRequest<{ added: number }>({
        method: "POST",
        path: "/mail/aliases",
        body: { source, destinations },
      }),
    deleteAlias: (id: number) =>
      agentRequest<Ok>({ method: "DELETE", path: `/mail/aliases/${id}` }),
    /** What Postfix did with mail to or from `address` over the last `days`
     *  (1-14), newest first. */
    trackDelivery: (address: string, days: number) =>
      agentRequest<MailTrackResp>({
        method: "GET",
        path: "/mail/track",
        query: { address, days },
      }),
  },

  databases: {
    listUsers: () => agentRequest<DbUser[]>({ method: "GET", path: "/db/users" }),
    listDatabases: () => agentRequest<DbDatabase[]>({ method: "GET", path: "/db/databases" }),
    createUser: (body: { user: string; password?: string; host?: string }) =>
      agentRequest<{ user: string; host: string; initial_password: string }>({
        method: "POST",
        path: "/db/users",
        body,
      }),
    setUserPassword: (user: string, body: { host?: string; password?: string }) =>
      agentRequest<{ user: string; host: string; password: string }>({
        method: "POST",
        path: `/db/users/${encodeURIComponent(user)}/password`,
        body,
      }),
    createDatabase: (name: string) =>
      agentRequest<DbDatabase>({ method: "POST", path: "/db/databases", body: { name } }),
    grant: (body: { user: string; host?: string; database: string; privileges?: string }) =>
      agentRequest<Ok>({ method: "POST", path: "/db/grant", body }),
    /** Drop a user's db-level grants on one database — the inverse of
     *  `grant`, wired to the × on the Granted-users chips. */
    revoke: (body: { user: string; host?: string; database: string }) =>
      agentRequest<Ok>({ method: "POST", path: "/db/revoke", body }),
    /** Import a staged `.sql` dump (uploaded through /files, normally into
     *  `drive:sql-dumps/`) by piping it through the mariadb CLI as a
     *  background task. The agent auto-detects a CREATE DATABASE / USE
     *  header; `target_db` only applies to headerless dumps. */
    importSql: (body: {
      root: string;
      path: string;
      target_db?: string;
      create_target?: boolean;
      drop_first?: boolean;
    }) =>
      agentRequest<DbImportAccepted>({ method: "POST", path: "/db/import", body }),
    /** With `host`, drops only user@host; without it, every host of the user. */
    deleteUser: (user: string, host?: string) =>
      agentRequest<Ok>({
        method: "DELETE",
        path: `/db/users/${encodeURIComponent(user)}`,
        query: { host },
      }),
    deleteDatabase: (name: string) =>
      agentRequest<Ok>({ method: "DELETE", path: `/db/databases/${encodeURIComponent(name)}` }),
  },

  // PostgreSQL — parallel surface to `databases`. The agent runs psql as
  // the postgres unix user (peer auth), so no connection credentials are
  // needed here. The split keeps MariaDB and Postgres independent: each
  // engine has its own routes, types, and reserved-name list.
  postgres: {
    listUsers: () =>
      agentRequest<PgRole[]>({ method: "GET", path: "/pg/users" }),
    listDatabases: () =>
      agentRequest<PgDatabase[]>({ method: "GET", path: "/pg/databases" }),
    createRole: (body: { user: string; password?: string; superuser?: boolean }) =>
      agentRequest<{ user: string; initial_password: string }>({
        method: "POST",
        path: "/pg/users",
        body,
      }),
    setRolePassword: (user: string, body: { password?: string }) =>
      agentRequest<{ user: string; password: string }>({
        method: "POST",
        path: `/pg/users/${encodeURIComponent(user)}/password`,
        body,
      }),
    createDatabase: (body: { name: string; owner?: string }) =>
      agentRequest<PgDatabase>({
        method: "POST",
        path: "/pg/databases",
        body,
      }),
    updateDatabase: (
      name: string,
      body: { new_name?: string; owner?: string },
    ) =>
      agentRequest<Ok>({
        method: "PATCH",
        path: `/pg/databases/${encodeURIComponent(name)}`,
        body,
      }),
    grant: (body: { user: string; database: string; privileges?: string }) =>
      agentRequest<Ok>({ method: "POST", path: "/pg/grant", body }),
    deleteRole: (user: string) =>
      agentRequest<Ok>({
        method: "DELETE",
        path: `/pg/users/${encodeURIComponent(user)}`,
      }),
    deleteDatabase: (name: string) =>
      agentRequest<Ok>({
        method: "DELETE",
        path: `/pg/databases/${encodeURIComponent(name)}`,
      }),
    // pgvector: `available` reflects whether the system package is installed;
    // `enabled` lists the databases that already have `CREATE EXTENSION vector`.
    pgvectorStatus: () =>
      agentRequest<{ available: boolean; enabled: string[] }>({
        method: "GET",
        path: "/pg/pgvector",
      }),
    enablePgvector: (database: string) =>
      agentRequest<Ok>({
        method: "POST",
        path: "/pg/pgvector/enable",
        body: { database },
      }),
    disablePgvector: (database: string) =>
      agentRequest<Ok>({
        method: "POST",
        path: "/pg/pgvector/disable",
        body: { database },
      }),
  },

  notifications: {
    /** Send a test message to one channel now; rejects when delivery fails. */
    test: (channel: string, target: string) =>
      agentRequest<Ok>({ method: "POST", path: "/notifications/test", body: { channel, target } }),
    /** Deliver an event the panel raises (My Networks) to the channels
     *  subscribed to it. The agent sends it in the background. */
    send: (event: string, subject: string, body: string) =>
      agentRequest<Ok>({ method: "POST", path: "/notifications/send", body: { event, subject, body } }),
  },

  backups: {
    list: () => agentRequest<Snapshot[]>({ method: "GET", path: "/backups" }),
    run: (paths?: string[]) =>
      agentRequest<TaskAccepted>({ method: "POST", path: "/backups/run", body: { paths } }),
    restore: (snapshot: string, target: string) =>
      agentRequest<TaskAccepted>({
        method: "POST",
        path: "/backups/restore",
        body: { snapshot, target },
      }),
  },

  firewall: {
    status: () => agentRequest<FirewallStatus>({ method: "GET", path: "/firewall/status" }),
    allow: (port: number, proto = "tcp") =>
      agentRequest<Ok>({ method: "POST", path: "/firewall/allow", body: { port, proto } }),
    deny: (port: number, proto = "tcp") =>
      agentRequest<Ok>({ method: "POST", path: "/firewall/deny", body: { port, proto } }),
    enable: () => agentRequest<Ok>({ method: "POST", path: "/firewall/enable" }),
    setFail2banPolicy: (body: {
      bantime: string;
      findtime: string;
      maxretry: number;
      jails: string[];
    }) => agentRequest<Ok>({ method: "POST", path: "/fail2ban/policy", body }),
    jails: () => agentRequest<JailRow[]>({ method: "GET", path: "/fail2ban/jails" }),
    unban: (jail: string, ip: string) =>
      agentRequest<Ok>({ method: "POST", path: "/fail2ban/unban", body: { jail, ip } }),
    // Bulk-unban every IP in a jail then restart fail2ban so any in-memory
    // failure counters reset too. Useful as an "I've fixed the misconfigured
    // client, let everyone retry" panic button.
    unbanAll: (jail: string) =>
      agentRequest<Ok>({ method: "POST", path: "/fail2ban/unban-all", body: { jail } }),
    // Per-IP failure breakdown for a jail (parses /var/log/fail2ban.log).
    failedByIp: (jail: string) =>
      agentRequest<FailedByIp[]>({
        method: "GET",
        path: `/fail2ban/jails/${encodeURIComponent(jail)}/failed-by-ip`,
      }),
    // Mirror `panel.ip_allowlist` from panel.db into fail2ban's dovecot
    // ignoreip. Agent reads the setting itself, so no body needed.
    syncOperatorIgnoreip: () =>
      agentRequest<Ok>({ method: "POST", path: "/firewall/sync-operator-ignoreip" }),
  },

  ssh: {
    list: (user: string) =>
      agentRequest<KeyRow[]>({ method: "GET", path: "/ssh/keys", query: { user } }),
    add: (body: { user: string; key: string; label?: string }) =>
      agentRequest<Ok>({ method: "POST", path: "/ssh/keys", body }),
    // `purgeKeypair` (a panel-generated keypair name) additionally deletes
    // `~/.ssh/<name>` + `<name>.pub` and strips the matching `Host` block
    // from `~/.ssh/config`, freeing the name for a future generateKeypair.
    remove: (fp: string, user: string, purgeKeypair?: string) =>
      agentRequest<Ok>({
        method: "DELETE",
        path: `/ssh/keys/${encodeURIComponent(fp)}`,
        query: { user, purge_keypair: purgeKeypair },
      }),
    getConfig: () => agentRequest<SshServerConfig>({ method: "GET", path: "/ssh/config" }),
    setConfig: (body: {
      port?: number;
      idle_timeout_minutes?: number;
      password_auth?: boolean;
      permit_root_login?: SshRootLogin;
    }) => agentRequest<Ok>({ method: "PATCH", path: "/ssh/config", body }),
    getPublicKey: (fp: string, user: string) =>
      agentRequest<{ content: string }>({
        method: "GET",
        path: `/ssh/keys/${encodeURIComponent(fp)}/public`,
        query: { user },
      }),
    getPrivateKey: (user: string, name: string) =>
      agentRequest<{ content: string }>({
        method: "GET",
        path: "/ssh/keypair/private",
        query: { user, name },
      }),
    updatePublicKey: (fp: string, user: string, content: string) =>
      agentRequest<{ ok: true; fingerprint: string | null }>({
        method: "PUT",
        path: `/ssh/keys/${encodeURIComponent(fp)}/public`,
        query: { user },
        body: { content },
      }),
    updatePrivateKey: (user: string, name: string, content: string) =>
      agentRequest<{ ok: true; fingerprint: string | null }>({
        method: "PUT",
        path: "/ssh/keypair/private",
        query: { user, name },
        body: { content },
      }),
    getAuthorizedKeysFile: (user: string) =>
      agentRequest<{ content: string }>({
        method: "GET",
        path: "/ssh/files/authorized_keys",
        query: { user },
      }),
    putAuthorizedKeysFile: (user: string, content: string) =>
      agentRequest<{ ok: true }>({
        method: "PUT",
        path: "/ssh/files/authorized_keys",
        query: { user },
        body: { content },
      }),
    getConfigFile: (user: string) =>
      agentRequest<{ content: string }>({
        method: "GET",
        path: "/ssh/files/config",
        query: { user },
      }),
    putConfigFile: (user: string, content: string) =>
      agentRequest<{ ok: true }>({
        method: "PUT",
        path: "/ssh/files/config",
        query: { user },
        body: { content },
      }),
    generateKeypair: (body: { user: string; name: string }) =>
      agentRequest<{
        ok: true;
        public_key: string;
        private_key_path: string;
        public_key_path: string;
      }>({
        method: "POST",
        path: "/ssh/keypair",
        body,
      }),
  },

  cron: {
    list: () => agentRequest<CronJobRow[]>({ method: "GET", path: "/cron/jobs" }),
    create: (body: { schedule: string; user: string; command: string; label?: string }) =>
      agentRequest<CronJobRow>({ method: "POST", path: "/cron/jobs", body }),
    delete: (id: string) =>
      agentRequest<Ok>({ method: "DELETE", path: `/cron/jobs/${encodeURIComponent(id)}` }),
  },

  // Visual filesystem browser behind /files. Every call is scoped to a named
  // root (see the agent's `files::ROOTS`), which is also the jail: the agent
  // canonicalises and refuses anything resolving outside it. Deletes go to a
  // 30-day trash, never straight to `rm`.
  files: {
    roots: () => agentRequest<FileRoot[]>({ method: "GET", path: "/files/roots" }),
    /** List one directory. `path` is relative to the root; "" is the root. */
    list: (root: string, path = "") =>
      agentRequest<FileListing>({
        method: "GET",
        path: "/files/list",
        query: { root, path },
      }),
    mkdir: (root: string, path: string, name: string) =>
      agentRequest<{ ok: boolean; path: string }>({
        method: "POST",
        path: "/files/mkdir",
        body: { root, path, name },
      }),
    newFile: (root: string, path: string, name: string) =>
      agentRequest<{ ok: boolean; path: string }>({
        method: "POST",
        path: "/files/new-file",
        body: { root, path, name },
      }),
    rename: (root: string, path: string, newName: string) =>
      agentRequest<{ ok: boolean; path: string }>({
        method: "POST",
        path: "/files/rename",
        body: { root, path, new_name: newName },
      }),
    /** Move items into `dest` (a directory relative to the same root).
     *  Partial success is normal — read `errors` for the ones that didn't. */
    move: (root: string, paths: string[], dest: string) =>
      agentRequest<FileBatchResult>({
        method: "POST",
        path: "/files/move",
        body: { root, paths, dest },
      }),
    trash: (root: string, paths: string[]) =>
      agentRequest<FileBatchResult>({
        method: "POST",
        path: "/files/trash",
        body: { root, paths },
      }),
    listTrash: () => agentRequest<TrashRow[]>({ method: "GET", path: "/files/trash" }),
    restore: (id: string) =>
      agentRequest<{ ok: boolean; restored_to: string }>({
        method: "POST",
        path: "/files/trash/restore",
        body: { id },
      }),
    purge: (id: string) =>
      agentRequest<Ok>({
        method: "DELETE",
        path: `/files/trash/${encodeURIComponent(id)}`,
      }),
    emptyTrash: () => agentRequest<Ok>({ method: "POST", path: "/files/trash/empty" }),
  },

  sites: {
    list: () => agentRequest<SiteListEntry[]>({ method: "GET", path: "/sites" }),
    create: (body: CreateSite) =>
      agentRequest<CreateSiteResponse>({ method: "POST", path: "/sites", body }),
    update: (domain: string, body: UpdateSite) =>
      agentRequest<CreateSiteResponse>({
        method: "PATCH",
        path: `/sites/${encodeURIComponent(domain)}`,
        body,
      }),
    delete: (domain: string) =>
      agentRequest<Ok>({
        method: "DELETE",
        path: `/sites/${encodeURIComponent(domain)}`,
      }),
    status: (domain: string) =>
      agentRequest<SiteStatus>({
        method: "GET",
        path: `/sites/${encodeURIComponent(domain)}/status`,
      }),
  },
  redirects: {
    /** Replace the FULL redirect rule set for one domain. The agent rewrites
     *  /etc/nginx/vps-redirects/<domain>/redirects.conf, makes sure the vhost
     *  includes it, runs nginx -t (rolling back on failure) and reloads. An
     *  empty list removes the file. 409 when the domain has no panel-managed
     *  site or app vhost. */
    put: (domain: string, rules: RedirectRuleInput[]) =>
      agentRequest<Ok>({
        method: "PUT",
        path: `/redirects/${encodeURIComponent(domain)}`,
        body: { rules },
      }),
  },

  tasks: {
    list: () => agentRequest<TaskState[]>({ method: "GET", path: "/tasks" }),
    get: (id: string) => agentRequest<TaskState>({ method: "GET", path: `/tasks/${id}` }),
  },

  quickInstall: {
    /** List quick-install recipes. Pass `{ checkUpdates: true }` to also probe
     *  the registry for image updates (sets `update_available` on the n8n
     *  card). Off by default — only the /apps strip, which renders n8n, asks
     *  for it, so /databases doesn't pay the registry round-trip. */
    list: (opts?: { checkUpdates?: boolean }) =>
      agentRequest<QuickInstallRecipe[]>({
        method: "GET",
        path: "/quick-install",
        query: opts?.checkUpdates ? { check_updates: true } : undefined,
      }),
    run: (slug: string) =>
      agentRequest<TaskAccepted>({
        method: "POST",
        path: `/quick-install/${encodeURIComponent(slug)}`,
      }),
    setBind: (slug: string, bind: string) =>
      agentRequest<TaskAccepted>({
        method: "POST",
        path: `/quick-install/${encodeURIComponent(slug)}/bind`,
        body: { bind },
      }),
    /** Read the Roundcube container's current IMAP/SMTP target so the
     *  panel UI can show it as editable + warn when it drifts from the
     *  host's postfix myhostname. */
    getRoundcubeSettings: () =>
      agentRequest<RoundcubeSettings>({
        method: "GET",
        path: "/quick-install/roundcube/settings",
      }),
    /** Recreate the Roundcube container with the supplied values + wipe
     *  the cached config volume. Returns a task id so the UI can stream
     *  the docker recreation log. */
    updateRoundcubeSettings: (req: {
      mail_host: string;
      imap_port: number;
      smtp_port: number;
    }) =>
      agentRequest<TaskAccepted>({
        method: "POST",
        path: "/quick-install/roundcube/settings",
        body: req,
      }),
    /** Read the n8n container's current public-URL env vars + any
     *  routing-slot suggestions from /domains (port-mode sites mapped
     *  to upstream_port=5678). */
    getN8nSettings: () =>
      agentRequest<N8nSettings>({
        method: "GET",
        path: "/quick-install/n8n/settings",
      }),
    /** Recreate the n8n container with N8N_HOST / N8N_PROTOCOL /
     *  WEBHOOK_URL / N8N_EDITOR_BASE_URL set from the operator's input.
     *  Preserves the current bind + the n8n_data volume (workflows,
     *  credentials, sqlite DB) — those are untouched. */
    updateN8nSettings: (req: {
      host: string;
      protocol: "http" | "https";
      webhook_url: string;
    }) =>
      agentRequest<TaskAccepted>({
        method: "POST",
        path: "/quick-install/n8n/settings",
        body: req,
      }),
    /** Pull the latest n8nio/n8n image and recreate the container on it.
     *  Preserves the operator's public-URL env, the port bind, and the
     *  n8n_data volume (workflows, credentials, sqlite DB). Returns a task
     *  id so the UI can stream the update log. */
    updateN8n: () =>
      agentRequest<TaskAccepted>({
        method: "POST",
        path: "/quick-install/n8n/update",
      }),
    /** Read the phpMyAdmin container's current PMA_ABSOLUTE_URI + any
     *  routing-slot suggestions from /domains (port-mode sites mapped
     *  to upstream_port=8081). */
    getPhpMyAdminSettings: () =>
      agentRequest<PhpMyAdminSettings>({
        method: "GET",
        path: "/quick-install/phpmyadmin/settings",
      }),
    /** Recreate the phpMyAdmin container with PMA_ABSOLUTE_URI set from
     *  the operator's input. Empty string clears the env var. Preserves
     *  the current bind. Fixes the "Failed to set session cookie" login
     *  error when phpMyAdmin is behind an nginx HTTPS proxy. */
    updatePhpMyAdminSettings: (req: { absolute_uri: string }) =>
      agentRequest<TaskAccepted>({
        method: "POST",
        path: "/quick-install/phpmyadmin/settings",
        body: req,
      }),
  },

  /** Tailscale on this server (installed by the quick-install recipe). */
  tailscale: {
    /** State, this server's tailnet address, machines, settings and, while a
     *  login started here waits, the login link. The link lets whoever opens
     *  it put this server in their tailnet: admins only. */
    status: () => agentRequest<TailscaleStatus>({ method: "GET", path: "/tailscale/status" }),
    /** `tailscale up --reset` with the options, as task `tailscale:up`: waits
     *  up to 15 min for someone to open the link, or logs in with the key. */
    login: (body: TailscaleOptions & { auth_key: string }) =>
      agentRequest<TaskAccepted>({ method: "POST", path: "/tailscale/login", body }),
    /** `tailscale set` for a logged-in server; other settings stay. */
    saveSettings: (body: TailscaleOptions) =>
      agentRequest<TailscaleSteps>({ method: "PUT", path: "/tailscale/settings", body }),
    connect: () => agentRequest<TailscaleSteps>({ method: "POST", path: "/tailscale/connect" }),
    disconnect: () =>
      agentRequest<TailscaleSteps>({ method: "POST", path: "/tailscale/disconnect" }),
    logout: () => agentRequest<TailscaleSteps>({ method: "POST", path: "/tailscale/logout" }),
  },
};

/** What the panel sets on Tailscale. Empty hostname = the server's own. */
export type TailscaleOptions = {
  hostname: string;
  ssh: boolean;
  accept_routes: boolean;
  accept_dns: boolean;
  exit_node: boolean;
};

export type TailscaleNode = {
  name: string;
  /** MagicDNS name, no trailing dot. */
  dns_name: string;
  ips: string[];
  os: string;
  online: boolean;
  /** RFC 3339; only for a machine that is offline. */
  last_seen: string | null;
  /** This server sends its internet traffic through that machine. */
  exit_node: boolean;
  exit_node_option: boolean;
};

export type TailscaleStatus = {
  installed: boolean;
  version: string | null;
  /** `systemctl is-active tailscaled`. */
  daemon: string;
  server_hostname: string;
  /** Running, NeedsLogin, NeedsMachineAuth, Stopped, Starting or NoState. */
  backend_state: string | null;
  auth_url: string | null;
  login_task: string | null;
  node: TailscaleNode | null;
  tailnet: string | null;
  user: string | null;
  peers: TailscaleNode[];
  health: string[];
  prefs: (TailscaleOptions & { advertised_routes: string[]; shields_up: boolean }) | null;
  error: string | null;
};

export type TailscaleSteps = { steps: string[] };

export type RoundcubeSettings = {
  mail_host: string;
  imap_port: number;
  smtp_port: number;
  detected_mail_host: string | null;
  in_sync: boolean;
};

export type N8nSettings = {
  host: string;
  protocol: string;
  webhook_url: string;
  bind: string;
  suggested_domains: string[];
};

export type PhpMyAdminSettings = {
  /** Current PMA_ABSOLUTE_URI env value on the container. Empty if unset. */
  absolute_uri: string;
  /** Current bind ("127.0.0.1" or "0.0.0.0"). */
  bind: string;
  /** Domains in panel.db with a port-mode site at upstream_port=8081. */
  suggested_domains: string[];
};

/** A browsable jail on the /files page. `writable` false = browse-only
 *  (logs, nginx config). `exists` false = the directory isn't on this box. */
export type FileRoot = {
  id: string;
  label: string;
  path: string;
  description: string;
  writable: boolean;
  exists: boolean;
};

export type FileEntry = {
  name: string;
  kind: "dir" | "file" | "symlink" | "other";
  size: number;
  /** RFC3339 UTC, or null when the mtime couldn't be read. */
  modified: string | null;
  /** Four octal digits, e.g. "0755". */
  mode: string;
  owner: string;
  group: string;
  /** Verbatim symlink target (not resolved). */
  link_target: string | null;
  link_is_dir: boolean;
  /** True when the entry can't be renamed, moved or deleted — a protected
   *  system path, an ancestor of one, or a top-level directory. */
  locked: boolean;
};

export type FileListing = {
  root: FileRoot;
  /** Path relative to the root, forward-slashed. "" at the root itself. */
  path: string;
  abs_path: string;
  /** True when new items may be created in THIS directory. */
  writable: boolean;
  entries: FileEntry[];
  /** True when the directory holds more items than the agent returns. */
  truncated: boolean;
};

/** Move/trash are batch operations: `errors` carries the per-item failures so
 *  a drop of five items that half-lands still reports both halves. */
export type FileBatchResult = {
  ok: boolean;
  moved?: number;
  trashed?: number;
  errors: string[];
};

export type TrashRow = {
  id: string;
  name: string;
  kind: string;
  root_id: string;
  /** Absolute path the item came from. Restore puts it back here. */
  original_path: string;
  size: number;
  trashed_at: string;
  /** Whole days until the agent's purge loop collects it (30-day retention). */
  days_left: number;
};

export type SshRootLogin = "yes" | "prohibit-password" | "forced-commands-only" | "no";

/** What sshd actually runs with (`sshd -T`, drop-ins included). */
export type SshServerConfig = {
  port: number;
  /** ChannelTimeout for sessions, in minutes; 0 = never. */
  idle_timeout_minutes: number;
  password_auth: boolean;
  permit_root_login: SshRootLogin;
};

export type SystemUserRow = {
  name: string;
  uid: number;
  home: string;
  shell: string;
  /** True for root + UID >= 1000 — i.e. operator-shaped accounts. False for
   *  service users (www-data, redis, postgres, ...). */
  is_human: boolean;
  /** Membership in the Debian/Ubuntu `sudo` group. */
  sudo: boolean;
  /** Reserved system account: the agent refuses password reset and delete. */
  reserved: boolean;
};

export type QuickInstallRecipe = {
  slug: string;
  name: string;
  description: string;
  default_port: number;
  installed: boolean;
  web_ui: boolean;
  bind: string | null;
  /** For installed docker recipes whose image we track (currently n8n):
   *  true when the registry has a newer image than the running container.
   *  null = not applicable or the registry couldn't be reached. */
  update_available: boolean | null;
};

export type Ok = { ok: boolean };
export type TaskAccepted = { task_id: string };

export type SysInfo = {
  uptime_secs: number;
  load_1: number;
  load_5: number;
  load_15: number;
  mem_total_kb: number;
  mem_available_kb: number;
  swap_total_kb: number;
  swap_free_kb: number;
  cpu_count: number;
  disk: { mount: string; total_bytes: number; free_bytes: number }[];
  net: { iface: string; rx_bytes: number; tx_bytes: number }[];
};

export type SwapArea = {
  name: string;
  /** "file" or "partition", as /proc/swaps says. */
  kind: string;
  size_bytes: number;
  used_bytes: number;
};

/** GET /system/swap. */
export type SwapStatus = {
  path: string;
  /** Why this server cannot have a swap file (a container); null when it can. */
  unsupported: string | null;
  /** Size on disk; null when there is no swap file. */
  file_bytes: number | null;
  active: boolean;
  /** As the kernel uses it (a little under file_bytes); 0 when not active. */
  size_bytes: number;
  used_bytes: number;
  /** /etc/fstab turns it on at boot. */
  in_fstab: boolean;
  /** Every other active swap area. */
  other: SwapArea[];
  /** A temporary file an interrupted change left; the next save removes it. */
  leftover_bridge: string | null;
  swappiness: number;
  mem_total_bytes: number;
  mem_available_bytes: number;
  disk_total_bytes: number;
  disk_free_bytes: number;
  /** Always kept free: the larger of 2 GiB and 10% of the disk. */
  disk_reserve_bytes: number;
  min_mb: number;
  /** Largest swap file the disk takes now, the current file's space included. */
  max_mb: number;
  /** A swap change is running. */
  busy: boolean;
};

/** GET /system/hostname: the server's own name, separate from PANEL_HOST. */
export type HostnameStatus = {
  /** Static hostname (/etc/hostname): the name after a reboot. */
  hostname: string;
  /** The running kernel's hostname. */
  running: string;
  /** cloud-init is installed, so it resets the hostname at boot unless told not to. */
  cloud_init: boolean;
  /** The panel's cloud-init drop-in is in place. */
  preserved: boolean;
};

export type AppRow = {
  name: string;
  subdomain: string;
  port: number;
  source_dir: string;
  active: string;
  sub: string;
  kind: "panel" | "external";
  /** "missing" → /srv/apps/<name> doesn't exist
   *  "empty"   → exists but contains nothing
   *  "populated" → exists with code */
  source_dir_state: "missing" | "empty" | "populated";
};

export type GitStatusResp = {
  has_repo: boolean;
  behind: number;
  ahead: number;
  current_sha: string | null;
  upstream_sha: string | null;
  error: string | null;
};

export type AppEnvFile = {
  /** Path relative to the source dir, forward-slashed (e.g. `.env.local`). */
  path: string;
  /** Absolute on-disk path, for display. */
  abs_path: string;
  size: number;
  /** File body. Empty when `too_large`. */
  content: string;
  /** Set when the file exceeds the inline cap; shown read-only. */
  too_large: boolean;
};

export type AppEnvFilesResp = {
  source_dir: string;
  files: AppEnvFile[];
  /** True when the scan hit the result cap and stopped early. */
  truncated: boolean;
};

export type CreateApp = {
  name: string;
  source_dir: string;
  port: number;
  subdomain: string;
  runtime: string;
  /** Omit to keep the existing unit's ExecStart on a re-deploy
   *  (`/usr/bin/npm start` for a new app). */
  exec_start?: string;
  /** Omit to keep the existing EnvironmentFile on a re-deploy (empty for a
   *  new app); a list replaces the file. */
  env?: { key: string; value: string }[];
  issue_ssl: boolean;
  skip_dns_check?: boolean;
  /** Optional git repo to clone into source_dir before the deploy runs.
   *  Streamed git/npm output goes into the task log. */
  repo_url?: string;
  /** Which subset of the pipeline to run. Default = full deploy. */
  step?: "pull" | "install" | "build" | "deploy" | "all";
};

export type AppRoutingResp = {
  ok: boolean;
  state: "assigned" | "unassigned";
  /** Assign only: the app came up on the new port within the wait window. */
  bound: boolean;
  /** journalctl tail when `bound` is false. */
  log: string[];
};

export type ServiceRow = {
  unit: string;
  load: string;
  active: string;
  sub: string;
  description: string;
};

export type CoreServiceState = {
  name: string;
  package: string;
  unit: string;
  installed: boolean;
  active: boolean;
  enabled: boolean;
  quick_install_slug: string | null;
};

export type UpdateCheckResult = {
  up_to_date: boolean;
  behind: number;
  ahead: number;
  current_commit: string;
  target_commit: string;
  repo_dir: string;
  fetch_error: string | null;
};

export type CertRow = {
  domain: string;
  not_after: string;
  days_remaining: number;
  /** false = no nginx vhost serves this name any more (site removed, cert
   *  kept). The renewal loop skips these and the dashboard does not count
   *  them as expiring. */
  serving: boolean;
};

export type ZoneRow = { id: number; name: string; type: string; records: number };
export type RecordRow = {
  id: number;
  name: string;
  type: string;
  content: string;
  ttl: number;
  prio: number;
  disabled: boolean;
};
export type CreateRecord = {
  name: string;
  type: string;
  content: string;
  ttl?: number;
  prio?: number;
};
export type UpdateRecord = {
  content?: string;
  ttl?: number;
  disabled?: boolean;
  prio?: number;
  /** New record name. "@" / empty → apex. Bare label → `<label>.<zone>`. */
  name?: string;
  /** New record type (e.g. switch A → CNAME). Validated server-side. */
  type?: string;
};
export type ReconcileZoneResp = {
  added: string[];
  updated: string[];
  unchanged: string[];
  non_standard_extras: string[];
};

export type MailDomainRow = { id: number; name: string; mailboxes: number };
export type MailDomainCreateResp = {
  name: string;
  dkim_status: string;
  records_published: boolean;
  note: string;
  /** Agent task issuing a cert that covers mail.<name>, or null when one
   *  already does / the caller asked for issue_ssl=false. */
  ssl_task_id: string | null;
};

export type MailSslResp = {
  /** null when a valid cert already covers mail.<name> (nothing to do). */
  task_id: string | null;
  already_covered: boolean;
};
export type MailboxRow = { id: number; email: string; quota_mb: number };
export type MailboxListResp = { server: string; mailboxes: MailboxRow[] };
export type MailboxUsage = { usage_bytes: number; usage_mb: number };
export type MailAliasRow = { id: number; source: string; destination: string };
export type MailTrackStatus =
  | "delivered"
  | "sent"
  | "deferred"
  | "bounced"
  | "expired"
  | "rejected"
  | "queued";
export type MailTrackRow = {
  /** journalctl short-iso, e.g. 2026-10-11T14:11:03+0200 */
  time: string;
  queue_id: string | null;
  /** "" for bounce notices and Out of Office replies (null sender) */
  from: string;
  /** "" while nothing has been tried yet (status queued) */
  to: string;
  /** The address the mail was sent to, when a forwarder rewrote it. */
  orig_to: string | null;
  status: MailTrackStatus;
  detail: string;
};
export type MailTrackResp = {
  address: string;
  days: number;
  rows: MailTrackRow[];
  truncated: boolean;
};

export type DbUser = { user: string; host: string };
/** A user@host pair holding db-level grants on a database. Global `ON *.*`
 *  grants and wildcard-pattern grants don't appear here (they live outside
 *  mysql.db's per-schema rows / would attach to every row as noise). */
export type DbGrantedUser = { user: string; host: string };
export type DbDatabase = { name: string; users: DbGrantedUser[] };

/** Accepted SQL-dump import. `database` is what the import targets —
 *  detected from the dump's own header, or the caller's pick for headerless
 *  dumps. Stream progress via /api/tasks/[task_id]/stream. */
export type DbImportAccepted = {
  task_id: string;
  database: string;
  dump_defines_db: boolean;
};

/** Postgres login role (no host concept like MariaDB — Postgres handles
 *  host-based auth in pg_hba.conf, the role itself isn't host-scoped). */
export type PgRole = { user: string; superuser: boolean };
/** Postgres database row. Owner is the role name (resolved server-side
 *  via pg_authid join) so the UI can show "owned by main" without a
 *  follow-up lookup. */
export type PgDatabase = { name: string; owner: string };

export type Snapshot = { id: string; time: string; paths: string[]; hostname: string };

export type ListeningPort = {
  proto: "tcp" | "udp";
  address: string;
  port: number;
  process: string;
};

export type FirewallStatus = {
  ufw_enabled: boolean;
  rules: string[];
  listening_ports: ListeningPort[];
  fail2ban_active: boolean;
};
export type JailRow = { name: string; banned: string[]; total_failed: number };
export type FailedByIp = { ip: string; attempts: number; last_seen: string };

export type KeyRow = {
  fingerprint: string;
  key_type: string;
  comment: string;
  /** Set when the row was generated by the panel and a private key file
   * still exists on disk under `~/.ssh/<keypair_name>`. Drives whether
   * the UI offers a "View private key" action. */
  keypair_name: string | null;
  /** False for a keypair in `~/.ssh/` whose public half is not in
   * authorized_keys (e.g. a deploy key put there by hand): the server can
   * use it to reach other hosts, but it doesn't log in here. Listed after
   * the authorized_keys lines. */
  authorized: boolean;
};

export type CronJobRow = {
  id: string;
  schedule: string;
  user: string;
  command: string;
  label: string;
};

export type TaskState = {
  id: string;
  kind: string;
  status: "pending" | "running" | "succeeded" | "failed";
  started_at: string;
  finished_at: string | null;
  log: string[];
};

export type SiteMode = "port" | "folder";

export type CreateSite = {
  domain: string;
  mode: SiteMode;
  upstream_port?: number | null;
  folder_path?: string | null;
  force_www_redirect: boolean;
  issue_ssl: boolean;
};

export type UpdateSite = {
  mode: SiteMode;
  upstream_port?: number | null;
  folder_path?: string | null;
  force_www_redirect: boolean;
  issue_ssl: boolean;
};

export type CreateSiteResponse = { ok: boolean; task_id: string | null };

export type SiteStatus = {
  cert_exists: boolean;
  cert_not_after: string | null;
  days_remaining: number | null;
  /** Set when the name has no cert of its own but the parent domain's cert
   *  lists it as a SAN (mail.<d> / www.<d> riding on <d>). */
  covered_by?: string | null;
};

export type RedirectRuleInput = {
  id: number;
  source_path: string;
  target_url: string;
  code: 301 | 302;
  match_mode: "exact" | "prefix";
};

export type SiteListEntry = {
  domain: string;
  /** "site" panel-managed | "app" panel-deployed app | "panel-host" panel UI vhost
   *  | "external" manual deploy. UI shows the "Remove from agent" action only
   *  for "site" and "external". */
  kind: "site" | "app" | "panel-host" | "external";
};
