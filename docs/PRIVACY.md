# Privacy

This page is for the person who runs a Bolt Panel server and answers for the personal data on it: when you write your own privacy policy, fill in a record of processing, or answer someone who asks what you hold about them. It says what the panel and the services it sets up store, where, for how long, what leaves the server, and what the settings under **Settings > Privacy** change. It describes the code of this version on a default install; where a value is a default of Ubuntu or of a package rather than something the panel sets, it says so.

- [At a glance](#at-a-glance)
- [What is stored, where, and for how long](#what-is-stored-where-and-for-how-long)
  - [The panel's database](#the-panels-database)
  - [Mail](#mail)
  - [Logs](#logs)
  - [What you host](#what-you-host)
  - [Backups](#backups)
  - [Retention at a glance](#retention-at-a-glance)
- [What leaves the server](#what-leaves-the-server)
- [Privacy settings](#privacy-settings)
- [Requests about one person](#requests-about-one-person)
- [Cookies](#cookies)
- [What to set if you are in the EU](#what-to-set-if-you-are-in-the-eu)

## At a glance

Whose personal data is on the server:

- **Panel users** (you and anyone you give an account): email address, password hash, sign-in history, and the changes they make and terminal commands they run, in the audit log.
- **Visitors of the sites and apps you host**: IP address, time, the address they asked for and their browser, in the web server's logs.
- **Mailbox users and the people they write with**: the mail itself, addresses and IP addresses in the mail logs, address books in webmail.
- **Anyone who tries to sign in**, to the panel, to SSH or to a mailbox: IP address and the name or email they typed.
- **Whatever your own apps and databases hold.** The panel does not look inside them.

The panel has no analytics, no telemetry and no crash reporting, and sends nothing to its authors. What does leave the server is listed under [What leaves the server](#what-leaves-the-server).

Every privacy setting starts at the behaviour the panel had before the settings existed: full IP addresses and query strings in the web logs, no time limit on logs, and the server-location lookups on. Nothing changes until you change it under Settings > Privacy.

## What is stored, where, and for how long

### The panel's database

`/var/lib/vps-panel/panel.db` (SQLite; `PANEL_DB_PATH` in `/etc/vps-panel/.env`), readable by root and the `vps-panel` user only. "Housekeeping" below is the clean-up the panel runs when it starts and once a day after that (`src/lib/housekeeping.ts`).

| Data | Table | Kept |
|---|---|---|
| Panel accounts: email address, argon2id password hash, the two-factor (TOTP) secret, role, creation time | `users` | Until the account is deleted on the Users page. |
| Sessions: a random token ID, the account, the expiry time. No IP address and no browser details. | `sessions` | Deleted by housekeeping one day after they expire. |
| Sign-in attempts: the IP address, the email address typed (also one that has no account), the time, and whether it succeeded. Two-factor code attempts are recorded the same way, under `totp:<email>`. | `login_attempts` | Failed attempts 7 days, successful ones 30 days (housekeeping). With a shorter log retention: that many days. |
| Audit log: time, account, action, target, result and a message. Targets include email addresses typed at a failed sign-in (also ones with no account), mailbox and forwarder addresses, and an old address when an account's email is changed. Calls from AI clients (MCP) record the client's IP address. **Every command run in the web terminal is recorded word for word** (up to 2,000 characters) with the folder it ran in, before it runs. | `audit_log` | No limit. With a log retention set: that many days. |
| Terminal history: per account and per folder, the last 100 commands, word for word | `terminal_history` | Until 100 newer commands in that folder push it out, or the account is deleted. No time limit. |
| Task history: the kind of task, its status, times, and its whole log (build output, backup output, certificate orders) | `tasks` | 30 days from the start of the task. The agent deletes older finished tasks when it starts and every 24 hours. With a shorter log retention, housekeeping deletes finished tasks after that many days; a task that is still running keeps its row. |
| Notification channels: email addresses, webhook URLs, Telegram user IDs and bot tokens | `notifications` | Until the channel is removed. |
| Settings that name people: the SSL contact email (`panel.acme_email`), the sender of notification email (`notifications.email_from`), trusted IP addresses (`panel.ip_allowlist`), the panel access list | `settings` | Until changed. |
| My Networks: the name, URL and public key of every server in your networks, when each was last heard from, and its uptime, load, memory and disk use | `networks`, `network_members`, `network_invites` | Until the network is deleted or the server leaves it. The final member list of a network this server deleted (`networks_dissolved`): 90 days. |

The panel keeps nothing about you in the browser beyond the two sign-in cookies (see [Cookies](#cookies)).

### Mail

| Data | Where | Kept |
|---|---|---|
| Mailboxes: address, password hash (SHA512-CRYPT), quota | MariaDB, database `mail`, table `virtual_users` | Until the mailbox is deleted on the Mailboxes page. |
| Forwarders and catch-alls: the address and where its mail goes | `mail.virtual_aliases` | Until the forwarder is deleted. Deleting a mailbox also removes the forwarders on its address and the forwarders that deliver to it. |
| The mail itself, in all folders | `/var/vmail/<domain>/<name>/` (Maildir) | Until the mailbox user deletes it. Nothing expires mail automatically, Trash and Junk included. |
| Filters and Out of Office, set in webmail | `/var/vmail/<domain>/<name>/home/` | With the mailbox. |
| A deleted mailbox's mail | `/var/lib/vps-panel/trash/<id>/` (Files > Trash) | 30 days, then deleted. The agent checks every 6 hours, and whenever the trash is listed. Emptying the trash deletes it sooner. Until then it can be restored. |
| Mail waiting to be delivered | `/var/spool/postfix/` | Until delivered or returned to the sender (Postfix's default gives up after 5 days; the panel does not change it). |
| Webmail (Roundcube, if installed from Apps): each webmail user's settings, identities, address book and sessions | Docker volume `roundcube_db` (SQLite), under `/var/lib/docker/volumes/roundcube_db/_data` | Until deleted in webmail or the volume is removed. |

Mail logs are under [Logs](#logs).

### Logs

| Log | What it holds | Where | Kept by default |
|---|---|---|---|
| Web server access log | One line per request to every site, app and the panel itself: the client's IP address, time, the request line with its query string, status, size, referrer (with its query string) and user agent. One file for all sites: the panel writes no per-site logs. | `/var/log/nginx/access.log`, older days in `access.log.1`, `access.log.2.gz` ... | Ubuntu's rotation (`/etc/logrotate.d/nginx`): daily, 14 old files kept, so about 15 days. The panel does not change this unless you set a log retention. |
| Web server error log | Errors with the client's IP address, the request line and the site name. Always the full address and the full request: the privacy settings cannot change this log's format. | `/var/log/nginx/error.log` and its rotated files | Same rotation as the access log. |
| System journal | Postfix: sender and recipient addresses, the name and IP address of every server or client that connects, mailbox login names. Dovecot: every mailbox login with its IP address, and the address tried with a wrong password. sshd: user names and IP addresses of logins and failed attempts. The output of your Node apps. The panel and the agent (the agent records the address of a mailbox it deletes). | `/var/log/journal/` (`journalctl`) | No time limit unless you set a log retention. The installer sets persistent storage and a 1 GB cap (`SystemMaxUse=1G` in `/etc/systemd/journald.conf`); the oldest entries go when it is full. On a quiet server that is many months. |
| fail2ban | Banned IP addresses, the jail and the time; its database also keeps the log lines that led to each ban | `/var/log/fail2ban.log` and rotated files; `/var/lib/fail2ban/fail2ban.sqlite3` | Ubuntu's rotation for the log file (weekly, 4 old files) and fail2ban's own purge age for the database (`dbpurgeage` in `/etc/fail2ban/fail2ban.conf`). A log retention applies to the log file, not to the database. |
| Rspamd (spam filter) | The sending IP address and the sender and recipient addresses of each message it checks | `/var/log/rspamd/` (set by Rspamd's package, not by the panel) | The package's own rotation. |
| Container logs | What each Docker app and one-click install writes to its output | `/var/lib/docker/containers/<id>/<id>-json.log` | 10 MB per file, 3 files per container, for every container the panel starts. |
| Task logs | See `tasks` under [The panel's database](#the-panels-database). | | 30 days. |

If rsyslog is installed (Ubuntu Server images usually include it; check with `ls /var/log/mail.log /var/log/auth.log`), the journal's mail, SSH and system lines are also written as text to `/var/log/mail.log`, `/var/log/auth.log`, `/var/log/syslog` and the other files `/etc/logrotate.d/rsyslog` names, rotated by Ubuntu's own rules there (weekly, 4 old files, so about four weeks). The firewall's lines, with the source address of each connection it logs, go to `/var/log/ufw.log`. A log retention set under Settings > Privacy applies to these files too, and the request tool searches them but does not change them (see below). If you do not need them, remove rsyslog (`apt purge rsyslog`); the journal keeps the same lines.

MariaDB and PostgreSQL write their own server logs to `/var/log/mysql/` and `/var/log/postgresql/`, Rspamd to `/var/log/rspamd/`, each rotated by its package. The panel does not change how long these are kept.

fail2ban's jails here (SSH, Postfix login, Dovecot) read the journal. None reads the nginx logs.

### What you host

Your apps (`/srv/apps`), their databases in MariaDB and PostgreSQL, the files in Drive (`/srv/drive`), the data volumes of Docker apps and one-click installs (n8n, Qdrant, pgAdmin, phpMyAdmin), and the sites you serve are yours. The panel runs them but does not know what personal data they hold, does not apply the privacy settings to them (apart from the nginx logs in front of them), and the request tool does not search them. They are in the backups.

### Backups

Backups are made with restic when you start one (Backups > Run backup now, or the `backups.run` MCP tool). The panel does not run them on a schedule by itself.

| | |
|---|---|
| What a snapshot holds | `/etc/nginx/sites-available`, `/etc/systemd/system`, `/etc/vps-panel`, `/etc/postfix`, `/etc/dovecot`, `/etc/rspamd`, `/etc/powerdns`, `/var/lib/vps-panel` (panel.db and the Files trash, so also deleted mailboxes still in it), `/var/vmail` (all mail), `/srv/apps`, `/srv/drive`, Docker's volumes (`/var/lib/docker/volumes`, when Docker is installed), and fresh dumps of every MariaDB and PostgreSQL database and of panel.db. Left out: `node_modules`, `.next/cache` and build snapshots. **Logs are not in snapshots.** |
| Where | The restic repository `/var/backups/vps-panel` (`RESTIC_REPOSITORY` in `/etc/vps-panel/.env`), on the same server. |
| Encryption | Every snapshot is encrypted by restic with one password, stored in `/etc/vps-panel/.env` (`RESTIC_PASSWORD`) and `/etc/vps-panel/backup.key`. |
| Kept | After each backup, restic keeps the newest snapshot of each of the last 7 days, 4 weeks and 6 months that have one, and deletes the rest (`forget --keep-daily 7 --keep-weekly 4 --keep-monthly 6 --prune`). With a backup every day, the oldest snapshot is about six months old. |

Three things follow from how this works:

- Old snapshots are only removed when a new backup runs. If backups stop, the last snapshots stay until you delete them.
- restic counts days, weeks and months that have a snapshot. With irregular backups, a snapshot can be older than six months and still be one of the six "monthly" ones.
- The database dumps are also left on disk in plain text, readable by root only, in `/var/backups/vps-panel-db-dumps/` (`mariadb/`, `postgres/`, `panel/`), until the next backup replaces them. A restored snapshot stays in `/srv/drive/restores/` until you delete it.

### Retention at a glance

"With N days" is what changes when a log retention is set under Settings > Privacy.

| Data | Default | With N days |
|---|---|---|
| nginx access and error logs | about 15 days | N days, also when N is longer than 15 |
| System journal (mail, SSH, services) | no time limit, 1 GB cap | N days; the 1 GB cap stays |
| rsyslog's files (syslog, auth.log, mail.log and the rest), ufw.log, fail2ban.log, where installed | about four weeks | N days, also when N is longer |
| Audit log | no limit | N days |
| Sign-in attempts | 7 days failed, 30 days successful | the shorter of those and N days |
| Task history | 30 days | the shorter of 30 and N days |
| Sessions | 1 day after expiry | unchanged |
| Terminal history | last 100 commands per folder | unchanged |
| fail2ban's database, Rspamd's log, MariaDB's and PostgreSQL's logs, container logs | their package defaults | unchanged |
| Mail, mailboxes, forwarders, webmail data | until deleted | unchanged |
| A deleted mailbox in the Files trash | 30 days | unchanged |
| Backups | up to about six months with daily backups | unchanged |

## What leaves the server

| To | What is sent | When | Can be switched off |
|---|---|---|---|
| Let's Encrypt (`acme-v02.api.letsencrypt.org`) | The account's contact email: Settings > DNS & SSL > SSL contact email, or else `ACME_EMAIL` in `/etc/vps-panel/.env`, which the installer sets to the admin email. The domain names on each certificate. Let's Encrypt then connects to the server over HTTP to check them. Every certificate, with its names, is published in the public Certificate Transparency logs. | When a certificate is ordered or renewed; renewal runs by itself. | Only by not using certificates. Set a role address (such as `hostmaster@`) as the contact email. |
| GitHub, or wherever the panel's source clone points (`git -C /root/vps-panel-src remote get-url origin`) | A `git fetch`: the server's IP address and nothing about your data | Each time the panel is opened in a browser (the update check in the top bar) and on every update | Point the clone at another remote. |
| Package sources: Ubuntu's mirrors, `deb.nodesource.com`, `rspamd.com`, `sh.rustup.rs`, crates.io and the npm registry | Download requests (the server's IP address) | At install and on every update; Ubuntu's mirrors also daily for security updates | No |
| Next.js telemetry (`telemetry.nextjs.org`, run by Vercel) | Nothing. The installer and the updater build the panel with `NEXT_TELEMETRY_DISABLED=1`, and the panel's service unit runs it with the same setting. Earlier versions sent anonymous build information with each build at install and on every update (tool version, operating system, CPU and memory size, page counts, build duration, with a random ID). Apps you deploy that use Next.js make their own choice. | Never | Already off |
| Public DNS resolvers 1.1.1.1 (Cloudflare), 8.8.8.8 (Google), 9.9.9.9 (Quad9) | The names being checked: your domains, their mail and DKIM/DMARC names, the reverse name of the server's addresses | Before a certificate is ordered, and on the mail DNS and mail IP checks | No |
| ipinfo.io, and GeoJS (`get.geojs.io`) when ipinfo.io does not answer | The IP address of each server in your My Networks networks, this one included, in the request URL. Nothing about visitors or users. | When the My Networks page is shown. A found location is cached for 24 hours, a failed lookup for 10 minutes, in memory. | Yes: Settings > Privacy, outside lookups. |
| api.ipify.org | An empty request; it answers with the server's public IPv4 address | Only when the panel's hostname does not resolve to a public IPv4 address: on the Dashboard, Apps, Databases and MCP pages and when a domain is added; cached 5 minutes | No (not covered by the outside-lookups switch). Make the panel hostname's A record point at the server. |
| Telegram (`api.telegram.org`), if you add a Telegram channel | The bot token, the user ID and the message: the subject and text of the event. For a failed task that includes its last log lines; for a failed service the last 20 lines of that service's journal, which for Postfix or Dovecot name addresses and IP addresses. | When an event the channel subscribes to happens | Remove the channel, or subscribe it to fewer events. |
| Webhook URLs, if you add one | The same subject and text, as JSON | Same | Same |
| Notification email | The same text, sent by the server's Postfix to the address you set | Same | Same |
| The other servers in your My Networks networks | This server's name, URL, uptime, load, CPU count and memory and disk use, signed | Every 10 seconds, to each member | Leave or delete the network. |
| Tailscale, if installed | Tailscale's own coordination service learns this server's name and addresses, as with any Tailscale device. The packages come from `pkgs.tailscale.com`. | While Tailscale runs | Uninstall Tailscale. |
| Your apps' sources and registries | `git fetch` to each app's repository, downloads from the npm registry, `docker pull` and image update checks against Docker registries | On deploy and update, and when the Apps page checks for updates (on each load, unless switched off there) | The page-load check can be switched off on the Apps page. |
| Other mail servers | The mail your users send, with its headers | When they send it | That is what a mail server does. |
| A remote backup store, if you set `RESTIC_REPOSITORY` to one (`sftp:`, `s3:`, `b2:` and similar) | The encrypted snapshots | On each backup | It is local (`/var/backups/vps-panel`) unless you change it. |

Nothing else leaves the server on the panel's initiative. In particular:

- No analytics, no usage statistics, no crash reports, nothing to the panel's authors.
- The panel's pages load scripts, styles, fonts and images only from the panel itself (its Content-Security-Policy is `default-src 'self'`), so a visit to the panel tells no third party anything.
- Backups stay on the server unless you configure a remote repository or copy them yourself.

## Privacy settings

Under **Settings > Privacy**. Each is a row in panel.db's `settings` table; when you save, the panel stores it and asks the agent to bring the server in line. If that last step fails, the page says "saved, but not applied" and why. The agent also applies the settings when it starts and once a day after that, which puts the log format back after a package upgrade replaced `/etc/nginx/nginx.conf`. The outside-lookups setting is the panel's alone and needs no agent.

| Setting | Key | Values | Default |
|---|---|---|---|
| IP addresses in web server logs | `privacy.log_ip` | `full`, `masked` | `full` |
| Query strings in web server logs | `privacy.log_query` | `keep`, `strip` | `keep` |
| Keep logs for | `privacy.retention_days` | empty (no limit), or 7 to 3650 days | empty |
| Outside lookups (server locations) | `privacy.server_geo` | `on`, `off` | `on` |

The tab shows, for the web server's access log, the system logs (the journal and the text logs next to it in `/var/log`) and the web server's log files, what the settings ask for ("wanted"), what the server does now ("actual"), and, when the two differ, why and what to do. Read that, not this page, to know what holds on your server right now.

**Masked IP addresses.** nginx writes visitors' addresses shortened: an IPv4 address with its last number set to 0 (203.0.113.87 becomes 203.0.113.0), an IPv6 address cut to its first three groups (2001:db8:85a3:8d3::7 becomes 2001:db8:85a3::). An IPv4 address that reaches nginx written as IPv6 (`::ffff:203.0.113.87`) is logged as `::`, and anything nginx prints that is not an address it recognises is logged as `0.0.0.0`: more coarsely, never in full. This covers the access log of every site, app and the panel, since they share one file. Lines already written stay as they are until rotation or the log retention removes them. Bans are not affected: fail2ban reads the journal, not these logs, and the panel's sign-in limits use the address of the live connection, not the log. The sites and apps behind nginx still receive the visitor's full address and may log it themselves.

**Stripped query strings.** nginx logs the address asked for and the referrer without the part after `?`, where search terms, email addresses in links and tokens tend to be. Again lines written before stay as they were.

How the two settings reach nginx: while either is on, the `access_log` line of the http block in `/etc/nginx/nginx.conf` is commented out with `#vps-panel-privacy:`, and the panel's own file `/etc/nginx/conf.d/vps-panel-privacy.conf` declares the same log again in the format `vps_panel_privacy`. Lines already in the logs are not rewritten. Turning both settings off removes the mark and the file, and nginx.conf is again what it was. The change is tested with `nginx -t` and put back if nginx refuses it. Not covered:

- **nginx's error log.** nginx cannot change its format, so it keeps full visitor addresses and full request lines. It is rotated and deleted together with the access log.
- **A vhost you wrote by hand with its own `access_log`** (in a `server` or `location` block), and an `access_log` in nginx.conf in a format of your own. The panel does not rewrite them, so those requests are still logged in full; the status names each one with its file and line. Add the format name (`access_log <path> vps_panel_privacy;`) or remove the line, then reload nginx.

**Keep logs for N days.** A value lower than the current one deletes data at once, so the page (and the `privacy.set` MCP tool) asks for a confirmation first. The setting is applied when you save it, when the agent and the panel start, and once a day after that:

- **The system journal.** The panel writes `/etc/systemd/journald.conf.d/90-vps-panel-retention.conf`, which makes journald start a new journal file every N/30 days, rounded up (every day for a limit of up to 30 days), and delete a file once its first entry is older than N days less that span. No entry is kept longer than N days. journald deletes whole files, never single entries, so the first time a limit is set or lowered, a journal file that holds any entry past the limit is deleted whole, with the newer entries in it; on a quiet server that can be most of the journal. The 1 GB cap from journald.conf still applies.
- **Log files that logrotate turns over:** nginx's access and error logs (`/etc/logrotate.d/nginx`) and, where the package is installed, rsyslog's files (`/etc/logrotate.d/rsyslog`: syslog, auth.log, mail.log and the rest), ufw.log (`/etc/logrotate.d/ufw`) and fail2ban.log (`/etc/logrotate.d/fail2ban`). The package's file is not edited: the panel puts a copy in front of it (`/etc/logrotate.d/0-vps-panel-nginx`, `0-vps-panel-rsyslog`, `0-vps-panel-ufw`, `0-vps-panel-fail2ban`) with `rotate` set so that no file holds lines older than N days, `maxage` N and `ignoreduplicates`, so that logrotate uses the copy and passes over the package's file. Rotated files already past the limit are deleted when the setting is applied and in the daily run, also those of a log too quiet to be rotated. N is also how long these files are kept when it is longer than the package's own rotation. A package file the panel does not rewrite (one that uses `size`, `dateext` or `olddir`, for example) is left as it is, and the status says so.
- **In the panel:** housekeeping deletes audit log entries, sign-in records and finished tasks older than N days, right away when you save and once a day. Sign-in records and task history never stay longer than 30 days, whatever is chosen (failed sign-ins 7 days, successful ones and finished tasks 30). With N under 30, a successful sign-in from an address also counts for only N days, instead of 30, when the sign-in limiter decides whether that address is one of the account's own.

Clearing the setting removes the journald drop-in and the logrotate copies; the system's own rotation applies again.

Not covered by the log retention: the terminal history, fail2ban's database, Rspamd's log, the MariaDB and PostgreSQL server logs, wtmp, btmp and lastlog, container logs, your apps' own logs, web logs a hand-written vhost writes to files `/etc/logrotate.d/nginx` does not name (Ubuntu's names `/var/log/nginx/*.log`), mail, backups and whatever your apps store.

**Outside lookups off.** The My Networks page makes no request to ipinfo.io or GeoJS (`get.geojs.io`) and shows no locations, also none found earlier. The server's own-address lookup at api.ipify.org is not part of this switch (see [What leaves the server](#what-leaves-the-server)).

## Requests about one person

Under Settings > Privacy, for a request about one person ("what do you hold about me", "delete it"). Enter one email address or one IP address. Only admins can use it, and it is also available to AI clients with a full-access MCP token.

**Search** reads, without changing anything, the sources below. Each has an id, which the MCP tools and the downloaded report use.

| Source (id) | What is searched | Erase |
|---|---|---|
| Web server access log (`nginx_access`) | Every `.log` file in `/var/log/nginx` whose name does not contain "error", the current ones and the rotated ones, `.gz` included. A vhost that logs somewhere else is not searched. | Yes: the value is replaced in each line |
| Web server error log (`nginx_error`) | nginx's error logs in `/var/log/nginx`, current and rotated | Yes: the value is replaced in each line |
| Mail server log (`journal_mail`) | Postfix and Dovecot lines in the system journal | No |
| SSH log (`journal_ssh`), for an IP address | sshd lines in the system journal. For an email address they are counted under `journal_other`. | No |
| Other services (`journal_other`) | Every other line in the system journal: the firewall, apps, the panel itself | No |
| Other log files in /var/log (`system_logs`) | syslog, auth.log, mail.log, mail.err, mail.warn, mail.info, kern.log, ufw.log, fail2ban.log, daemon.log, user.log, messages and debug in `/var/log`, and the logs in `/var/log/rspamd`, `/var/log/mysql` and `/var/log/postgresql`, with their rotated files | No |
| fail2ban bans (`fail2ban`), for an IP address | The bans fail2ban holds now, and its database `/var/lib/fail2ban/fail2ban.sqlite3` | No |
| Mailbox (`mailboxes`), for an email address | A mailbox with this address (`mail.virtual_users`) | No |
| Forwarders (`forwarders`), for an email address | Forwarders that have the address as their source or as a destination | No |
| Stored mail (`maildir`), for an email address | Whether the mailbox's folder `/var/vmail/<domain>/<name>/` exists, with the number of files and their size. The mail itself is not read. | No |
| Mail of a deleted mailbox (`mail_trash`), for an email address | Files > Trash entries that hold this address's mail folder | No |
| Mail queue (`mail_queue`), for an email address | Messages from or to the address that Postfix has not delivered yet | No |
| Task logs (`task_logs`) | The logs of the panel's tasks (deploys, installs, certificates) in panel.db | Yes, in the logs of finished tasks |
| Panel account (`panel_users`), for an email address | The panel account that signs in with this address | No |
| Panel sign-in records (`panel_logins`) | Sign-in attempts with this email address, or from this IP address | Yes: the records are deleted |
| Panel audit log (`panel_audit`) | Audit log entries whose target or message contains the value | Yes: the value is replaced in each entry |

A match is the value itself, not part of a longer one: 203.0.113.8 is not found inside 203.0.113.87, and ann@example.com is not found inside joann@example.com. An email address is found in any letter case and also percent-encoded, as it appears in a URL. An IPv6 address is found in any of its spellings, and an IPv4 address written as IPv6 (`::ffff:203.0.113.5`) is searched for as that IPv4 address. While masking is on, the web server logs hold the masked form of new visitors' addresses, so a search for the full address finds only the lines written before.

For each source it shows how many records match and up to 20 of them, newest first, each cut at 500 characters, and a note on what the source is and what happens to it. The search is limited: each source stops after 45 seconds and all of them after 60; a log file source reads the newest 400 files and at most 4 GB of text, the journal at most 200,000 lines, the task logs at most 512 MB; fail2ban, the mail queue and the size of a mail folder get 10 seconds. Files compressed with anything other than gzip are not read. A source that reached a limit or could not be read says so. **Download as JSON** saves this report as a file. It holds the counts, the notes and the examples, not every record: for a full copy of a large source, read it yourself, for example `zgrep -F '203.0.113.87' /var/log/nginx/access.log*` or `journalctl -o short-iso --grep 'alice@example\.com'`.

**Erase** works on the sources you tick that are erasable:

- **nginx's access and error logs** (`nginx_access`, `nginx_error`): the value is replaced inside each line and the rest of the line stays: an IP address becomes its masked form (203.0.113.0), an email address becomes `[erased]`. Each file that holds it is rewritten, a `.gz` unpacked and packed again; the log nginx is writing to is swapped for the corrected copy and nginx is told to reopen it, so no line written meanwhile is lost. One erase runs at a time. It starts no new file after 120 seconds and gives up on the file it is rewriting after 240 seconds, which then stays exactly as it was; run the erase again to go on.
- **Task logs** (`task_logs`): the same replacement in the logs of finished tasks. A task that is still running is left alone; erase again when it has finished.
- **Panel sign-in records** (`panel_logins`): the records are deleted. That includes the address's recent failed attempts, so the sign-in limiter no longer counts them. Two-factor code attempts are recorded under `totp:<email>`; a search by email address or by IP address finds them too.
- **Panel audit log** (`panel_audit`): the value is replaced inside the entries (an IP address by its masked form, an email address by `[erased]`), and the entries stay.

An IP address that is already in its masked form has nothing to be replaced with: log lines, task logs and audit entries that hold it stay as they are. Every search and every erase is written to the audit log (`privacy.subject.search`, `privacy.subject.erase`) with the kind of value and the first 12 hexadecimal characters of a keyed hash of it (HMAC-SHA-256 with a key derived from this server's `JWT_SECRET`), not the value. Without that secret the value cannot be worked out from the audit log; two requests about the same value show the same characters, so they can be told to belong together. The agent's line about the erase in the journal holds the kind and the number of records changed, not the value.

What the tool does not do, and where you have to act yourself:

- **It never deletes a panel account, a mailbox or a forwarder, or touches the mail in a mailbox.** Delete the account on the Users page, the mailbox and its forwarders on the Mailboxes page. A deleted mailbox's mail stays in Files > Trash for 30 days unless you delete the entry there or empty the trash.
- **The system journal cannot be edited** (`journal_mail`, `journal_ssh`, `journal_other`). Matching lines go when the log retention removes them; without a retention, when the journal reaches its 1 GB cap, which can take many months.
- **The text logs in /var/log** (`system_logs`) are not rewritten. logrotate deletes them on its schedule: after about four weeks on Ubuntu, after N days for rsyslog's files, ufw.log and fail2ban.log with a log retention set. Rspamd's, MariaDB's and PostgreSQL's logs follow their package's rotation.
- **fail2ban bans** are a security measure and are not lifted by an erase: unban the address on the Firewall page if it should go. fail2ban drops a ban from its database by itself, one day after it was set unless `dbpurgeage` says otherwise.
- **The mail queue**: messages leave it when they are delivered or returned, after five days at the latest.
- **Backups.** Snapshots already made keep what they hold until they expire: with the default retention, up to about six months after the last backup that contains it (see [Backups](#backups)). The plain-text dumps in `/var/backups/vps-panel-db-dumps/` keep it until the next backup.
- **What the tool does not search:** your apps' databases and files, Drive files, Docker volumes and container logs; webmail's database (Roundcube's address books, identities and settings); Rspamd's history of checked messages; wtmp, btmp and lastlog; mail in other mailboxes that was sent by or to the address; the terminal history; and configuration you typed in yourself, such as firewall rules, the trusted IP list (`panel.ip_allowlist`), notification channels and the SSL contact email.
- **Mail already sent** to other servers, and copies people keep in their own mail clients.
- **Other servers and services**: the other servers in your networks, Let's Encrypt, Telegram, webhook receivers, your backup store if it is remote.

## Cookies

The panel sets two cookies, both needed to sign in, and nothing else: no tracking, no analytics, no local storage.

| Cookie | What it is | Path | Lifetime | Flags |
|---|---|---|---|---|
| `vps_panel_session` | The signed sign-in token: account ID, email address and role | `/` | The session timeout (Settings > Account > Session security: 60 minutes unless changed, from 1 minute to 30 days), renewed while you use the panel; 30 days when "Remember me" was ticked | `HttpOnly`, `SameSite=Strict`, `Secure` once the panel has a certificate |
| `vps_panel_refresh` | Renews the sign-in token | `/api/auth/refresh` | Same as above | Same as above |

`Secure` follows `COOKIE_SECURE` in `/etc/vps-panel/.env`: the installer starts with `0`, because the panel is first reached over plain HTTP, and the panel sets it to `1` once its own certificate is in place.

Both cookies are strictly necessary for a service the user asked for, so the panel itself needs no consent banner. Webmail (Roundcube), phpMyAdmin and pgAdmin, if installed, set their own session cookies on their own addresses. The sites and apps you host are your own matter.

## What to set if you are in the EU

These are recommendations from how the panel works. They are not legal advice.

1. **Mask IP addresses and strip query strings** in Settings > Privacy. Full visitor addresses are rarely needed in web logs, and nothing on the server depends on them: bans come from the journal.
2. **Set a log retention** you can justify, for example 30 days. It also limits the audit log, so pick a number that leaves you the audit trail you need. For the log files logrotate turns over it is the time they are kept, not only a ceiling: a number over 15 keeps nginx's logs longer than Ubuntu's rotation would.
3. **Switch outside lookups off** unless you use the server locations on My Networks. They send your servers' addresses (not your visitors') to ipinfo.io or GeoJS.
4. **Use a role address as the SSL contact email** (Settings > DNS & SSL), not a person's.
5. **Keep personal data out of notifications** to outside services. Telegram and webhook messages carry log lines; email to a mailbox on your own server keeps them here.
6. **Keep personal data out of terminal commands.** They are kept word for word in the audit log and the terminal history.
7. **Write the backup window into your policy**: data deleted on the server stays in snapshots for up to about six months. If you copy the repository elsewhere, record where.
8. **Have a data processing agreement with your hosting provider**: it runs the machine all of this is on.
9. **Use the tables on this page for your record of processing**, and add what your own apps and sites hold. Their visitors' web logs are covered by the settings above; their own data and cookies are not.
10. **For a request about one person**, use the request tool, then the places listed under [Requests about one person](#requests-about-one-person) that it cannot reach.
