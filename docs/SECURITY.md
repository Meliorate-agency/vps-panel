# Security model

## Threat model

Adversaries we defend against:

1. **Internet attackers** scanning for credentials, exploits, or bots probing the panel.
2. **Compromised user app** running on the same VPS (e.g. a deployed Node app gets RCE).
3. **Compromised panel process** (e.g. a Next.js dependency exploit).
4. **Stolen panel session cookie** (XSS, MITM, careless laptop).

Adversaries we explicitly do NOT defend against:

- A root-level adversary already on the VPS. If they have root, they own the panel.
- A malicious admin. The admin has full power by design.
- Side-channel hardware attacks.

## Defenses

### Process isolation (defense against #3 - panel compromise)
- Next.js runs as `vps-panel` (unprivileged). It has no `sudo`, no shell tools in its `WorkingDirectory`, and the systemd unit sets `ProtectSystem=strict`, `NoNewPrivileges=true`, `PrivateTmp=true`, `ReadWritePaths=/var/lib/vps-panel /var/log/vps-panel /opt/vps-panel/app/.next`.
- The only privileged path is `/run/vps-panel/agent.sock`, owned `0660 root:vps-panel`. The bearer token in `/etc/vps-panel/agent.token` (mode `0640 root:vps-panel`) is a second factor: an attacker would need both group membership AND the token.
- The Rust agent runs as root but has a tiny attack surface: one Unix socket, one HTTP handler per typed request, no string-interpolated shell commands ever (see `exec.rs`).

### App isolation (defense against #2 - tenant app RCE)
- Each deployed Node app runs as the unprivileged `apps` user with a hardened systemd unit: `ProtectSystem=strict`, `PrivateTmp=true`, `NoNewPrivileges=true`, `ReadWritePaths` limited to its source dir and a per-app data dir.
- Apps cannot read `/etc/vps-panel`, `/var/lib/vps-panel`, the agent socket, or other apps' env files.
- Each app's env file is mode `0640 root:apps` and contains only that app's secrets.
- A Docker app runs its image in a container with `no-new-privileges`, a process limit, and memory and CPU limits by default. The image runs as its publisher built it (often root inside the container), so deploying an image means trusting its publisher. The panel accepts only an image name, a port, env, folders and limits, never raw `docker` flags: privileged mode, the host network, the Docker socket and host paths cannot be asked for. Kept folders are named volumes, not host paths.
- A Docker app built from Git runs `docker build` on its own checkout: the Dockerfile's `RUN` steps run in build containers, not on the host, and the build folder and Dockerfile must resolve inside the checkout (a symlink pointing out of it is refused, since the docker CLI runs as root). Build variables are stored in the image, so secrets belong in the runtime environment.
- A Docker app's port is published on `127.0.0.1` only and reaches the internet through nginx. Docker's own port publishing bypasses UFW, so a `0.0.0.0` publish would be public whatever the firewall says; the agent never writes one.

### Network surface (defense against #1 - internet attackers)
- UFW default deny inbound. Allowed: SSH (22, or the port sshd is on), 25, 465, 587 (mail SMTP), 80, 443 (web), 53 (DNS), 993, 995 (IMAP/POP3 SSL), and 4190 (ManageSieve) from Docker's networks only. The installer opens the web, mail and DNS ports once, on the first install: a port closed since stays closed when the installer is run again, which then only makes sure the SSH rule is there.
- Panel port 10000 is bound to `127.0.0.1` only, exposed via nginx on `panel.<your-domain>` over HTTPS.
- nginx sets `Strict-Transport-Security`, `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`, a strict CSP, and `Referrer-Policy: same-origin`. HSTS (two years) covers the name that has the certificate, not its subdomains (no `includeSubDomains`): a subdomain served elsewhere, or without a certificate, stays reachable over plain HTTP. An update removes `includeSubDomains` from the vhosts the panel wrote earlier.
- fail2ban watches SSH, dovecot, postfix-sasl, reading the journal. Default ban: 1h after 5 failures in 10 min; the dovecot jail bans for 10 min after 10 failures in 10 min, since mail clients open several connections at once.
- Optional IP allowlist: Settings -> Access writes `allow`/`deny` directives into the panel's nginx vhost.

