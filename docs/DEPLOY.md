# Deploy

The panel is designed to install onto a fresh Ubuntu 24.04 LTS server and bring up everything (nginx, MariaDB, PowerDNS, Postfix, Dovecot, Rspamd, fail2ban, UFW, restic, Node, Rust toolchain, the panel itself, the agent itself) in one shot.

## Requirements

- Fresh Ubuntu 24.04 LTS (server edition).
- 2 GB RAM minimum (build of the Rust agent and the Next.js standalone bundle is the peak).
- 20 GB disk minimum.
- A public IPv4 address.
- A domain you control. Pick a subdomain for the panel (e.g. `panel.example.com`).

## First install

```bash
ssh root@your-server
git clone https://github.com/Meliorate-agency/vps-panel.git /root/vps-panel-src
bash /root/vps-panel-src/scripts/install.sh --domain panel.example.com --email you@example.com
```

Step by step, including IP-only installs and private forks: [FIRST_INSTALL.md](../FIRST_INSTALL.md). The clone stays at `/root/vps-panel-src`: updates pull into it.

The script will:

1. Update apt and install all packages (see [SECURITY.md](SECURITY.md) for what each is hardened to).
2. Create system users `vps-panel`, `apps`, `vmail`.
3. Install rustup (as root), build the agent in release mode, install it to `/usr/local/bin/vps-panel-agent`.
4. Install Node 22 LTS from NodeSource (a server that already has Node 20 or newer keeps it), copy the source to `/opt/vps-panel/app` and run `npm ci && npm run build` there as `vps-panel`.
5. Generate `JWT_SECRET`, `AGENT_TOKEN`, `RESTIC_PASSWORD` and write `/etc/vps-panel/.env`.
6. Initialize MariaDB, create databases `panel`, `pdns`, `mail`, plus their service users.
7. Configure PowerDNS, Postfix, Dovecot, Rspamd to read from MariaDB.
8. Apply hardening (UFW, fail2ban jails, sysctl, unattended-upgrades, chrony). SSH is left permissive on purpose, see [SECURITY.md](SECURITY.md).
9. Install the panel nginx vhost on `panel.example.com` (HTTP first; you will issue HTTPS from the panel UI).
10. Enable + start `vps-panel-agent.service` and `vps-panel.service`.
11. Print the bootstrap admin credentials.

After it exits, point your DNS so `panel.example.com` resolves to the server, open the panel, log in, enroll TOTP, then go to Settings -> Issue cert for panel host.

## Update

The Update button in the panel's top bar, or by hand:

```bash
bash /root/vps-panel-src/scripts/update.sh
```

Pulls, builds the agent and the panel, and only then installs them and restarts both services. If a build fails, the previous panel install is restored and nothing else changes. When the agent build fails, Rust is updated (`rustup update stable`) and the build is tried once more. SQLite migrations run on panel start.

If the new agent or the new panel does not stay up after the restart, or the script is stopped between the build and the restart, the update is rolled back: the previous agent binary, unit files and panel install are put back together, both services are restarted on them, and the script says so and exits non-zero (the update task in the panel is marked failed). Not undone:

- Database migrations. They only add tables and columns, which the previous version ignores.
- What the update changed in the nginx, mail and DNS configuration. Each of those changes is checked when it is made and put back by itself if the check fails.
- The checkout. It stays at the new commit, so the panel shows no update until there is a newer one; running the update again tries the same version again.

The previous version stays on the server until the next update: `/opt/vps-panel/app.prev`, `/usr/local/bin/vps-panel-agent.prev` and the unit files in `/var/backups/vps-panel-config/units.prev`.

Use the Update button or SSH, not the panel's own terminal: there the script is ended by the agent restart it performs, and nothing checks the result or rolls it back.

## Uninstall

```bash
bash /root/vps-panel-src/scripts/uninstall.sh
```

Stops both services, removes systemd units, the panel's nginx vhost, the agent binary and `/opt/vps-panel`; leaves `/var/lib/vps-panel`, `/etc/vps-panel` and the MariaDB databases alone. `--purge` also removes the panel's own state: `/var/lib/vps-panel` (panel.db, the Files trash), its logs, `.env`, the agent token, the panel access list and the `panel` database. Certificates, app env files, the backup key and repository, the `pdns` and `mail` databases, site vhosts and app units stay: nginx, your apps, DNS and mail keep running on them.

## Backups

On the Backups page. The restic repository is `/var/backups/vps-panel`, on the same server: copy it somewhere else yourself until off-server destinations are supported.

## Manual recovery

If the panel is broken and you need to fix it from SSH:

- Panel logs: `journalctl -u vps-panel.service -f`
- Agent logs: `journalctl -u vps-panel-agent.service -f`
- Panel DB: `sqlite3 /var/lib/vps-panel/panel.db` (`apt install sqlite3` first; the installer does not install the CLI)
- Agent socket health: `curl --unix-socket /run/vps-panel/agent.sock http://x/health -H "Authorization: Bearer $(cat /etc/vps-panel/agent.token)"`
- Reset admin password: `sudo /usr/local/bin/vps-panel-agent admin-reset --email you@example.com` (type the new password, Enter, then Ctrl-D; writes new argon2 hash to `panel.db`).
