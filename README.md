# VPS Panel

A self-hosted, single-operator VPS control panel for Ubuntu 24.04 LTS. Two processes: a **Next.js 16** panel (port 10000) for the UI, and a privileged **Rust agent** that does all system-level work over a Unix socket. The panel never has root and never shells out.

See [ARCHITECTURE.md](ARCHITECTURE.md) for the map of the codebase, [docs/SECURITY.md](docs/SECURITY.md) for the threat model and hardening, and [docs/DEPLOY.md](docs/DEPLOY.md) for installing on a fresh server.

## What it does

- Deploy Node.js apps from a folder + port + subdomain. Writes a hardened systemd unit and an nginx vhost; optionally issues a Let's Encrypt cert.
- Self-hosted DNS via PowerDNS (MariaDB-backed). Add zones and records; one-click DNSSEC.
- Mail: Postfix + Dovecot + Rspamd with virtual users in MariaDB, DKIM signing, SPF/DMARC helpers.
- MariaDB user/database CRUD with random passwords.
- SSL: in-process ACME via the `instant-acme` Rust crate. Daily renewal scan.
- Backups: encrypted, deduped, incremental via `restic`.
- Live log tailing, task progress streams, audit log, server health charts.
- One-click installer that hardens the OS (ufw, fail2ban, unattended-upgrades, AppArmor, sysctl, SSH, journald, chrony).
- Cron jobs, SSH key management, IP allowlist, notification channels (email/webhook).

## Install

```
git clone <repo> /opt/vps-panel
cd /opt/vps-panel
sudo ./scripts/install.sh --domain panel.example.com --email you@example.com
```

The script installs everything, generates secrets, creates system users, brings up the agent and the panel, and prints the bootstrap admin credentials.

## Develop

For local development on a workstation (no system mutations, no privileged paths):

- `npm install`
- `npm run dev`  - panel on `http://localhost:10000`
- `cargo check --manifest-path agent/Cargo.toml` - the agent will only fully build/run on Linux because of UnixListener and `nix` requirements.

## Out of scope

Docker/container deploys, PHP, multi-server orchestration.

## License

[AGPL-3.0](LICENSE). You may use, change and share it; if you run a modified version as a service for others, you must publish your changes under the same license.
