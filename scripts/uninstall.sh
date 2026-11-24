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

# With the previous version update.sh keeps for a rollback (the previous
# panel install is /opt/vps-panel/app.prev).
rm -f /usr/local/bin/vps-panel-agent /usr/local/bin/vps-panel-agent.prev
rm -rf /var/backups/vps-panel-config/units.prev
rm -rf /opt/vps-panel
rm -rf /run/vps-panel
# Settings > Privacy: the masked access log and the log retention are kept up
# by the agent. Without it they go back to the system's own settings, so the
# server is not left with a log format and limits nothing shows any more.
if grep -q '#vps-panel-privacy: ' /etc/nginx/nginx.conf 2>/dev/null || [[ -f /etc/nginx/conf.d/vps-panel-privacy.conf ]]; then
  sed -i 's/#vps-panel-privacy: //' /etc/nginx/nginx.conf
  rm -f /etc/nginx/conf.d/vps-panel-privacy.conf
  if nginx -t >/dev/null 2>&1; then systemctl reload nginx || true; fi
fi
rm -f /etc/logrotate.d/0-vps-panel-*
if [[ -f /etc/systemd/journald.conf.d/90-vps-panel-retention.conf ]]; then
  rm -f /etc/systemd/journald.conf.d/90-vps-panel-retention.conf
  systemctl restart systemd-journald || true
fi

# The agent orders MariaDB after Docker while phpMyAdmin has it listen on the
# Docker bridge. Without the agent nothing maintains that drop-in.
if [[ -f /etc/systemd/system/mariadb.service.d/10-vps-panel-docker.conf ]]; then
  rm -f /etc/systemd/system/mariadb.service.d/10-vps-panel-docker.conf
  rmdir /etc/systemd/system/mariadb.service.d 2>/dev/null || true
  systemctl daemon-reload || true
fi

if [[ "$PURGE" -eq 1 ]]; then
  # Only the panel's own state. Other services keep running on the rest:
  # nginx, Postfix and Dovecot use the certificates in /etc/vps-panel/certs,
  # every app's unit reads /etc/vps-panel/apps/<name>.env, PowerDNS and mail
  # log in against the pdns and mail databases. Purge used to delete all of
  # it and leave nginx unable to reload, apps unable to start and DNS and
  # mail dead. Backups stay too, with the key needed to read them.
  echo "purging panel state"
  rm -rf /var/lib/vps-panel /var/log/vps-panel
  # The plain-SQL dumps a backup takes before restic picks them up. They used
  # to go with /var/lib/vps-panel; the encrypted copies stay in the repository.
  rm -rf /var/backups/vps-panel-db-dumps
  rm -f /etc/vps-panel/.env /etc/vps-panel/agent.token /etc/vps-panel/acme-account.json
  rm -f /etc/fail2ban/jail.d/zz-vps-panel-operator.conf
  # Settings > Panel access. Only the panel vhost (removed above) includes
  # it. Left behind, a reinstall kept enforcing the old list while the new
  # panel, its database gone, showed none.
  rm -f /etc/nginx/vps-panel-access.conf
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
