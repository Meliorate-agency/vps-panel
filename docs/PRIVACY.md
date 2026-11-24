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
| Sign-in attempts: the IP address, the email address typed (also one that has no account), the time, and whether it succeeded. Two-factor code attempts are recorded the same way, under `totp:<email>`. | `login_attempts` | Failed attempts 7 days, successful ones 30 days (housekeeping). |
| Audit log: time, account, action, target, result and a message. Targets include email addresses typed at a failed sign-in (also ones with no account), mailbox and forwarder addresses, and an old address when an account's email is changed. Calls from AI clients (MCP) record the client's IP address. **Every command run in the web terminal is recorded word for word** (up to 2,000 characters) with the folder it ran in, before it runs. | `audit_log` | No limit. With a log retention set: that many days. |
| Terminal history: per account and per folder, the last 100 commands, word for word | `terminal_history` | Until 100 newer commands in that folder push it out, or the account is deleted. No time limit. |
| Task history: the kind of task, its status, times, and its whole log (build output, backup output, certificate orders) | `tasks` | 30 days from the start of the task. The agent deletes older finished tasks when it starts and every 24 hours. With a shorter log retention: that many days. |
| Notification channels: email addresses, webhook URLs, Telegram user IDs and bot tokens | `notifications` | Until the channel is removed. |
| Settings that name people: the SSL contact email (`panel.acme_email`), the sender of notification email (`notifications.email_from`), trusted IP addresses (`panel.ip_allowlist`), the panel access list | `settings` | Until changed. |
| My Networks: the name, URL and public key of every server in your networks, when each was last heard from, and its uptime, load, memory and disk use | `networks`, `network_members`, `network_invites` | Until the network is deleted or the server leaves it. The final member list of a network this server deleted (`networks_dissolved`): 90 days. |

