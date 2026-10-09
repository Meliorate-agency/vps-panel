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
git clone https://github.com/you/vps-panel /opt/vps-panel
cd /opt/vps-panel
./scripts/install.sh --domain panel.example.com --email you@example.com
```

The script will:

1. Update apt and install all packages listed in [ARCHITECTURE.md](../ARCHITECTURE.md) and [SECURITY.md](SECURITY.md).
2. Create system users `vps-panel`, `apps`, `vmail`.
3. Install rustup as `vps-panel`, build the agent in release mode, install it to `/usr/local/bin/vps-panel-agent`.
4. Install Node 20 LTS from NodeSource, run `npm ci && npm run build`, install the standalone output to `/opt/vps-panel/.next/standalone`.
5. Generate `JWT_SECRET`, `AGENT_TOKEN`, `RESTIC_PASSWORD` and write `/etc/vps-panel/.env`.
6. Initialize MariaDB, create databases `panel`, `pdns`, `mail`, plus their service users.
7. Configure PowerDNS, Postfix, Dovecot, Rspamd to read from MariaDB.
8. Apply hardening (SSH lockdown, sysctl, AppArmor checks, unattended-upgrades, fail2ban jails).
9. Install the panel nginx vhost on `panel.example.com` (HTTP first; you will issue HTTPS from the panel UI).
10. Enable + start `vps-panel-agent.service` and `vps-panel.service`.
11. Print the bootstrap admin credentials.

After it exits, point your DNS so `panel.example.com` resolves to the server, open the panel, log in, enroll TOTP, then go to Settings -> Issue cert for panel host.

## Update

```bash
cd /opt/vps-panel
./scripts/update.sh
```

Pulls, builds the agent and the panel, and only then installs them and restarts both services. If a build fails, the previous panel install is restored and nothing else changes (the previous install stays in `/opt/vps-panel/app.prev` until the next update). SQLite migrations run on panel start.

## Uninstall

```bash
./scripts/uninstall.sh
```

Stops both services, removes systemd units, removes the nginx vhost, leaves `/var/lib/vps-panel` and MariaDB databases alone. Pass `--purge` to wipe state too.

## Backups

Configure in Settings -> Backups. Default repository is `/var/backups/vps-panel`. Add an off-host repo (S3 / B2 / SFTP) in the same screen.

## Manual recovery

If the panel is broken and you need to fix it from SSH:

- Panel logs: `journalctl -u vps-panel.service -f`
- Agent logs: `journalctl -u vps-panel-agent.service -f`
- Panel DB: `sqlite3 /var/lib/vps-panel/panel.db`
- Agent socket health: `curl --unix-socket /run/vps-panel/agent.sock http://x/health -H "Authorization: Bearer $(cat /etc/vps-panel/agent.token)"`
- Reset admin password: `sudo /usr/local/bin/vps-panel-agent admin-reset --email you@example.com` (writes new argon2 hash to `panel.db`).
