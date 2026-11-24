#!/usr/bin/env bash
# VPS Panel one-shot installer for Ubuntu 24.04 LTS.
# Run as root from a fresh server.
set -euo pipefail

PANEL_HOST=""
ADMIN_EMAIL=""
ASSUME_YES=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --domain) PANEL_HOST="$2"; shift 2 ;;
    --email)  ADMIN_EMAIL="$2"; shift 2 ;;
    -y|--yes) ASSUME_YES=1; shift ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

if [[ $EUID -ne 0 ]]; then
  echo "must run as root" >&2
  exit 1
fi

if ! grep -q "Ubuntu 24" /etc/os-release; then
  echo "this installer only supports Ubuntu 24.04" >&2
  exit 1
fi

# A re-run keeps the hostname the panel is already set to (Settings > Panel
# hostname). Without this it fell back to the system hostname and reset
# Postfix's myhostname and Dovecot's postmaster address.
if [[ -z "$PANEL_HOST" && -f /etc/vps-panel/.env ]]; then
  # `|| true`: with pipefail, a .env without a PANEL_HOST line ended the
  # installer right here, without a word.
  PREV_HOST="$(grep -E '^PANEL_HOST=' /etc/vps-panel/.env | head -1 | cut -d= -f2- || true)"
  if [[ -n "$PREV_HOST" && ! "$PREV_HOST" =~ ^[0-9.]+$ && "$PREV_HOST" == *.* ]]; then
    PANEL_HOST="$PREV_HOST"
  fi
fi

if [[ -z "$PANEL_HOST" ]] && [[ -t 0 ]] && [[ $ASSUME_YES -eq 0 ]]; then
  echo "No --domain supplied. Leave blank to install in IP-only mode (you can"
  echo "set a real hostname later from the panel UI)."
  read -r -p "Panel hostname (e.g. panel.example.com, or blank): " PANEL_HOST
fi
if [[ -z "$ADMIN_EMAIL" ]] && [[ -t 0 ]] && [[ $ASSUME_YES -eq 0 ]]; then
  echo "No --email supplied. Leave blank for the default admin@<hostname>."
  read -r -p "Admin email (or blank): " ADMIN_EMAIL
fi

# PANEL_HOST is optional. When supplied it MUST be an FQDN — Postfix refuses
# numeric `myhostname`, and the panel's Origin/Host checks expect a real
# hostname. When omitted, install in IP-only mode: nginx serves any Host,
# Postfix uses the system hostname, cookies are non-Secure (HTTP works), and
# the operator sets the real FQDN later through the panel UI.
# Only the characters of a hostname: the value goes into .env, Postfix, a sed
# expression for Dovecot and the nginx vhost. A pasted URL
# (https://panel.example.com/) used to pass, land in .env and Postfix, and
# then stop the installer at the Dovecot step.
if [[ -n "$PANEL_HOST" ]]; then
  PANEL_HOST="${PANEL_HOST,,}"
  if [[ "$PANEL_HOST" =~ ^[0-9.]+$ ]] || [[ "$PANEL_HOST" != *.* ]] \
     || [[ ! "$PANEL_HOST" =~ ^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)+$ ]]; then
    echo "FATAL: PANEL_HOST '$PANEL_HOST' is not a valid FQDN." >&2
    echo "       Use something like panel.example.com: letters, digits, hyphens and dots only" >&2
    echo "       (no http://, no path, must contain a dot, not an IP address)." >&2
    echo "       Or leave blank to defer the hostname choice." >&2
    exit 1
  fi
fi

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
INSTALL_DIR="/opt/vps-panel"
APP_DIR="$INSTALL_DIR/app"
ETC_DIR="/etc/vps-panel"
VAR_DIR="/var/lib/vps-panel"
LOG_DIR="/var/log/vps-panel"
RUN_DIR="/run/vps-panel"
# Outside $VAR_DIR: nginx (www-data) must never need the vps-panel group.
ACME_WEBROOT="/var/www/vps-panel-acme"

log()  { printf "\033[1;36m==>\033[0m %s\n" "$*"; }
warn() { printf "\033[1;33m  !\033[0m %s\n" "$*" >&2; }

# Public IPv4 detection — used in IP-only mode for the .env PANEL_HOST display
# value and later by pdns for local-address. Detected once here so both
# consumers see the same value. The filter strips RFC1918/100.64/127/169.254
# ranges and falls back to the first global v4 if no public one is found.
# `|| true`: with pipefail, grep matching nothing (a NAT'd VPS with only a
# private address) used to abort the installer right here.
PUBLIC_IP=$(ip -4 -o addr show scope global 2>/dev/null | awk '{print $4}' | cut -d/ -f1 \
            | grep -vE '^(10|127|172\.(1[6-9]|2[0-9]|3[01])|192\.168|169\.254|100\.(6[4-9]|[7-9][0-9]|1[01][0-9]|12[0-7]))\.' \
            | head -1 || true)
if [[ -z "$PUBLIC_IP" ]]; then
  PUBLIC_IP=$(ip -4 -o addr show scope global 2>/dev/null | awk '{print $4}' | cut -d/ -f1 | head -1 || true)
fi

# Effective hostnames for downstream service configs and the .env file.
# - MAIL_HOST: used by Postfix `myhostname` and Dovecot `postmaster_address`.
#   Must be a valid hostname (Postfix rejects numeric/IP-literal myhostname).
#   Falls back to the system hostname when no FQDN was supplied.
# - NGINX_SERVER_NAME: written into the panel vhost. `_` makes the block a
#   default_server matching any Host header, so http://<ip>/ reaches the panel
#   before the operator sets a real domain.
# - PANEL_HOST_ENV: written into /etc/vps-panel/.env so the panel UI knows
#   "where am I accessible." In IP-only mode this is the detected public IP.
# - COOKIE_SECURE: always 0 at install. Nothing here issues a certificate,
#   so the panel starts on plain HTTP even with a domain, and browsers drop
#   Secure cookies over HTTP: with 1 the first login could never stick. The
#   panel-host task flips it to 1 once a certificate is in place.
if [[ -n "$PANEL_HOST" ]]; then
  MAIL_HOST="$PANEL_HOST"
  NGINX_SERVER_NAME="$PANEL_HOST"
  PANEL_HOST_ENV="$PANEL_HOST"
  COOKIE_SECURE_ENV=0
else
  MAIL_HOST="$(hostname -f 2>/dev/null || hostname)"
  if [[ -z "$MAIL_HOST" || "$MAIL_HOST" =~ ^[0-9.]+$ || "$MAIL_HOST" != *.* ]]; then
    MAIL_HOST="vps-panel.local"
  fi
  NGINX_SERVER_NAME="_"
  PANEL_HOST_ENV="${PUBLIC_IP:-127.0.0.1}"
  COOKIE_SECURE_ENV=0
fi

# Default admin login email to admin@<mail-host> when not supplied. The
# operator can change the ACME contact email separately through the wizard's
# SSL email step (panel.acme_email setting overrides ACME_EMAIL from .env on
# every issuance). ADMIN_EMAIL and ADMIN_INITIAL_PASSWORD only seed the first
# admin: once it exists the panel never reads them again.
if [[ -z "$ADMIN_EMAIL" ]]; then
  ADMIN_EMAIL="admin@$MAIL_HOST"