### Auth (defense against #4 - session theft)
- Password hash: argon2id with sane params (time=3, mem=64MB, parallelism=4).
- JWT in `httpOnly SameSite=Strict` cookie, `Secure` once the panel has a certificate (`COOKIE_SECURE=1`; the installer starts on plain HTTP with `0`). The access cookie (`vps_panel_session`, path `/`) and the refresh cookie (`vps_panel_refresh`, path `/api/auth/refresh`) both last as long as the session timeout in Settings > Account (60 minutes unless changed), renewed while the panel is in use, or 30 days with Remember me. SameSite=Strict + Origin header check defeats CSRF.
- TOTP (RFC 6238) is optional: a user enrols at `/totp`, and only accounts with it enabled are asked for a code at login.
- Login rate limit: 5 failed attempts per IP per 15 min, 10 per email per 15 min; the wait is shown to the user.
- Logout invalidates the JWT jti via the `sessions` revocation table. `getSession` (`src/lib/auth.ts`) checks it for every page, server action and API route; `proxy.ts` only verifies the token's signature and type.

### Secret hygiene
- `.env` lives at `/etc/vps-panel/.env` (mode `0640 root:vps-panel`).
- `ADMIN_EMAIL` and `ADMIN_INITIAL_PASSWORD` in `/etc/vps-panel/.env` only seed the first admin: the panel reads them while it has no admin and never again. Change the email and password in the panel; a lost password is reset as root with `vps-panel-agent admin-reset --email <login email>`.
- `JWT_SECRET`, `AGENT_TOKEN`, `RESTIC_PASSWORD` generated by the installer with `openssl rand -base64 48`. Never written to logs.
- Audit log records request hashes, never request bodies that may contain secrets. The exception is the web terminal: every command is recorded word for word (up to 2,000 characters), so a password typed into a command line ends up in the audit log and the terminal history. What else the panel keeps, and for how long: [PRIVACY.md](PRIVACY.md).
- Passwords for created mailboxes / DB users are returned exactly once, in the response, and never re-displayed (UI shows a copy-once dialog).

### Hardening checklist (applied by `scripts/install.sh`)
- SSH: ships **permissive** out of the box — `PermitRootLogin yes`, `PasswordAuthentication yes`, `KbdInteractiveAuthentication yes`. This is deliberate: fresh VMs that were provisioned without an SSH key used to get locked out the moment `install.sh` finished. Tighten both via **Settings → SSH server** in the panel once you've added a key under `/ssh-keys` (the right long-term posture). fail2ban's `sshd` jail (default `maxretry=5`, `bantime=1h`, configurable from `/firewall`) blunts brute-force in the meantime. ed25519 host keys are preserved.
- sysctl: `net.ipv4.tcp_syncookies=1`, `net.ipv4.conf.all.rp_filter=1`, `kernel.kptr_restrict=2`, `kernel.dmesg_restrict=1`, `fs.protected_hardlinks=1`, `fs.protected_symlinks=1`.
- Unattended security upgrades enabled, auto-reboot at 04:00 if a kernel update needs it.
- AppArmor: `apparmor`, `apparmor-utils` and `apparmor-profiles` are installed; the installer loads and enforces no profile of its own.
- chrony for NTP (TOTP, JWT, certs all need accurate time).
- auditd is installed and running with Ubuntu's default (empty) rule set; the installer adds no watch rules.
- systemd-journald: persistent storage, 1 GB cap, no time limit unless a log retention is set in Settings > Privacy (see [PRIVACY.md](PRIVACY.md)).

## What goes in `.env`

```
JWT_SECRET=...                     # 48 random bytes b64
AGENT_TOKEN=...                    # 48 random bytes b64
PANEL_HOST=panel.example.com       # used in cookie domain + cert
PANEL_DB_PATH=/var/lib/vps-panel/panel.db
AGENT_SOCKET=/run/vps-panel/agent.sock
RESTIC_PASSWORD=...                # 48 random bytes b64
RESTIC_REPOSITORY=/var/backups/vps-panel
ADMIN_EMAIL=...                    # seeds the first admin only
ADMIN_INITIAL_PASSWORD=...         # seeds the first admin only - never read again once an admin exists
```

## Reporting

If you find a vulnerability while building this thing, write it down in `docs/SECURITY.md` under "Known issues" before fixing it.