The panel keeps nothing about you in the browser beyond the two sign-in cookies (see [Cookies](#cookies)).

### Mail

| Data | Where | Kept |
|---|---|---|
| Mailboxes: address, password hash (SHA512-CRYPT), quota | MariaDB, database `mail`, table `virtual_users` | Until the mailbox is deleted on the Mailboxes page. |
| Forwarders and catch-alls: the address and where its mail goes | `mail.virtual_aliases` | Until the forwarder is deleted. Deleting a mailbox does not delete forwarders that name its address; remove those under Forwarders on the Mailboxes page. |
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
| Web server error log | Errors with the client's IP address, the request line and the site name | `/var/log/nginx/error.log` and its rotated files | Same rotation as the access log. |
| System journal | Postfix: sender and recipient addresses, the name and IP address of every server or client that connects, mailbox login names. Dovecot: every mailbox login with its IP address, and the address tried with a wrong password. sshd: user names and IP addresses of logins and failed attempts. The output of your Node apps. The panel and the agent (the agent records the address of a mailbox it deletes). | `/var/log/journal/` (`journalctl`) | No time limit. The installer sets persistent storage and a 1 GB cap (`SystemMaxUse=1G` in `/etc/systemd/journald.conf`); the oldest entries go when it is full. On a quiet server that is many months. |
| fail2ban | Banned IP addresses, the jail and the time; its database also keeps the log lines that led to each ban | `/var/log/fail2ban.log` and rotated files; `/var/lib/fail2ban/fail2ban.sqlite3` | Ubuntu's rotation for the log file and fail2ban's own purge age for the database (`dbpurgeage` in `/etc/fail2ban/fail2ban.conf`); the panel changes neither. |
| Rspamd (spam filter) | The sending IP address and the sender and recipient addresses of each message it checks | `/var/log/rspamd/` (set by Rspamd's package, not by the panel) | The package's own rotation. |
| Container logs | What each Docker app and one-click install writes to its output | `/var/lib/docker/containers/<id>/<id>-json.log` | 10 MB per file, 3 files per container, for every container the panel starts. |
| Task logs | See `tasks` under [The panel's database](#the-panels-database). | | 30 days. |

If rsyslog is installed (Ubuntu Server images usually include it; check with `ls /var/log/mail.log /var/log/auth.log`), the journal's mail, SSH and system lines are also written as text to `/var/log/mail.log`, `/var/log/auth.log` and `/var/log/syslog`, rotated by Ubuntu's own rules in `/etc/logrotate.d/rsyslog`. The panel does not manage these files: neither the log retention setting nor the request tool covers them. If you do not need them, remove rsyslog (`apt purge rsyslog`); the journal keeps the same lines.

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
| nginx access and error logs | about 15 days | N days |
| System journal (mail, SSH, services) | no time limit, 1 GB cap | N days |
| Audit log | no limit | N days |
| Sign-in attempts | 7 days failed, 30 days successful | the shorter of those and N days |
| Task history | 30 days | the shorter of 30 and N days |
| Sessions | 1 day after expiry | unchanged |
| Terminal history | last 100 commands per folder | unchanged |
| fail2ban log and database, Rspamd log, rsyslog files, container logs | their package defaults | unchanged |
| Mail, mailboxes, forwarders, webmail data | until deleted | unchanged |
| A deleted mailbox in the Files trash | 30 days | unchanged |
| Backups | up to about six months with daily backups | unchanged |

## What leaves the server

| To | What is sent | When | Can be switched off |
|---|---|---|---|
| Let's Encrypt (`acme-v02.api.letsencrypt.org`) | The account's contact email: Settings > DNS & SSL > SSL contact email, or else `ACME_EMAIL` in `/etc/vps-panel/.env`, which the installer sets to the admin email. The domain names on each certificate. Let's Encrypt then connects to the server over HTTP to check them. Every certificate, with its names, is published in the public Certificate Transparency logs. | When a certificate is ordered or renewed; renewal runs by itself. | Only by not using certificates. Set a role address (such as `hostmaster@`) as the contact email. |
| GitHub, or wherever the panel's source clone points (`git -C /root/vps-panel-src remote get-url origin`) | A `git fetch`: the server's IP address and nothing about your data | Each time the panel is opened in a browser (the update check in the top bar) and on every update | Point the clone at another remote. |
| Package sources: Ubuntu's mirrors, `deb.nodesource.com`, `rspamd.com`, `sh.rustup.rs`, crates.io and the npm registry | Download requests (the server's IP address) | At install and on every update; Ubuntu's mirrors also daily for security updates | No |
| Next.js telemetry (`telemetry.nextjs.org`, run by Vercel) | Nothing since this version: the panel is built and run with `NEXT_TELEMETRY_DISABLED=1`. Before, each build at install and on every update sent anonymous build information (tool version, operating system, CPU and memory size, page counts, build duration, with a random ID). Apps you deploy that use Next.js make their own choice. | Never | Already off |
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

Under **Settings > Privacy**. Each is a row in panel.db's `settings` table; when you save, the panel stores it and asks the agent to bring the server in line. If that last step fails, the page says "saved, but not applied" and why. The agent also applies the settings when it starts.

| Setting | Key | Values | Default |
|---|---|---|---|
| IP addresses in web server logs | `privacy.log_ip` | `full`, `masked` | `full` |
| Query strings in web server logs | `privacy.log_query` | `keep`, `strip` | `keep` |
| Keep logs for | `privacy.retention_days` | empty (no limit), or 7 to 3650 days | empty |
| Outside lookups (server locations) | `privacy.server_geo` | `on`, `off` | `on` |

The tab shows, for the web server logs, the system journal and the nginx log files, what the settings ask for ("wanted"), what the server does now ("actual"), and, when the two differ, why and what to do. Read that, not this page, to know what holds on your server right now.

**Masked IP addresses.** nginx writes visitors' addresses shortened: an IPv4 address with its last number set to 0 (203.0.113.87 becomes 203.0.113.0), an IPv6 address cut to its first three groups (2001:db8:85a3:8d3::7 becomes 2001:db8:85a3::). This covers the access log of every site, app and the panel, since they share one file. Lines already written stay as they are until rotation or the log retention removes them. Bans are not affected: fail2ban reads the journal, not these logs, and the panel's sign-in limits use the address of the live connection, not the log.

**Stripped query strings.** nginx logs the address asked for and the referrer without the part after `?`, where search terms, email addresses in links and tokens tend to be. Again lines written before stay as they were.

**Keep logs for N days.** On the server: entries in the system journal (mail, SSH and service logs) and nginx's rotated log files older than N days are deleted. In the panel: the daily housekeeping also deletes audit log entries, sign-in attempts and task history older than N days; the shorter limits these already have stay. The journal removes whole files rather than single lines, so an entry can stay somewhat longer than N days. Not covered: terminal history, the fail2ban log and database, Rspamd's log, rsyslog's text files, container logs, mail, backups and whatever your apps store.

**Outside lookups off.** The My Networks page makes no request to ipinfo.io or GeoJS and shows no locations.

## Requests about one person

Under Settings > Privacy, for a request about one person ("what do you hold about me", "delete it"). Enter one email address or one IP address. Only admins can use it, and it is also available to AI clients with a full-access MCP token.

**Search** reads, without changing anything:

- in panel.db: the panel account with that email, its sign-in attempts (by email or IP address), and audit log entries whose target or message contains the value;
- on the server: the nginx access logs, the system journal (mail and SSH lines), fail2ban, and for an email address the mailbox, forwarders and mail folder that belong to it.

For each source it shows how many records match and up to 20 of them, newest first. The search is limited in time and output; a source that could not be read says so. **Export** downloads this report as a JSON file. The export holds the counts and the examples, not every record: for a full copy of a large source, read it yourself, for example `zgrep -F '203.0.113.87' /var/log/nginx/access.log*` or `journalctl -o short-iso --grep 'alice@example\.com'`.

**Erase** works on the sources you tick that are marked erasable. It replaces the value in place rather than deleting whole files: an IP address becomes its masked form (203.0.113.0), an email address becomes `[erased]`. In panel.db, sign-in attempts for the person are deleted, and in the audit log the value is replaced inside the entries, which stay. The erase is itself written to the audit log, with the kind of value and the first characters of its SHA-256 hash, not the value.

What the tool does not do, and where you have to act yourself:

- **It never deletes a panel account or a mailbox, or touches the mail in it.** Delete the account on the Users page, the mailbox and its forwarders on the Mailboxes page. A deleted mailbox's mail stays in Files > Trash for 30 days unless you empty it there.
- **The system journal cannot be edited.** Matching lines go when the log retention removes them; without a retention, when the journal reaches its 1 GB cap, which can take many months.
- **Backups.** Snapshots already made keep what they hold until they expire: with the default retention, up to about six months after the last backup that contains it (see [Backups](#backups)). The plain-text dumps in `/var/backups/vps-panel-db-dumps/` keep it until the next backup.
- **Your apps' databases and files**, Drive files, Docker volumes, and webmail's address books and settings: the tool does not search them.
- **Mail already sent** to other servers, and copies people keep in their own mail clients.
- **Other servers and services**: the other servers in your networks, Let's Encrypt, Telegram, webhook receivers, your backup store if it is remote.
- **Files the panel does not manage**: rsyslog's text logs, Rspamd's log, container logs.

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
2. **Set a log retention** you can justify, for example 30 days. It also limits the audit log, so pick a number that leaves you the audit trail you need.
3. **Switch outside lookups off** unless you use the server locations on My Networks. They send your servers' addresses (not your visitors') to ipinfo.io or GeoJS.
4. **Use a role address as the SSL contact email** (Settings > DNS & SSL), not a person's.
5. **Keep personal data out of notifications** to outside services. Telegram and webhook messages carry log lines; email to a mailbox on your own server keeps them here.
6. **Keep personal data out of terminal commands.** They are kept word for word in the audit log and the terminal history.
7. **Write the backup window into your policy**: data deleted on the server stays in snapshots for up to about six months. If you copy the repository elsewhere, record where.
8. **Have a data processing agreement with your hosting provider**: it runs the machine all of this is on.
9. **Use the tables on this page for your record of processing**, and add what your own apps and sites hold. Their visitors' web logs are covered by the settings above; their own data and cookies are not.
10. **For a request about one person**, use the request tool, then the places listed under [Requests about one person](#requests-about-one-person) that it cannot reach.