fi
if [[ ! "$ADMIN_EMAIL" =~ ^[^@[:space:]]+@[^@[:space:]]+$ ]]; then
  echo "FATAL: ADMIN_EMAIL '$ADMIN_EMAIL' is not a valid email." >&2
  exit 1
fi

#-------------------------------------------------- packages
# On a re-run apt may upgrade a package whose configuration file was changed
# on the server (pdns.conf, which this installer writes, for one). dpkg then
# stops to ask which version to keep, or fails where nobody can answer. These
# options keep the file that is installed, without asking.
APT_KEEP_CONF=(-o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold)

log "updating apt"
DEBIAN_FRONTEND=noninteractive apt-get update -qq
# bind9-dnsutils is dig: the agent looks records up with it before it orders
# a certificate, and for the mail checks. Minimal images come without it, and
# then every such check failed as "no public A or AAAA record".
DEBIAN_FRONTEND=noninteractive apt-get install -y "${APT_KEEP_CONF[@]}" --no-install-recommends \
  curl ca-certificates gnupg lsb-release software-properties-common \
  build-essential pkg-config libssl-dev libsqlite3-dev libmariadb-dev \
  jq acl htop iotop iftop ncdu rsync sudo bind9-dnsutils \
  nginx \
  mariadb-server mariadb-client \
  pdns-server pdns-backend-mysql \
  postfix postfix-mysql \
  dovecot-core dovecot-imapd dovecot-pop3d dovecot-lmtpd dovecot-mysql \
  dovecot-sieve dovecot-managesieved \
  ufw fail2ban nftables python3-systemd \
  unattended-upgrades apt-listchanges \
  chrony \
  auditd audispd-plugins \
  apparmor apparmor-utils apparmor-profiles \
  restic openssl

# Node 20 reached end of life in April 2026: fresh installs get 22. A server
# that already runs Node 20 keeps it, because deployed apps run on this Node
# and their native modules must be rebuilt after a major upgrade.
log "adding NodeSource (Node 22 LTS)"
if ! command -v node >/dev/null 2>&1 || [[ "$(node --version | cut -dv -f2 | cut -d. -f1)" -lt 20 ]]; then
  curl -fsSL https://deb.nodesource.com/setup_22.x | bash -
  apt-get install -y "${APT_KEEP_CONF[@]}" nodejs
fi

log "adding upstream Rspamd repo"
mkdir -p /etc/apt/keyrings
if [[ ! -f /etc/apt/keyrings/rspamd.gpg ]]; then
  curl -fsSL https://rspamd.com/apt-stable/gpg.key | gpg --dearmor -o /etc/apt/keyrings/rspamd.gpg
fi
echo "deb [signed-by=/etc/apt/keyrings/rspamd.gpg] http://rspamd.com/apt-stable/ $(lsb_release -cs) main" \
  > /etc/apt/sources.list.d/rspamd.list
apt-get update -qq
apt-get install -y "${APT_KEEP_CONF[@]}" rspamd

log "installing rustup for the panel build user"
if ! command -v cargo >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile minimal
  source "$HOME/.cargo/env"
fi
export PATH="/root/.cargo/bin:$PATH"

#-------------------------------------------------- system users
log "creating system users (vps-panel, apps, vmail)"
getent group vps-panel >/dev/null 2>&1 || groupadd --system vps-panel
id -u vps-panel >/dev/null 2>&1 || useradd --system --gid vps-panel \
  --home-dir "$VAR_DIR" --shell /usr/sbin/nologin vps-panel

# www-data must NOT be in the vps-panel group: that group reads .env (the
# JWT secret, the agent token) and panel.db and connects to the root agent's
# socket, so anything running as www-data could become root. Older installs
# added it to reach the ACME webroot, which now lives outside $VAR_DIR.
if id -nG www-data 2>/dev/null | tr ' ' '\n' | grep -qx vps-panel; then
  gpasswd -d www-data vps-panel >/dev/null
fi

getent group apps >/dev/null 2>&1 || groupadd --system apps
id -u apps >/dev/null 2>&1 || useradd --system --gid apps \
  --home-dir /srv/apps --shell /bin/bash apps
mkdir -p /srv/apps
chown apps:apps /srv/apps
chmod 750 /srv/apps

# Operator storage behind the panel's Drive page. Sibling of /srv/apps rather
# than anything under /var/lib/vps-panel, so `uninstall.sh --purge` never takes
# personal files with it. If this ever needs its own disk, mount the volume
# here and nothing in the panel changes. The agent also creates this at
# startup, so existing installs pick it up on the next self-update.
mkdir -p /srv/drive
chown root:root /srv/drive
chmod 755 /srv/drive

getent group vmail >/dev/null 2>&1 || groupadd --system --gid 5000 vmail
id -u vmail >/dev/null 2>&1 || useradd --system --uid 5000 --gid vmail \
  --home-dir /var/vmail --shell /usr/sbin/nologin vmail
mkdir -p /var/vmail
chown vmail:vmail /var/vmail
chmod 770 /var/vmail

#-------------------------------------------------- directories
log "creating directories"
install -d -m 0750 -o vps-panel -g vps-panel "$INSTALL_DIR" "$APP_DIR" "$VAR_DIR" "$LOG_DIR"
install -d -m 0750 -o root      -g vps-panel "$ETC_DIR"
install -d -m 0755 -o root      -g root      "$RUN_DIR"
install -d -m 0755 -o root      -g root      "$ACME_WEBROOT"
install -d -m 0750 -o root      -g apps      "$ETC_DIR/apps"
install -d -m 0750 -o root      -g vps-panel "$ETC_DIR/certs"
install -d -m 0750 -o root      -g root      /var/backups/vps-panel

# Setgid so panel.db (and -wal/-shm) created by either process (agent as
# root, panel as vps-panel) end up with group=vps-panel and are read/write
# to both. The directory only, never its contents: on a re-run a recursive
# chown/chmod handed the panel's user the agent's root-only folders (database
# dumps, the Files trash) and changed the owner of everything in the trash,
# and of a running app's files that a build snapshot hard-links.
chmod 2770 "$VAR_DIR"
# Back to root what an earlier re-run gave away.
for d in "$VAR_DIR/db-dumps" "$VAR_DIR/trash" "$VAR_DIR/build-snapshots"; do
  if [[ -d "$d" ]]; then
    chown root "$d"
    chmod 0700 "$d"
  fi
done


# Pre-create panel.db with explicit vps-panel:vps-panel ownership + 0660 perms.
# Both the agent (root, rusqlite) and the panel (vps-panel, better-sqlite3)
# open this file read/write -- relying on UMask + setgid alone has proven
# fragile in the field (whichever process creates the file first wins, and
# some sqlite bindings ignore umask for the WAL/SHM sidecars). Touching the
# file now means it exists with correct perms BEFORE either service starts,
# and SQLite just opens an empty file instead of creating one with whatever
# mode its bindings choose. Same treatment for -wal/-shm so WAL mode boots
# without permission surprises.
touch "$VAR_DIR/panel.db" "$VAR_DIR/panel.db-wal" "$VAR_DIR/panel.db-shm"
chown vps-panel:vps-panel "$VAR_DIR/panel.db" "$VAR_DIR/panel.db-wal" "$VAR_DIR/panel.db-shm"
chmod 0660 "$VAR_DIR/panel.db" "$VAR_DIR/panel.db-wal" "$VAR_DIR/panel.db-shm"

