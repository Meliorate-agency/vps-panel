#!/usr/bin/env bash
set -euo pipefail

if [[ $EUID -ne 0 ]]; then
  echo "must run as root" >&2
  exit 1
fi

PURGE=0
if [[ "${1-}" == "--purge" ]]; then PURGE=1; fi

systemctl disable --now vps-panel.service vps-panel-agent.service 2>/dev/null || true
rm -f /etc/systemd/system/vps-panel.service /etc/systemd/system/vps-panel-agent.service
systemctl daemon-reload

rm -f /etc/nginx/sites-enabled/vps-panel.conf
rm -f /etc/nginx/sites-available/vps-panel.conf
nginx -t && systemctl reload nginx || true

rm -f /usr/local/bin/vps-panel-agent
rm -rf /opt/vps-panel
rm -rf /run/vps-panel

if [[ "$PURGE" -eq 1 ]]; then
  # Only the panel's own state. Other services keep running on the rest:
  # nginx, Postfix and Dovecot use the certificates in /etc/vps-panel/certs,
  # every app's unit reads /etc/vps-panel/apps/<name>.env, PowerDNS and mail
  # log in against the pdns and mail databases. Purge used to delete all of
  # it and leave nginx unable to reload, apps unable to start and DNS and
  # mail dead. Backups stay too, with the key needed to read them.
  echo "purging panel state"
  rm -rf /var/lib/vps-panel /var/log/vps-panel
  rm -f /etc/vps-panel/.env /etc/vps-panel/agent.token /etc/vps-panel/acme-account.json
  rm -f /etc/fail2ban/jail.d/zz-vps-panel-operator.conf
  mariadb -e "DROP DATABASE IF EXISTS panel;" || true
  cat <<'EOF'
kept (still in use by nginx, apps, DNS and mail, or needed for restores):
  /etc/vps-panel/certs, /etc/vps-panel/apps, /etc/vps-panel/backup.key
  /var/backups/vps-panel (restic repository)
  MariaDB databases pdns and mail; site vhosts; vps-app-* units
  /etc/ssh/sshd_config.d/00-vps-panel.conf (your SSH settings)
EOF
fi

echo "removed."