#-------------------------------------------------- secrets
log "generating secrets"
ENV_FILE="$ETC_DIR/.env"
if [[ ! -f "$ENV_FILE" ]]; then
  JWT_SECRET="$(openssl rand -base64 48 | tr -d '\n')"
  AGENT_TOKEN="$(openssl rand -base64 48 | tr -d '\n')"
  # Reuse the key of an existing backup repository (kept by uninstall
  # --purge); a new one would make every earlier snapshot unreadable.
  if [[ -s "$ETC_DIR/backup.key" ]]; then
    RESTIC_PASSWORD="$(cat "$ETC_DIR/backup.key")"
  else
    RESTIC_PASSWORD="$(openssl rand -base64 48 | tr -d '\n')"
  fi
  ADMIN_PASS="$(openssl rand -base64 24 | tr -d '\n=' | cut -c1-24)"
  cat > "$ENV_FILE" <<EOF
JWT_SECRET=$JWT_SECRET
AGENT_TOKEN=$AGENT_TOKEN
RESTIC_PASSWORD=$RESTIC_PASSWORD
PANEL_HOST=$PANEL_HOST_ENV
PANEL_PORT=10000
NODE_ENV=production
AGENT_SOCKET=$RUN_DIR/agent.sock
PANEL_DB_PATH=$VAR_DIR/panel.db
RESTIC_REPOSITORY=/var/backups/vps-panel
ADMIN_EMAIL=$ADMIN_EMAIL
ADMIN_INITIAL_PASSWORD=$ADMIN_PASS
COOKIE_SECURE=$COOKIE_SECURE_ENV
ACME_EMAIL=$ADMIN_EMAIL
REPO_DIR=$REPO_DIR
EOF
  chmod 0640 "$ENV_FILE"
  chown root:vps-panel "$ENV_FILE"
  echo "$AGENT_TOKEN" > "$ETC_DIR/agent.token"
  chmod 0640 "$ETC_DIR/agent.token"
  chown root:vps-panel "$ETC_DIR/agent.token"
  if [[ ! -s "$ETC_DIR/backup.key" ]]; then
    echo "$RESTIC_PASSWORD" > "$ETC_DIR/backup.key"
    chmod 0400 "$ETC_DIR/backup.key"
  fi
fi

#-------------------------------------------------- MariaDB schemas
log "configuring MariaDB schemas"
systemctl enable --now mariadb

# A re-run keeps the passwords PowerDNS, Postfix and Dovecot already have.
# New ones on every run meant that a run which stopped before those services
# got their new config left DNS and mail with a password MariaDB no longer
# accepted. The pattern keeps a hand-edited value out of the SQL below.
PDNS_PASS="$(sed -n 's/^gmysql-password=//p' /etc/powerdns/pdns.conf 2>/dev/null | head -1 || true)"
[[ "$PDNS_PASS" =~ ^[A-Za-z0-9+/]{16,}$ ]] || PDNS_PASS="$(openssl rand -base64 24 | tr -d '\n=' | cut -c1-24)"
MAIL_PASS="$(sed -n 's/^password = //p' /etc/postfix/mysql-virtual-mailbox-maps.cf 2>/dev/null | head -1 || true)"
[[ "$MAIL_PASS" =~ ^[A-Za-z0-9+/]{16,}$ ]] || MAIL_PASS="$(openssl rand -base64 24 | tr -d '\n=' | cut -c1-24)"

mariadb <<SQL
CREATE DATABASE IF NOT EXISTS panel CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;
CREATE DATABASE IF NOT EXISTS pdns  CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;
CREATE DATABASE IF NOT EXISTS mail  CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;

-- Drop+recreate (not CREATE IF NOT EXISTS) and pin auth plugin to
-- mysql_native_password. On Ubuntu 24.04 MariaDB 10.11, plain CREATE USER ...
-- IDENTIFIED BY '...' sometimes ends up with the unix_socket plugin instead
-- of native_password, which makes pdns_server fail with
-- "Access denied for user 'pdns'@'localhost'" (MariaDB error 1698) even with
-- the correct password. Same trap hit the 'mail' user.
DROP USER IF EXISTS 'pdns'@'localhost';
CREATE USER 'pdns'@'localhost' IDENTIFIED VIA mysql_native_password USING PASSWORD('$PDNS_PASS');
GRANT ALL PRIVILEGES ON pdns.* TO 'pdns'@'localhost';

DROP USER IF EXISTS 'mail'@'localhost';
CREATE USER 'mail'@'localhost' IDENTIFIED VIA mysql_native_password USING PASSWORD('$MAIL_PASS');
GRANT SELECT ON mail.* TO 'mail'@'localhost';
GRANT ALL PRIVILEGES ON mail.* TO 'root'@'localhost';
FLUSH PRIVILEGES;
SQL

mariadb pdns <<'SQL'
CREATE TABLE IF NOT EXISTS domains (
  id INT AUTO_INCREMENT PRIMARY KEY, name VARCHAR(255) NOT NULL,
  master VARCHAR(128) DEFAULT NULL, last_check INT DEFAULT NULL,
  type VARCHAR(8) NOT NULL, notified_serial INT UNSIGNED DEFAULT NULL,
  account VARCHAR(40) CHARACTER SET 'utf8' DEFAULT NULL,
  options VARCHAR(64000) DEFAULT NULL, catalog VARCHAR(255) DEFAULT NULL,
  UNIQUE KEY name_index (name)
) Engine=InnoDB CHARACTER SET 'latin1';
CREATE TABLE IF NOT EXISTS records (
  id BIGINT AUTO_INCREMENT PRIMARY KEY, domain_id INT DEFAULT NULL,
  name VARCHAR(255) DEFAULT NULL, type VARCHAR(10) DEFAULT NULL,
  content VARCHAR(64000) DEFAULT NULL, ttl INT DEFAULT NULL, prio INT DEFAULT NULL,
  disabled TINYINT(1) DEFAULT 0, ordername VARCHAR(255) DEFAULT NULL,
  auth TINYINT(1) DEFAULT 1,
  KEY nametype_index (name,type), KEY domain_id (domain_id),
  KEY ordername (ordername)
) Engine=InnoDB CHARACTER SET 'latin1';
CREATE TABLE IF NOT EXISTS supermasters (
  ip VARCHAR(64) NOT NULL, nameserver VARCHAR(255) NOT NULL,
  account VARCHAR(40) NOT NULL, PRIMARY KEY (ip,nameserver)
) Engine=InnoDB CHARACTER SET 'latin1';
CREATE TABLE IF NOT EXISTS comments (
  id INT AUTO_INCREMENT PRIMARY KEY, domain_id INT NOT NULL,
  name VARCHAR(255) NOT NULL, type VARCHAR(10) NOT NULL,
  modified_at INT NOT NULL, account VARCHAR(40) NOT NULL,
  comment TEXT CHARACTER SET 'utf8' NOT NULL
) Engine=InnoDB CHARACTER SET 'latin1';
CREATE TABLE IF NOT EXISTS domainmetadata (
  id INT AUTO_INCREMENT PRIMARY KEY, domain_id INT NOT NULL,
  kind VARCHAR(32), content TEXT, KEY domain_id (domain_id)
) Engine=InnoDB CHARACTER SET 'latin1';
CREATE TABLE IF NOT EXISTS cryptokeys (
  id INT AUTO_INCREMENT PRIMARY KEY, domain_id INT NOT NULL,
  flags INT NOT NULL, active TINYINT(1), published TINYINT(1) DEFAULT 1,
  content TEXT, KEY domain_id (domain_id)
) Engine=InnoDB CHARACTER SET 'latin1';
CREATE TABLE IF NOT EXISTS tsigkeys (
  id INT AUTO_INCREMENT PRIMARY KEY, name VARCHAR(255), algorithm VARCHAR(50),
  secret VARCHAR(255), UNIQUE KEY namealgoindex (name,algorithm)
) Engine=InnoDB CHARACTER SET 'latin1';
SQL

mariadb mail <<'SQL'
CREATE TABLE IF NOT EXISTS virtual_domains (
  id INT AUTO_INCREMENT PRIMARY KEY, name VARCHAR(255) NOT NULL UNIQUE
) Engine=InnoDB;
CREATE TABLE IF NOT EXISTS virtual_users (
  id INT AUTO_INCREMENT PRIMARY KEY, domain_id INT NOT NULL,
  email VARCHAR(255) NOT NULL UNIQUE, password VARCHAR(255) NOT NULL,
  quota_mb BIGINT NOT NULL DEFAULT 1024,
  FOREIGN KEY (domain_id) REFERENCES virtual_domains(id) ON DELETE CASCADE
) Engine=InnoDB;
CREATE TABLE IF NOT EXISTS virtual_aliases (
  id INT AUTO_INCREMENT PRIMARY KEY, domain_id INT NOT NULL,
  source VARCHAR(255) NOT NULL, destination VARCHAR(255) NOT NULL,
  FOREIGN KEY (domain_id) REFERENCES virtual_domains(id) ON DELETE CASCADE
) Engine=InnoDB;
SQL

#-------------------------------------------------- PowerDNS
log "configuring PowerDNS"

# pdns must NOT bind to 0.0.0.0:53 on Ubuntu 24.04 because systemd-resolved is
# already listening on 127.0.0.53:53 and 127.0.0.54:53 (the stub resolver).
# Binding to 0.0.0.0 collides with those and pdns fails to start with
# "Unable to bind UDP socket to '0.0.0.0:53': Address already in use".
# Bind to the public IPv4 + 127.0.0.1 explicitly instead. $PUBLIC_IP was
# detected near the top of this script.
if [[ -z "$PUBLIC_IP" ]]; then
  warn "no public IPv4 detected for pdns local-address; falling back to 127.0.0.1 only (pdns will only answer locally)"
  PDNS_LOCAL_ADDRESS="127.0.0.1"
else
  PDNS_LOCAL_ADDRESS="$PUBLIC_IP,127.0.0.1"
fi
# IPv6 too. Once the server has a published IPv6 address, zones get AAAA
# records for their own ns1/ns2 (agent/src/handlers/dns.rs), and a name server
# announced over IPv6 has to answer there: pdns did not. `::` is every IPv6
# address the server has or gets later, and cannot collide with
# systemd-resolved, which listens on IPv4 only. Not on a kernel without IPv6.
if [[ -e /proc/net/if_inet6 ]]; then
  PDNS_LOCAL_ADDRESS="$PDNS_LOCAL_ADDRESS,::"
fi

# Holds the database password: never readable by others, not even between
# creating it and the chmod below.
install -m 0640 -o root -g pdns /dev/null /etc/powerdns/pdns.conf
cat > /etc/powerdns/pdns.conf <<EOF
launch=gmysql
gmysql-host=127.0.0.1
gmysql-dbname=pdns
gmysql-user=pdns
gmysql-password=$PDNS_PASS
local-address=$PDNS_LOCAL_ADDRESS
local-port=53
setuid=pdns
setgid=pdns
disable-axfr=no
master=yes
slave=yes
api=no
webserver=no
default-soa-content=ns1.@ hostmaster.@ 0 10800 3600 604800 3600
EOF
chmod 0640 /etc/powerdns/pdns.conf
chown root:pdns /etc/powerdns/pdns.conf
# Something else on [::]:53 must not leave the server without DNS: fall back
# to IPv4 only.
if ! systemctl restart pdns; then
  if [[ "$PDNS_LOCAL_ADDRESS" == *",::" ]]; then
    warn "pdns did not start with an IPv6 listener; using IPv4 only"
    sed -i "s|^local-address=.*|local-address=${PDNS_LOCAL_ADDRESS%,::}|" /etc/powerdns/pdns.conf
  fi
  systemctl restart pdns
fi

#-------------------------------------------------- Postfix
log "configuring Postfix"
postconf -e "myhostname = $MAIL_HOST"
postconf -e "smtpd_banner = \$myhostname ESMTP"
postconf -e "biff = no"
postconf -e "append_dot_mydomain = no"
postconf -e "readme_directory = no"
postconf -e "compatibility_level = 2"
# Snakeoil only until the agent installs a real certificate: a re-run used to
# put it back over the Let's Encrypt one, so mail clients got certificate
# errors until the next issuance.
CURRENT_SMTPD_CERT="$(postconf -h smtpd_tls_cert_file 2>/dev/null || true)"
if [[ -z "$CURRENT_SMTPD_CERT" || ! -f "$CURRENT_SMTPD_CERT" ]]; then
  postconf -e "smtpd_tls_cert_file=/etc/ssl/certs/ssl-cert-snakeoil.pem"
  postconf -e "smtpd_tls_key_file=/etc/ssl/private/ssl-cert-snakeoil.key"
fi
postconf -e "smtpd_use_tls=yes"
postconf -e "smtpd_tls_session_cache_database = btree:\${data_directory}/smtpd_scache"
postconf -e "smtp_tls_session_cache_database = btree:\${data_directory}/smtp_scache"
postconf -e "alias_maps = hash:/etc/aliases"
postconf -e "alias_database = hash:/etc/aliases"
# Always: the postfix package lists the hostname here, and a name that is
# also a mail domain of the panel must not be a local destination.
postconf -e "mydestination = localhost"
# A re-run keeps a smarthost (relayhost) and the mynetworks list main.cf
# already has. Both used to be reset on every run: outgoing mail went
# straight out again instead of through the operator's relay, and networks
# the operator had allowed to relay were dropped. The postfix package writes
# an empty relayhost and the same mynetworks as below, so a first install
# ends up as before.
if [[ -z "$(postconf -nh relayhost 2>/dev/null || true)" ]]; then
  postconf -e "relayhost ="
fi
if [[ -z "$(postconf -nh mynetworks 2>/dev/null || true)" ]]; then
  postconf -e "mynetworks = 127.0.0.0/8 [::ffff:127.0.0.0]/104 [::1]/128"
fi
postconf -e "mailbox_size_limit = 0"
postconf -e "recipient_delimiter = +"
postconf -e "inet_interfaces = all"
# Both protocols, unless main.cf already says which (the postfix package
# writes "all"): a re-run used to put "all" back over an operator's choice.
# What Postfix SENDS over is decided further down, not here.
if [[ -z "$(postconf -nh inet_protocols 2>/dev/null || true)" ]]; then
  postconf -e "inet_protocols = all"
fi
postconf -e "virtual_transport = lmtp:unix:private/dovecot-lmtp"
postconf -e "virtual_mailbox_domains = mysql:/etc/postfix/mysql-virtual-mailbox-domains.cf"
postconf -e "virtual_mailbox_maps = mysql:/etc/postfix/mysql-virtual-mailbox-maps.cf"
postconf -e "virtual_alias_maps = mysql:/etc/postfix/mysql-virtual-alias-maps.cf"
postconf -e "smtpd_sasl_type = dovecot"
postconf -e "smtpd_sasl_path = private/auth"
# No SMTP AUTH on port 25: it is for servers delivering mail, clients
# submit on 587/465 (master.cf turns AUTH on there). Password guessing on
# 25 was never banned: the dovecot jail's ports leave out 25.
postconf -e "smtpd_sasl_auth_enable = no"
# A logged-in mailbox may only send as itself or as an alias that delivers
# to it. Without this any mailbox could send as any other tenant's address,
# and Rspamd DKIM-signed it for that domain.
postconf -e "smtpd_sender_login_maps = mysql:/etc/postfix/mysql-sender-login-maps.cf"
postconf -e "smtpd_sender_restrictions = reject_authenticated_sender_login_mismatch"
postconf -e "smtpd_recipient_restrictions = permit_mynetworks, permit_sasl_authenticated, reject_unauth_destination"
postconf -e "smtpd_milters = inet:127.0.0.1:11332"
postconf -e "non_smtpd_milters = inet:127.0.0.1:11332"
postconf -e "milter_protocol = 6"
postconf -e "milter_default_action = accept"

# Outgoing mail: IPv4 first, and at first IPv4 only. Most VPS providers set the
# IPv4 PTR correctly but leave the IPv6 one unset, and Gmail answers mail from
# an IPv6 address without a matching PTR with a permanent 5.7.25. Preferring
# IPv4 was not enough: when a destination greylists or is slow over IPv4,
# Postfix falls back to IPv6 within the same delivery, from an address no
# AAAA, SPF or PTR record vouches for, and the mail bounces. So the sending
# services are limited to IPv4 here (receiving still uses inet_protocols
# above), and the agent lifts that once the server has a published IPv6
# address that passes its check, pinning Postfix to that address
# (agent/src/handlers/mail_ip.rs; the operator chooses the protocol in the
# panel). A re-run keeps what was chosen since: the preference when main.cf
# has one, and IPv6 sending when the agent switched it on (smtp_bind_address6
# is set then). Not on a server without any IPv4 address: limited to IPv4 it
# could not send at all.
if [[ -z "$(postconf -nh smtp_address_preference 2>/dev/null || true)" ]]; then
  postconf -e "smtp_address_preference = ipv4"
fi
if [[ -n "$PUBLIC_IP" && -z "$(postconf -nh smtp_bind_address6 2>/dev/null || true)" ]]; then
  for svc in smtp relay; do
    if [[ -n "$(postconf -M "$svc/unix" 2>/dev/null || true)" ]]; then
      postconf -P "$svc/unix/inet_protocols=ipv4"
    fi
  done
fi

# These hold the mail database password: created unreadable to others before
# anything is written into them (they were world-readable until the chmod
# below).
for f in mysql-virtual-mailbox-domains.cf mysql-virtual-mailbox-maps.cf \
         mysql-virtual-alias-maps.cf mysql-sender-login-maps.cf; do
  install -m 0640 -o root -g postfix /dev/null "/etc/postfix/$f"
done
cat > /etc/postfix/mysql-virtual-mailbox-domains.cf <<EOF
user = mail
password = $MAIL_PASS
hosts = 127.0.0.1
dbname = mail
query = SELECT 1 FROM virtual_domains WHERE name='%s'
EOF
cat > /etc/postfix/mysql-virtual-mailbox-maps.cf <<EOF
user = mail
password = $MAIL_PASS
hosts = 127.0.0.1
dbname = mail
query = SELECT 1 FROM virtual_users WHERE email='%s'
EOF
# Forwarders (Mailboxes > Forwarders). A mailbox's own address resolves to
# itself too, so a forwarder on a mailbox keeps a copy there and a catch-all
# (@domain) never takes a real mailbox's mail.
cat > /etc/postfix/mysql-virtual-alias-maps.cf <<EOF
user = mail
password = $MAIL_PASS
hosts = 127.0.0.1
dbname = mail
query = SELECT destination FROM virtual_aliases WHERE source='%s' UNION SELECT email FROM virtual_users WHERE email='%s'
EOF
cat > /etc/postfix/mysql-sender-login-maps.cf <<EOF
user = mail
password = $MAIL_PASS
hosts = 127.0.0.1
dbname = mail
query = SELECT email FROM virtual_users WHERE email='%s' UNION SELECT destination FROM virtual_aliases WHERE source='%s'
EOF
chmod 0640 /etc/postfix/mysql-virtual-*.cf /etc/postfix/mysql-sender-login-maps.cf
chown root:postfix /etc/postfix/mysql-virtual-*.cf /etc/postfix/mysql-sender-login-maps.cf

# enable submission (587) and smtps (465) in master.cf
postconf -M submission/inet="submission inet n - y - - smtpd"
postconf -P "submission/inet/syslog_name=postfix/submission"
postconf -P "submission/inet/smtpd_tls_security_level=encrypt"
postconf -P "submission/inet/smtpd_sasl_auth_enable=yes"
postconf -P "submission/inet/smtpd_relay_restrictions=permit_sasl_authenticated,reject"
postconf -M smtps/inet="smtps inet n - y - - smtpd"
postconf -P "smtps/inet/syslog_name=postfix/smtps"
postconf -P "smtps/inet/smtpd_tls_wrappermode=yes"
postconf -P "smtps/inet/smtpd_sasl_auth_enable=yes"
postconf -P "smtps/inet/smtpd_relay_restrictions=permit_sasl_authenticated,reject"

systemctl restart postfix

#-------------------------------------------------- Dovecot
log "configuring Dovecot"
# home is a plain `home` dir inside the maildir, not the maildir itself: there,
# Sieve's files and Out of Office's reply log (.dovecot.sieve,
# .dovecot.lda-dupes.locks) showed up as folders and broke filing into folders.
# Created unreadable to others first: it holds the mail database password.
install -m 0640 -o root -g dovecot /dev/null /etc/dovecot/dovecot-sql.conf.ext
cat > /etc/dovecot/dovecot-sql.conf.ext <<EOF
driver = mysql
connect = host=127.0.0.1 dbname=mail user=mail password=$MAIL_PASS
default_pass_scheme = SHA512-CRYPT
password_query = SELECT email as user, password FROM virtual_users WHERE email='%u';
user_query = SELECT 5000 AS uid, 5000 AS gid, '/var/vmail/%d/%n/home' AS home, concat('*:bytes=', quota_mb, 'M') AS quota_rule FROM virtual_users WHERE email='%u';
EOF
chmod 0640 /etc/dovecot/dovecot-sql.conf.ext
chown root:dovecot /etc/dovecot/dovecot-sql.conf.ext

cat > /etc/dovecot/conf.d/99-vps-panel.conf <<'EOF'
mail_location = maildir:/var/vmail/%d/%n
mail_privileged_group = mail
ssl = yes
disable_plaintext_auth = yes
auth_mechanisms = plain login
passdb {
  driver = sql
  args = /etc/dovecot/dovecot-sql.conf.ext
}
userdb {
  driver = sql
  args = /etc/dovecot/dovecot-sql.conf.ext
}
service auth {
  unix_listener /var/spool/postfix/private/auth {
    mode = 0666
    user = postfix
    group = postfix
  }
}
service lmtp {
  unix_listener /var/spool/postfix/private/dovecot-lmtp {
    mode = 0600
    user = postfix
    group = postfix
  }
}
protocol lmtp {
  postmaster_address = postmaster@PANEL_HOST_PLACEHOLDER
}
EOF
sed -i "s/PANEL_HOST_PLACEHOLDER/$MAIL_HOST/g" /etc/dovecot/conf.d/99-vps-panel.conf

# Mailbox passwords live in SQL only, so drop Ubuntu's PAM passdb
# (auth-system.conf.ext, included by 10-auth.conf and tried BEFORE the SQL one).
# With it, every mailbox login first logs a pam_unix "authentication failure"
# and fail2ban's dovecot filter counts that line: a busy but legitimate client
# (an office behind one IP, Outlook's cloud sync servers) crosses maxretry on
# ordinary logins and is banned from IMAP AND submission. It also let the mail
# ports test Linux account passwords, root's included.
sed -i 's/^!include auth-system.conf.ext/#!include auth-system.conf.ext/' /etc/dovecot/conf.d/10-auth.conf
# Without PAM, a wrong password on SMTP AUTH is only logged when auth_verbose is
# on ("auth-worker<n>: sql(user,ip): Password mismatch" / "unknown user"), and that is the
# line fail2ban's dovecot filter matches to keep banning SASL brute force.
echo 'auth_verbose = yes' > /etc/dovecot/conf.d/96-vps-panel-auth.conf
chmod 0644 /etc/dovecot/conf.d/96-vps-panel-auth.conf

# Quota plugin + 80%/90% warnings.
# Loads the quota plugin (without which the quota_rule returned by user_query
# is ignored), enforces over-quota with a 552 bounce, and runs the warning
# script when a delivery crosses 80% or 90%.
cat > /etc/dovecot/conf.d/90-vps-panel-quota.conf <<'EOF'
mail_plugins = $mail_plugins quota

protocol imap {
  mail_plugins = $mail_plugins quota imap_quota
}
protocol lmtp {
  mail_plugins = $mail_plugins quota
}

plugin {
  quota = maildir:User quota
  quota_status_success    = DUNNO
  quota_status_nouser     = DUNNO
  quota_status_overquota  = "552 5.2.2 Mailbox is full"
  quota_grace             = 10%%
  quota_warning  = storage=80%% quota-warning 80 %u
  quota_warning2 = storage=90%% quota-warning 90 %u
}

service quota-warning {
  executable = script /usr/local/bin/vps-panel-quota-warning.sh
  user = vmail
  unix_listener quota-warning {
    user = vmail
  }
}
EOF
chmod 0644 /etc/dovecot/conf.d/90-vps-panel-quota.conf
chown root:root /etc/dovecot/conf.d/90-vps-panel-quota.conf

# Filters and Out of Office: LMTP runs each mailbox's Sieve script on delivery,
# and webmail edits it over ManageSieve (dovecot-managesieved, port 4190; ufw
# opens it to Docker's networks only, for the Roundcube container).
cat > /etc/dovecot/conf.d/91-vps-panel-sieve.conf <<'EOF'
# Managed by vps-panel: Sieve filters and Out of Office for every mailbox,
# edited in webmail (Roundcube's managesieve plugin, port 4190).
protocol lmtp {
  mail_plugins = $mail_plugins sieve
}
plugin {
  sieve = file:~/sieve;active=~/.dovecot.sieve
}
EOF
chmod 0644 /etc/dovecot/conf.d/91-vps-panel-sieve.conf

# Standard folders and spam filing. Every mailbox gets Drafts, Sent, Trash and
# Junk, created and subscribed on its first login or delivery, with the
# special-use flags mail clients look for. Rspamd tags likely spam with
# `X-Spam: Yes` (removing any copy the sender added) and a global Sieve script,
# run before the mailbox's own filters, files it into Junk and stops there, so
# those filters and Out of Office never act on spam. update.sh::enable_junk_folder
# writes the same two files on boxes installed before this.
install -d -m 0755 /etc/dovecot/sieve
cat > /etc/dovecot/sieve/vps-panel-spam.sieve <<'EOF'
# Managed by vps-panel: mail Rspamd tagged as spam goes to Junk. Runs before
# each mailbox's own Sieve script; stop keeps that script (filters, Out of
# Office) from acting on spam.
require ["fileinto", "mailbox"];
if header :is "X-Spam" "Yes" {
  fileinto :create "Junk";
  stop;
}
EOF
cat > /etc/dovecot/conf.d/92-vps-panel-junk.conf <<'EOF'
# Managed by vps-panel: standard folders for every mailbox, and spam into Junk.
namespace inbox {
  mailbox Drafts {
    auto = subscribe
    special_use = \Drafts
  }
  mailbox Sent {
    auto = subscribe
    special_use = \Sent
  }
  mailbox Trash {
    auto = subscribe
    special_use = \Trash
  }
  mailbox Junk {
    auto = subscribe
    special_use = \Junk
  }
}
plugin {
  sieve_before = /etc/dovecot/sieve/vps-panel-spam.sieve
}
EOF
# Compiled here because LMTP runs as the mailbox user and cannot write the
# binary into /etc/dovecot/sieve.
sievec /etc/dovecot/sieve/vps-panel-spam.sieve
chmod 0644 /etc/dovecot/sieve/vps-panel-spam.sieve /etc/dovecot/sieve/vps-panel-spam.svbin \
  /etc/dovecot/conf.d/92-vps-panel-junk.conf

# Warning delivery script. Invoked by Dovecot's quota plugin with two args:
# the percentage threshold that was crossed (80 or 90) and the mailbox.
# `noenforcing` on the dovecot-lda quota lets the warning itself be delivered
# even when the mailbox is already over quota - otherwise the warning bounces.
cat > /usr/local/bin/vps-panel-quota-warning.sh <<EOF
#!/bin/sh
PERCENT="\$1"
USER="\$2"
cat <<MSG | /usr/lib/dovecot/dovecot-lda -d "\$USER" -o "plugin/quota=maildir:User quota:noenforcing"
From: postmaster@$MAIL_HOST
To: \$USER
Subject: [Mailbox] \$PERCENT% full - please clear old messages
Content-Type: text/plain; charset=UTF-8

Your mailbox \$USER is now \$PERCENT% full.

Please delete old or large messages, or ask the administrator for more
quota. When the mailbox reaches 100% new incoming mail will bounce.

-- vps-panel
MSG
EOF
chmod 0755 /usr/local/bin/vps-panel-quota-warning.sh
chown root:root /usr/local/bin/vps-panel-quota-warning.sh

systemctl restart dovecot

#-------------------------------------------------- Rspamd DKIM signing
log "configuring Rspamd DKIM"
install -d -m 0750 -o _rspamd -g _rspamd /var/lib/rspamd/dkim
cat > /etc/rspamd/local.d/dkim_signing.conf <<EOF
enabled = true;
selector = "default";
path = "/var/lib/rspamd/dkim/\$domain.\$selector.key";
# Sign only when the logged-in user's domain is the From domain, so a
# mailbox cannot get another tenant's domain signed.
allow_username_mismatch = false;
allow_envfrom_empty = true;
EOF
systemctl restart rspamd

#-------------------------------------------------- UFW + fail2ban
log "configuring UFW"
# A re-run keeps the operator's rules. The reset here used to wipe every
# port opened since (apps, a public database) and the rule for an SSH port
# moved in Settings, locking SSH out.
# The service ports (web, mail, DNS) are opened once: when ufw was not active
# before this run, or was but the panel has never been installed here (its
# unit is written near the end of a first run). A re-run used to open them
# all again, also the ones the operator had closed since.
UFW_OPEN_SERVICES=0
if ! ufw status 2>/dev/null | grep -q "Status: active"; then
  UFW_OPEN_SERVICES=1
  ufw --force reset >/dev/null
  ufw default deny incoming
  ufw default allow outgoing
elif [[ ! -f /etc/systemd/system/vps-panel.service ]]; then
  UFW_OPEN_SERVICES=1
fi
# The port(s) SSH really listens on: sshd's own (sshd -T needs /run/sshd), a
# port set on ssh.socket (a drop-in there wins over what sshd -T reports) and
# the one this session came in on. With only sshd -T, a port moved on
# ssh.socket stayed closed and the operator was locked out. `|| true`: with
# pipefail a failing sshd -T ended the installer here instead of falling
# back to 22.
install -d -m 0755 /run/sshd
SSH_SESSION="${SSH_CONNECTION:-}"
SSH_PORTS="$(
  {
    sshd -T 2>/dev/null | awk '$1=="port"{print $2}'
    if systemctl is-active --quiet ssh.socket 2>/dev/null; then
      systemctl show ssh.socket -p Listen 2>/dev/null | sed -n 's/.*:\([0-9][0-9]*\) (Stream)$/\1/p'
    fi
    echo "${SSH_SESSION##* }"
  } | grep -E '^[0-9]+$' | sort -un || true
)"
# Always, also on a re-run: without its rule SSH is locked out.
for p in ${SSH_PORTS:-22}; do
  ufw allow "$p/tcp" comment 'ssh'
done
if [[ $UFW_OPEN_SERVICES -eq 1 ]]; then
  ufw allow 'Nginx Full'
  ufw allow 25/tcp comment 'smtp in'
  ufw allow 465/tcp comment 'smtps'
  ufw allow 587/tcp comment 'submission'
  ufw allow 993/tcp comment 'imaps'
  ufw allow 995/tcp comment 'pop3s'
  ufw allow 53/tcp comment 'dns tcp'
  ufw allow 53/udp comment 'dns udp'
else
  log "keeping the firewall rules for web, mail and DNS as they are"
fi
# ManageSieve for the Roundcube container only (Docker's 172.16.0.0/12). Not
# a public port, and webmail's filters need it: added on every run, as
# update.sh adds it back when it is missing.
ufw allow from 172.16.0.0/12 to any port 4190 proto tcp comment 'managesieve (webmail)'
ufw --force enable

log "configuring fail2ban"
cat > /etc/fail2ban/jail.d/zz-vps-panel.conf <<'EOF'
[DEFAULT]
banaction = nftables
backend = systemd
bantime = 1h
findtime = 10m
maxretry = 5
# Never ban loopback, RFC1918, or Docker bridge ranges. The panel runs
# Roundcube (and may run other clients) inside Docker; from Dovecot's
# point of view those connect from 172.17.0.x and hitting the brute-force
# threshold would lock the whole container out of IMAP.
ignoreip = 127.0.0.1/8 ::1 10.0.0.0/8 172.16.0.0/12 192.168.0.0/16

[sshd]
enabled = true

[postfix-sasl]
enabled = true
# Ubuntu runs Postfix as postfix@-.service; the stock filter only reads
# postfix.service, so this jail never saw a failed login.
journalmatch = _SYSTEMD_UNIT=postfix@-.service + _SYSTEMD_UNIT=postfix.service

[dovecot]
enabled = true
# IMAP clients open multiple parallel connections per refresh (iOS Mail in
# particular opens 4-8 at a time, one per mailbox folder + IDLE). The
# [DEFAULT] maxretry=5 / bantime=1h above is hostile to that pattern: a
# single misconfigured client can burn 5 strikes in seconds and lock the
# operator out for an hour while they try to fix it. The botnet brute-force
# from /16 ranges hits hundreds of attempts per minute, so 10 attempts in
# 10 minutes still catches them; legitimate clients on flaky networks have
# room to retry. Ban duration is dropped to 10 minutes so even a worst-case
# operator lockout self-heals quickly.
maxretry = 10
findtime = 600
bantime  = 600
EOF
systemctl restart fail2ban

#-------------------------------------------------- unattended-upgrades, chrony, sysctl
log "configuring unattended-upgrades"
cat > /etc/apt/apt.conf.d/52unattended-upgrades-local <<'EOF'
Unattended-Upgrade::Automatic-Reboot "true";
Unattended-Upgrade::Automatic-Reboot-Time "04:00";
EOF
dpkg-reconfigure --priority=low --frontend=noninteractive unattended-upgrades || true

log "switching NTP to chrony"
systemctl disable --now systemd-timesyncd 2>/dev/null || true
systemctl enable --now chrony

log "applying sysctl tweaks"
cat > /etc/sysctl.d/99-vps-panel.conf <<'EOF'
net.ipv4.tcp_syncookies=1
net.ipv4.conf.all.rp_filter=1
kernel.kptr_restrict=2
kernel.dmesg_restrict=1
fs.protected_hardlinks=1
fs.protected_symlinks=1
EOF
# Not fatal: in a container (LXC, OpenVZ) the kernel.* keys are read-only and
# sysctl exits non-zero, which ended the installer before the panel was built.
sysctl --system >/dev/null || warn "some sysctl settings could not be applied (normal inside a container)"

log "hardening journald"
mkdir -p /var/log/journal
sed -i 's/#Storage=auto/Storage=persistent/; s/#SystemMaxUse=.*/SystemMaxUse=1G/' /etc/systemd/journald.conf
systemctl restart systemd-journald

# /root/.ssh (the agent's keypairs) and /root/.npm (npm's cache, for build
# steps that shell out to a child `npm install`) exist before the agent
# starts. The unit used to whitelist both with ReadWritePaths=, which only
# pins a path that is there at start; it no longer sandboxes /root at all.
install -d -m 0700 -o root -g root /root/.ssh
install -d -m 0755 -o root -g root /root/.npm

log "configuring SSH (password + root login allowed by default)"
# Deliberately leave password auth + root login enabled on a fresh install.
# Some Ubuntu cloud images ship with PasswordAuthentication=no out of the
# box and many users provision without uploading an SSH key, which used to
# silently lock them out of their own VPS the moment install.sh finished.
# The panel exposes Settings -> SSH server to toggle these off once the
# operator has added a key under /ssh-keys (the right long-term posture).
#
# fail2ban (configured above) still blunts brute-force attempts via the
# sshd jail, and the panel's Firewall page lets the operator tighten the
# bantime/maxretry policy further.
#
# The values go in the panel's own drop-in, the file Settings -> SSH server
# edits. Ubuntu's sshd_config includes sshd_config.d/*.conf at the top and
# sshd keeps the first value it reads, so 00-vps-panel.conf beats the cloud
# image drop-ins (50-cloud-init.conf, 60-cloudimg-settings.conf) and
# sshd_config itself. Written only when missing: re-running the installer
# keeps whatever the operator chose since.
install -d -m 0755 /etc/ssh/sshd_config.d
if [[ ! -f /etc/ssh/sshd_config.d/00-vps-panel.conf ]]; then
  cat > /etc/ssh/sshd_config.d/00-vps-panel.conf <<'EOF'
# Managed by vps-panel (Settings > SSH server). Sorts first in
# sshd_config.d, so these values win over the other files.
PermitRootLogin yes
PasswordAuthentication yes
KbdInteractiveAuthentication yes
EOF
  chmod 0644 /etc/ssh/sshd_config.d/00-vps-panel.conf
fi
if ! grep -qE '^[[:space:]]*Include[[:space:]]+/etc/ssh/sshd_config\.d/\*\.conf' /etc/ssh/sshd_config; then
  sed -i '1i Include /etc/ssh/sshd_config.d/*.conf' /etc/ssh/sshd_config
fi
# sshd -t needs /run/sshd, which a socket-activated ssh.service only creates
# on the first connection.
install -d -m 0755 /run/sshd
if sshd -t; then
  systemctl try-reload-or-restart ssh || systemctl try-reload-or-restart sshd || true
else
  warn "sshd -t rejected the SSH config; sshd was not reloaded"
fi

#-------------------------------------------------- build agent + panel
log "building Rust agent (release)"
cd "$REPO_DIR"
cargo build --release --locked --manifest-path agent/Cargo.toml
install -m 0755 -o root -g root \
  "$REPO_DIR/target/release/vps-panel-agent" \
  /usr/local/bin/vps-panel-agent

log "installing panel files into $APP_DIR"
rsync -a --delete \
  --exclude=node_modules --exclude=.next --exclude=target --exclude=.git \
  --exclude=.env.local \
  "$REPO_DIR/" "$APP_DIR/"

# rsync runs as root and preserves source ownership, so the tree lands as
# root-owned. Flip it to vps-panel BEFORE npm so `sudo -u vps-panel npm ci`
# can create node_modules/ and .next/. Doing this after npm is too late --
# the npm step fails with EACCES on mkdir node_modules, the `|| npm ci`
# fallback we used to have masked it by re-running as root, and you ended
# up with a half-root, half-vps-panel tree that update.sh later trips on.
chown -R vps-panel:vps-panel "$APP_DIR"

cd "$APP_DIR"

log "installing Node deps and building Next.js"
sudo -u vps-panel HOME="$VAR_DIR" npm ci --no-audit --no-fund
# NEXT_TELEMETRY_DISABLED: the Next.js build tool otherwise reports each build
# (versions, machine size, page counts) to telemetry.nextjs.org.
sudo -u vps-panel HOME="$VAR_DIR" NEXT_TELEMETRY_DISABLED=1 npm run build

# standalone output: copy the static and public assets into .next/standalone
# alongside server.js. Run as vps-panel so the copied files inherit the same
# ownership as the rest of .next/ (which npm just built as that user).
if [[ -d .next/standalone ]]; then
  sudo -u vps-panel cp -r public .next/standalone/public 2>/dev/null || true
  sudo -u vps-panel cp -r .next/static .next/standalone/.next/static
fi

#-------------------------------------------------- systemd units
log "installing systemd units"
install -m 0644 "$REPO_DIR/scripts/systemd/vps-panel-agent.service" \
  /etc/systemd/system/vps-panel-agent.service
install -m 0644 "$REPO_DIR/scripts/systemd/vps-panel.service" \
  /etc/systemd/system/vps-panel.service
systemctl daemon-reload

#-------------------------------------------------- nginx vhost for the panel
log "installing nginx vhost (server_name=$NGINX_SERVER_NAME)"
# Settings > Panel access writes the allowlist here; the vhost includes it.
if [[ ! -f /etc/nginx/vps-panel-access.conf ]]; then
  echo "# Managed by vps-panel (Settings > Panel access). No entries: every address may reach the panel." \
    > /etc/nginx/vps-panel-access.conf
fi
# server_name=_ in IP-only mode means "match any Host header". Also add
# `default_server` so this vhost wins against the distro default (which we
# unlink below anyway). The agent's panel-host change endpoint re-renders
# this file later when the operator sets a real FQDN.
# default_server with a domain too: the panel must answer by IP until the
# domain's DNS points here, or a --domain install is unreachable.
LISTEN_DIRECTIVE="listen 80 default_server;\n    listen [::]:80 default_server;"
# Written only on the first install. After that the agent owns the file
# (hostname changes and SSL issuance re-render it): a re-run used to put the
# HTTP-only template back over the HTTPS vhost while .env still said
# COOKIE_SECURE=1, so nobody could log in.
if [[ -f /etc/nginx/sites-available/vps-panel.conf ]]; then
  log "keeping the existing panel vhost"
else
  sed -e "s|__PANEL_HOST__|$NGINX_SERVER_NAME|g" \
      -e "s|__ACME_WEBROOT__|$ACME_WEBROOT|g" \
      -e "s|__LISTEN__|$LISTEN_DIRECTIVE|g" \
    "$REPO_DIR/scripts/nginx/panel.conf.template" \
    > /etc/nginx/sites-available/vps-panel.conf
fi
ln -sf /etc/nginx/sites-available/vps-panel.conf /etc/nginx/sites-enabled/vps-panel.conf
rm -f /etc/nginx/sites-enabled/default
nginx -t
systemctl reload nginx

#-------------------------------------------------- start services
log "enabling agent and panel"
# restart, not `enable --now`: on a re-run both are already running, and
# `--now` left the old agent and the old panel process on the new files.
systemctl enable vps-panel-agent.service vps-panel.service
systemctl restart vps-panel-agent.service
sleep 1
systemctl restart vps-panel.service
sleep 3
# Type=simple: restart succeeds even when the process exits right after.
for unit in vps-panel-agent.service vps-panel.service; do
  if ! systemctl is-active --quiet "$unit"; then
    warn "$unit is not running. Check: journalctl -u $unit -n 50"
  fi
done

#-------------------------------------------------- summary
log "panel ready"
if [[ -n "$PANEL_HOST" ]]; then
  PANEL_URL="http://$PANEL_HOST"
  HOSTNAME_NOTE="HTTPS: issue a cert from Settings -> Issue cert after DNS is live."
else
  PANEL_URL="http://${PUBLIC_IP:-<server-ip>}"
  HOSTNAME_NOTE="No domain set -- accessible by IP over HTTP. Set a real hostname from the wizard's 'Hostname' step or Settings to enable HTTPS."
fi
cat <<EOF

  Panel URL    : $PANEL_URL
  Admin email  : $(grep ^ADMIN_EMAIL= "$ENV_FILE" | cut -d= -f2-)
  Admin pass   : $(grep ^ADMIN_INITIAL_PASSWORD= "$ENV_FILE" | cut -d= -f2-)

  $HOSTNAME_NOTE

  First login lands on /setup -- a one-time wizard that walks through hostname,
  service health, Let's Encrypt contact email, nameserver pair, fail2ban
  policy, optional apps, and finally a password change. The dashboard is
  gated until setup is finished.

  The email and password above are the initial ones, from $ENV_FILE. The
  panel reads them only while it has no admin; after that, change them in
  the panel. A lost password: vps-panel-agent admin-reset --email <login email>

  Logs:
    journalctl -u vps-panel-agent.service -f
    journalctl -u vps-panel.service -f

EOF
