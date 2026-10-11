#!/usr/bin/env bash
set -euo pipefail

if [[ $EUID -ne 0 ]]; then
  echo "must run as root" >&2
  exit 1
fi

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
INSTALL_DIR="/opt/vps-panel"
APP_DIR="$INSTALL_DIR/app"

# install.sh adds cargo via rustup into /root/.cargo/bin but doesn't update
# root's profile, so a non-login `bash scripts/update.sh` won't find it.
# Prepend the cargo bin dir before any cargo invocation.
export PATH="/root/.cargo/bin:$PATH"

cd "$REPO_DIR"
git pull --ff-only

# NOTE: deliberately NOT copying any .env file from the repo. /etc/vps-panel/.env
# is the deployment's source of truth — install.sh generates it on first run
# with unique secrets and the operator's PANEL_HOST. update.sh must never
# clobber it (an earlier version did, with dev-only .env.local from the repo,
# which wiped JWT_SECRET / AGENT_TOKEN / PANEL_HOST on every update).

# ---------------------------------------------------------------------------
# Build everything BEFORE changing the live install. Any failure up to the end
# of this section leaves the running agent binary, units and panel as they
# were. The panel used to be rsynced and rebuilt in place, so a failed
# `npm ci` / `next build` (registry blip, broken commit) left it with no build
# and down until someone fixed it over SSH.
# ---------------------------------------------------------------------------

cargo build --release --locked --manifest-path agent/Cargo.toml

# The new panel is built at its real path, because Next.js output can embed the
# directory it was built in. The running install is moved aside to $PREV_DIR
# first and put back if the build fails. The old process keeps running
# throughout (its restart is the last step below); pages it has not loaded yet
# may error until then, as they did with the in-place build.
PREV_DIR="$INSTALL_DIR/app.prev"

build_panel() {
  # Match install.sh: panel files live under /opt/vps-panel/app, not directly
  # under /opt/vps-panel. systemd's WorkingDirectory points at /opt/vps-panel/app.
  # A && chain rather than `set -e`, which bash ignores inside `if ! build_panel`.
  rsync -a --delete \
    --exclude=node_modules --exclude=.next --exclude=target --exclude=.git \
    --exclude=.env.local \
    "$REPO_DIR/" "$APP_DIR/" &&
  # The live install's untracked .env.local (rsync never touched it before).
  { [[ ! -f "$PREV_DIR/.env.local" ]] || cp -a "$PREV_DIR/.env.local" "$APP_DIR/.env.local"; } &&
  # Flip ownership to vps-panel BEFORE npm. rsync ran as root with -a, so the
  # tree just landed root-owned; without this chown `sudo -u vps-panel npm ci`
  # fails with EACCES on node_modules.
  chown -R vps-panel:vps-panel "$APP_DIR" &&
  (cd "$APP_DIR" && sudo -u vps-panel HOME=/var/lib/vps-panel npm ci --no-audit --no-fund) &&
  (cd "$APP_DIR" && sudo -u vps-panel HOME=/var/lib/vps-panel npm run build) &&
  (
    cd "$APP_DIR" &&
    if [[ -d .next/standalone ]]; then
      { sudo -u vps-panel cp -r public .next/standalone/public 2>/dev/null || true; } &&
      sudo -u vps-panel cp -r .next/static .next/standalone/.next/static
    fi
  ) &&
  # A "successful" build that produced nothing must not replace a working one.
  [[ -d "$APP_DIR/.next" ]]
}

rm -rf "$PREV_DIR"
if [[ -d "$APP_DIR" ]]; then
  mv "$APP_DIR" "$PREV_DIR"
fi
if ! build_panel; then
  echo "panel build failed - restoring the previous panel install; nothing else was changed" >&2
  rm -rf "$APP_DIR"
  if [[ -d "$PREV_DIR" ]]; then
    mv "$PREV_DIR" "$APP_DIR"
  fi
  exit 1
fi
echo "new panel built in $APP_DIR (previous install kept in $PREV_DIR)"

# ---------------------------------------------------------------------------
# Both builds succeeded: install them.
# ---------------------------------------------------------------------------

install -m 0755 -o root -g root \
  "$REPO_DIR/target/release/vps-panel-agent" \
  /usr/local/bin/vps-panel-agent

# Move the ACME webroot out of /var/lib/vps-panel, then take www-data out of
# the vps-panel group. That group reads .env (JWT secret, agent token) and
# panel.db and connects to the root agent, so anything running as www-data
# could become root; nginx was only in it to reach the old webroot.
# Idempotent. If nginx rejects the rewritten vhosts they are restored and
# www-data keeps the group until the next update.
migrate_acme_webroot() {
  local new=/var/www/vps-panel-acme old=/var/lib/vps-panel/acme-webroot
  install -d -m 0755 -o root -g root "$new" "$new/.well-known" "$new/.well-known/acme-challenge"
  local files f stamp backup_dir=/var/backups/vps-panel-config
  files="$(grep -rlF "$old" /etc/nginx 2>/dev/null || true)"
  if [[ -n "$files" ]]; then
    stamp="$(date +%Y%m%d%H%M%S)"
    install -d -m 0700 "$backup_dir"
    for f in $files; do
      cp -a "$f" "$backup_dir/$(basename "$f").acme-$stamp"
      sed -i "s#$old#$new#g" "$f"
    done
    if nginx -t >/dev/null 2>&1; then
      systemctl reload nginx
      echo "vhosts now serve ACME challenges from $new"
    else
      echo "nginx -t failed after moving the ACME webroot - reverting" >&2
      for f in $files; do cp -a "$backup_dir/$(basename "$f").acme-$stamp" "$f"; done
      return 0
    fi
  fi
  if id -nG www-data 2>/dev/null | tr ' ' '\n' | grep -qx vps-panel; then
    gpasswd -d www-data vps-panel >/dev/null
    # New nginx workers pick up the changed group list on reload.
    systemctl reload nginx || true
    echo "removed www-data from the vps-panel group"
  fi
  return 0
}
migrate_acme_webroot || true

# Pre-create /root/.ssh so the agent's ReadWritePaths whitelist for it
# attaches at restart (ProtectHome=read-only would otherwise block any
# mkdir under /root from the agent). Idempotent.
install -d -m 0700 -o root -g root /root/.ssh

# Same reasoning for /root/.npm — npm's global package cache. Next.js and
# other framework "auto-install missing deps" features shell out to npm
# during builds, and that inner npm writes to ~/.npm/_cacache/. Without
# the dir existing at agent-restart time, ReadWritePaths can't pin it and
# every build that triggers an auto-install fails with EROFS.
install -d -m 0755 -o root -g root /root/.npm

install -m 0644 "$REPO_DIR/scripts/systemd/vps-panel-agent.service" \
  /etc/systemd/system/vps-panel-agent.service
install -m 0644 "$REPO_DIR/scripts/systemd/vps-panel.service" \
  /etc/systemd/system/vps-panel.service
systemctl daemon-reload

# Backfill the upload limits into the LIVE panel vhost.
#
# The vhost templates carry these, but nothing regenerates the file on update —
# it's only rewritten by set_panel_host and by SSL issuance. Without this an
# existing box keeps nginx's 1m default, so any Drive upload over a megabyte
# gets a 413 AND nginx stops reading the request, which the browser experiences
# as a hang rather than an error.
#
# Idempotent via the marker comment. Inserted after each `server_name` line,
# where both directives are valid at server scope.
patch_panel_vhost_upload_limits() {
  # Earlier versions left their .bak copies next to the vhosts.
  if compgen -G '/etc/nginx/sites-available/*.bak-*' >/dev/null; then
    install -d -m 0700 /var/backups/vps-panel-config
    mv /etc/nginx/sites-available/*.bak-* /var/backups/vps-panel-config/
  fi
  local vhost
  vhost="$(grep -rls '^# vps-panel-host:' /etc/nginx/sites-available/ 2>/dev/null | head -1)" || true
  if [[ -z "$vhost" ]]; then
    echo "no panel vhost found; skipping upload-limit backfill"
    return 0
  fi
  # Vhosts rendered from the current templates already have the upload
  # location without this marker; adding a second one failed nginx -t and
  # printed a revert warning on every update.
  if grep -q 'vps-panel-upload-limits\|location = /api/files/upload' "$vhost"; then
    return 0
  fi

  # Backups go outside sites-available, where the vhost scans (this one
  # included) would pick them up as vhosts.
  local backup_dir=/var/backups/vps-panel-config
  install -d -m 0700 "$backup_dir"
  local backup="$backup_dir/$(basename "$vhost").bak-$(date +%Y%m%d%H%M%S)"
  echo "adding upload limits to $vhost"
  cp -a "$vhost" "$backup"

  awk '
    { print }
    /^[[:space:]]*server_name[[:space:]]/ {
      print "";
      print "    # vps-panel-upload-limits (added by update.sh)";
      print "    client_max_body_size 25m;";
      print "";
      print "    location = /api/files/upload {";
      print "        proxy_pass http://127.0.0.1:10000;";
      print "        proxy_set_header Host $host;";
      print "        proxy_set_header X-Real-IP $remote_addr;";
      print "        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;";
      print "        proxy_set_header X-Forwarded-Proto $scheme;";
      print "        proxy_http_version 1.1;";
      print "        proxy_buffering off;";
      print "        proxy_request_buffering off;";
      print "        client_max_body_size 5g;";
      print "        proxy_read_timeout 3600s;";
      print "        proxy_send_timeout 3600s;";
      print "    }";
    }
  ' "$backup" > "$vhost"

  # A broken config must never survive this script.
  if nginx -t >/dev/null 2>&1; then
    systemctl reload nginx
    echo "upload limits applied; nginx reloaded"
  else
    echo "nginx -t failed after patching $vhost — reverting" >&2
    cp -a "$backup" "$vhost"
    nginx -t >/dev/null 2>&1 || echo "WARNING: nginx config still invalid" >&2
  fi
}
patch_panel_vhost_upload_limits

# My Networks: the other servers in a network reach /api/peer/ from any
# address, past Settings > Panel access (the panel checks each request's
# signature). New vhosts carry the location (the agent's templates and
# scripts/nginx/panel.conf.template); this adds it to an existing one, before
# every `location /` that proxies to the panel. Not to the HTTPS redirect
# block, which proxies nothing. Idempotent; reverted if nginx -t fails.
patch_panel_vhost_peer_api() {
  local vhost
  vhost="$(grep -rls '^# vps-panel-host:' /etc/nginx/sites-available/ 2>/dev/null | head -1)" || true
  [[ -n "$vhost" ]] || return 0
  grep -q 'location \^~ /api/peer/' "$vhost" && return 0

  local backup_dir=/var/backups/vps-panel-config
  install -d -m 0700 "$backup_dir"
  local backup="$backup_dir/$(basename "$vhost").bak-$(date +%Y%m%d%H%M%S)"
  echo "adding the My Networks location to $vhost"
  cp -a "$vhost" "$backup"

  awk '
    function peer(ind) {
      print ind "# My Networks: the other servers in a network reach this panel here";
      print ind "# from any address, whatever Settings > Panel access allows. The panel";
      print ind "# checks each request'"'"'s signature. Only plain /api/peer/<name> paths";
      print ind "# pass, so nothing else can be made to normalize into this location.";
      print ind "location ^~ /api/peer/ {";
      print ind "    allow all;";
      print ind "    if ($request_uri !~ \"^/api/peer/[a-z]+(\\?[A-Za-z0-9=&_-]*)?$\") {";
      print ind "        return 404;";
      print ind "    }";
      print ind "    client_max_body_size 64k;";
      print ind "    proxy_pass http://127.0.0.1:10000;";
      print ind "    proxy_set_header Host $host;";
      print ind "    proxy_set_header X-Real-IP $remote_addr;";
      print ind "    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;";
      print ind "    proxy_set_header X-Forwarded-Proto $scheme;";
      print ind "    proxy_http_version 1.1;";
      print ind "    proxy_read_timeout 60s;";
      print ind "}";
      print "";
    }
    # Hold each `location / {` block until its closing brace, to see whether
    # it proxies to the panel.
    holding {
      buf = buf $0 "\n";
      if ($0 ~ /^[[:space:]]*[}]/) {
        if (buf ~ /proxy_pass http:\/\/127\.0\.0\.1:10000;/) {
          match(held, /^[[:space:]]*/);
          peer(substr(held, 1, RLENGTH));
        }
        print held;
        printf "%s", buf;
        holding = 0;
      }
      next;
    }
    /^[[:space:]]*location \/ [{]/ { held = $0; buf = ""; holding = 1; next; }
    { print }
    END { if (holding) { print held; printf "%s", buf; } }
  ' "$backup" > "$vhost" || {
    # Never the reason an update stops: the panel works without it.
    echo "could not add the My Networks location; $vhost left as it was" >&2
    cp -a "$backup" "$vhost"
    return 0
  }

  if nginx -t >/dev/null 2>&1; then
    systemctl reload nginx || true
    echo "My Networks location added; nginx reloaded"
  else
    echo "nginx -t failed after patching $vhost - reverting" >&2
    cp -a "$backup" "$vhost"
    nginx -t >/dev/null 2>&1 || echo "WARNING: nginx config still invalid" >&2
  fi
}
patch_panel_vhost_peer_api

# Backfill the Dovecot fixes install.sh now applies on a fresh box.
#
# 1. Drop the PAM passdb (see the comment in install.sh): with it every mailbox
#    login logs a pam_unix "authentication failure" that fail2ban counts, so
#    busy legitimate clients get banned from IMAP and submission.
# 2. auth_verbose, so wrong passwords are still logged in the form fail2ban's
#    dovecot filter bans on.
# 3. Move the per-tenant SNI file from 97- to 99-vps-panel-sni.conf so it loads
#    after the 98- default cert (the agent writes the new name from now on).
#
# Idempotent. Reverted if doveconf rejects the result, and it never fails the
# update: mail config must not be the reason the panel did not restart.
patch_dovecot_auth() {
  local conf=/etc/dovecot/conf.d
  [[ -d "$conf" ]] && command -v doveconf >/dev/null 2>&1 || return 0
  local stamp; stamp="$(date +%Y%m%d%H%M%S)"
  local auth_backup="" made_verbose=0 moved_sni=0

  if grep -q '^!include auth-system.conf.ext' "$conf/10-auth.conf" 2>/dev/null; then
    auth_backup="$conf/10-auth.conf.bak-$stamp"
    cp -a "$conf/10-auth.conf" "$auth_backup"
    sed -i 's/^!include auth-system.conf.ext/#!include auth-system.conf.ext/' "$conf/10-auth.conf"
  fi
  if [[ ! -f "$conf/96-vps-panel-auth.conf" ]]; then
    echo 'auth_verbose = yes' > "$conf/96-vps-panel-auth.conf"
    chmod 0644 "$conf/96-vps-panel-auth.conf"
    made_verbose=1
  fi
  if [[ -f "$conf/97-vps-panel-sni.conf" && ! -f "$conf/99-vps-panel-sni.conf" ]]; then
    mv "$conf/97-vps-panel-sni.conf" "$conf/99-vps-panel-sni.conf"
    moved_sni=1
  elif [[ -f "$conf/97-vps-panel-sni.conf" ]]; then
    rm -f "$conf/97-vps-panel-sni.conf"
  fi
  [[ -n "$auth_backup" || $made_verbose -eq 1 || $moved_sni -eq 1 ]] || return 0

  if doveconf -n >/dev/null 2>&1; then
    systemctl reload dovecot || true
    echo "dovecot auth backfill applied; dovecot reloaded"
  else
    echo "doveconf failed after the dovecot auth backfill - reverting" >&2
    [[ -n "$auth_backup" ]] && cp -a "$auth_backup" "$conf/10-auth.conf"
    [[ $made_verbose -eq 1 ]] && rm -f "$conf/96-vps-panel-auth.conf"
    [[ $moved_sni -eq 1 ]] && mv "$conf/99-vps-panel-sni.conf" "$conf/97-vps-panel-sni.conf"
  fi
  return 0
}
patch_dovecot_auth || true

# Mail hardening for boxes installed before it (install.sh does the same):
# 1. No SMTP AUTH on port 25 (clients use 587/465; guessing on 25 was never
#    banned).
# 2. A logged-in mailbox may only send as itself or an alias that delivers
#    to it (smtpd_sender_login_maps + reject_authenticated_sender_login_mismatch);
#    any mailbox could send as any other tenant's address before.
# 3. Rspamd DKIM-signs only when the login's domain is the From domain.
# 4. The postfix-sasl jail reads postfix@-.service, where Ubuntu logs Postfix.
# Checked with postfix check and a lookup of a real mailbox; restored on
# failure. Idempotent, and never fails the update.
harden_mail() {
  [[ -f /etc/postfix/main.cf ]] && command -v postconf >/dev/null 2>&1 || return 0
  local stamp backup_dir=/var/backups/vps-panel-config
  stamp="$(date +%Y%m%d%H%M%S)"
  install -d -m 0700 "$backup_dir"
  local map=/etc/postfix/mysql-sender-login-maps.cf
  local src=/etc/postfix/mysql-virtual-mailbox-maps.cf

  if [[ "$(postconf -h smtpd_sasl_auth_enable)" != "no" || ! -f "$map" ]] \
     || ! postconf -h smtpd_sender_restrictions | grep -q reject_authenticated_sender_login_mismatch; then
    [[ -f "$src" ]] || return 0
    cp -a /etc/postfix/main.cf "$backup_dir/main.cf.bak-$stamp"
    cp -a /etc/postfix/master.cf "$backup_dir/master.cf.bak-$stamp"
    if [[ ! -f "$map" ]]; then
      { grep -E '^(user|password|hosts|dbname)[[:space:]]*=' "$src"
        echo "query = SELECT email FROM virtual_users WHERE email='%s' UNION SELECT destination FROM virtual_aliases WHERE source='%s'"
      } > "$map"
      chmod 0640 "$map"
      chown root:postfix "$map"
    fi
    local current
    current="$(postconf -h smtpd_sender_restrictions)"
    # AUTH goes off on 25 only when 587 (and 465 if present) keep it on
    # through their own master.cf override; otherwise clients could not send.
    if [[ -n "$(postconf -M submission/inet 2>/dev/null)" ]]; then
      postconf -P "submission/inet/smtpd_sasl_auth_enable=yes"
      if [[ -n "$(postconf -M smtps/inet 2>/dev/null)" ]]; then
        postconf -P "smtps/inet/smtpd_sasl_auth_enable=yes"
      fi
      postconf -e "smtpd_sasl_auth_enable = no"
    fi
    postconf -e "smtpd_sender_login_maps = mysql:$map"
    if ! grep -q reject_authenticated_sender_login_mismatch <<<"$current"; then
      postconf -e "smtpd_sender_restrictions = ${current:+$current, }reject_authenticated_sender_login_mismatch"
    fi
    local probe ok=1
    probe="$(mariadb -N -e "SELECT email FROM mail.virtual_users LIMIT 1" 2>/dev/null || true)"
    postfix check >/dev/null 2>&1 || ok=0
    if [[ -n "$probe" && "$(postmap -q "$probe" "mysql:$map" 2>/dev/null)" != *"$probe"* ]]; then ok=0; fi
    if [[ $ok -eq 1 ]]; then
      systemctl reload postfix || true
      echo "postfix: no AUTH on port 25; senders must match their login"
    else
      echo "postfix check or the sender map lookup failed - restoring main.cf" >&2
      cp -a "$backup_dir/main.cf.bak-$stamp" /etc/postfix/main.cf
      cp -a "$backup_dir/master.cf.bak-$stamp" /etc/postfix/master.cf
      systemctl reload postfix || true
    fi
  fi

  local dkim=/etc/rspamd/local.d/dkim_signing.conf
  if [[ -f "$dkim" ]] && grep -qE '^allow_username_mismatch[[:space:]]*=[[:space:]]*true' "$dkim"; then
    cp -a "$dkim" "$backup_dir/dkim_signing.conf.bak-$stamp"
    sed -i -E 's/^allow_username_mismatch[[:space:]]*=[[:space:]]*true;/allow_username_mismatch = false;/' "$dkim"
    if rspamadm configtest >/dev/null 2>&1; then
      systemctl reload rspamd || systemctl restart rspamd || true
      echo "rspamd: DKIM only for the login's own domain"
    else
      cp -a "$backup_dir/dkim_signing.conf.bak-$stamp" "$dkim"
    fi
  fi

  local jail=/etc/fail2ban/jail.d/zz-vps-panel.conf
  if [[ -f "$jail" ]] && grep -q '^\[postfix-sasl\]' "$jail" \
     && ! grep -q 'postfix@-.service' "$jail"; then
    cp -a "$jail" "$backup_dir/zz-vps-panel.conf.bak-$stamp"
    awk '{ print } /^\[postfix-sasl\]/ { print "journalmatch = _SYSTEMD_UNIT=postfix@-.service + _SYSTEMD_UNIT=postfix.service" }' \
      "$jail" > "$jail.tmp" && mv "$jail.tmp" "$jail"
    if fail2ban-client -t >/dev/null 2>&1; then
      systemctl reload fail2ban || true
      echo "fail2ban: postfix-sasl now reads postfix@-.service"
    else
      cp -a "$backup_dir/zz-vps-panel.conf.bak-$stamp" "$jail"
    fi
  fi
  return 0
}
harden_mail || true

# Forwarders (Mailboxes > Forwarders) on boxes installed before them. Postfix
# must find a mailbox's own address first, or a catch-all (@domain) row would
# take its mail and a forwarder on a mailbox would stop it keeping a copy. The
# same query install.sh writes. Checked with postfix check and lookups against
# the map; restored on failure. Idempotent, and never fails the update.
enable_forwarders() {
  local map=/etc/postfix/mysql-virtual-alias-maps.cf
  local query="query = SELECT destination FROM virtual_aliases WHERE source='%s' UNION SELECT email FROM virtual_users WHERE email='%s'"
  [[ -f "$map" ]] && command -v postmap >/dev/null 2>&1 || return 0
  grep -qxF "$query" "$map" && return 0
  local backup_dir=/var/backups/vps-panel-config stamp
  stamp="$(date +%Y%m%d%H%M%S)"
  install -d -m 0700 "$backup_dir"
  cp -a "$map" "$backup_dir/mysql-virtual-alias-maps.cf.bak-$stamp"
  sed -i "s|^query[[:space:]]*=.*|$query|" "$map"

  local ok=1 rc probe
  # Exit 0 = found, 1 = not found; anything else is a broken query.
  postmap -q "nobody@vps-panel.invalid" "mysql:$map" >/dev/null 2>&1; rc=$?
  [[ $rc -le 1 ]] || ok=0
  probe="$(mariadb -N -e "SELECT email FROM mail.virtual_users LIMIT 1" 2>/dev/null || true)"
  if [[ -n "$probe" && "$(postmap -q "$probe" "mysql:$map" 2>/dev/null)" != *"$probe"* ]]; then ok=0; fi
  postfix check >/dev/null 2>&1 || ok=0
  if [[ $ok -eq 1 ]]; then
    systemctl reload postfix || true
    echo "postfix: forwarders and catch-alls leave mailboxes their own mail"
  else
    echo "the forwarder lookup failed its checks - restoring it" >&2
    cp -a "$backup_dir/mysql-virtual-alias-maps.cf.bak-$stamp" "$map"
    systemctl reload postfix || true
  fi
  return 0
}
enable_forwarders || true

# Filters and Out of Office in webmail on boxes installed before them: Dovecot
# runs each mailbox's Sieve script on delivery (LMTP), and Roundcube edits it
# over ManageSieve (port 4190, opened in ufw to Docker's networks only). The
# agent adds Roundcube's side when it starts.
# A mailbox's Dovecot home moves from its maildir root to a plain `home` dir
# inside it: in the root, Sieve's files and Out of Office's reply log
# (.dovecot.sieve, .dovecot.lda-dupes.locks) show up as folders and break
# filing into folders. Mail itself does not move (mail_location is absolute).
# The new user_query is run against MariaDB and the config through doveconf
# (LMTP must still load quota) before Dovecot restarts; put back on failure.
# Idempotent, never fails the update.
enable_sieve() {
  local conf=/etc/dovecot/conf.d/91-vps-panel-sieve.conf
  local sql=/etc/dovecot/dovecot-sql.conf.ext
  [[ -d /etc/dovecot/conf.d && -f "$sql" ]] && command -v doveconf >/dev/null 2>&1 || return 0
  if ! dpkg -s dovecot-sieve dovecot-managesieved >/dev/null 2>&1; then
    if ! DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
         dovecot-sieve dovecot-managesieved >/dev/null 2>&1; then
      echo "could not install dovecot-sieve; webmail filters stay off until the next update" >&2
      return 0
    fi
  fi

  local want
  want="$(cat <<'EOF'
# Managed by vps-panel: Sieve filters and Out of Office for every mailbox,
# edited in webmail (Roundcube's managesieve plugin, port 4190).
protocol lmtp {
  mail_plugins = $mail_plugins sieve
}
plugin {
  sieve = file:~/sieve;active=~/.dovecot.sieve
}
EOF
)"
  local old_home="'/var/vmail/%d/%n' AS home" new_home="'/var/vmail/%d/%n/home' AS home"
  if [[ "$(cat "$conf" 2>/dev/null)" != "$want" ]] || grep -qF "$old_home" "$sql"; then
    local backup_dir=/var/backups/vps-panel-config stamp conf_backup=""
    stamp="$(date +%Y%m%d%H%M%S)"
    install -d -m 0700 "$backup_dir"
    cp -a "$sql" "$backup_dir/dovecot-sql.conf.ext.bak-$stamp"
    if [[ -f "$conf" ]]; then
      conf_backup="$backup_dir/91-vps-panel-sieve.conf.bak-$stamp"
      cp -a "$conf" "$conf_backup"
    fi
    printf '%s\n' "$want" > "$conf"
    chmod 0644 "$conf"
    sed -i "s|$old_home|$new_home|" "$sql"

    local ok=1 plugins probe query home
    plugins=" $(doveconf -f protocol=lmtp -h mail_plugins 2>/dev/null || true) "
    doveconf -n >/dev/null 2>&1 || ok=0
    [[ "$plugins" == *" sieve "* && "$plugins" == *" quota "* ]] || ok=0
    probe="$(mariadb -N -e "SELECT email FROM mail.virtual_users LIMIT 1" 2>/dev/null || true)"
    if [[ -n "$probe" ]]; then
      query="$(sed -n 's/^user_query[[:space:]]*=[[:space:]]*//p' "$sql")"
      query="${query//%u/$probe}"; query="${query//%d/${probe#*@}}"; query="${query//%n/${probe%@*}}"
      home="$(mariadb -N mail -e "$query" 2>/dev/null | awk '{print $3}')"
      [[ "$home" == "/var/vmail/${probe#*@}/${probe%@*}/home" ]] || ok=0
    fi
    if [[ $ok -eq 1 ]]; then
      # A restart, not a reload: a reload does not open the new 4190 listener.
      systemctl restart dovecot || true
      echo "dovecot: filters and out of office on"
    else
      echo "the sieve config or the new mailbox home failed its checks - putting both back" >&2
      cp -a "$backup_dir/dovecot-sql.conf.ext.bak-$stamp" "$sql"
      if [[ -n "$conf_backup" ]]; then cp -a "$conf_backup" "$conf"; else rm -f "$conf"; fi
    fi
  fi

  if command -v ufw >/dev/null 2>&1 && ufw status 2>/dev/null | grep -q "Status: active" \
     && ! ufw status 2>/dev/null | grep -q '^4190/tcp'; then
    ufw allow from 172.16.0.0/12 to any port 4190 proto tcp comment 'managesieve (webmail)' >/dev/null || true
  fi
  return 0
}
enable_sieve || true

# Retire the nginx fail2ban jails: nothing in the panel's nginx uses basic
# auth or limit_req, and nginx logs to files the systemd backend never reads,
# so they could never ban anyone while showing up as protection. Dropping
# their sections leaves them at fail2ban's default (disabled). Idempotent.
retire_nginx_jails() {
  local f changed=0
  for f in /etc/fail2ban/jail.d/zz-vps-panel.conf /etc/fail2ban/jail.d/zzz-vps-panel-policy.conf; do
    [[ -f "$f" ]] && grep -qE '^\[nginx-(http-auth|limit-req)\]' "$f" || continue
    awk '/^\[/ { skip = ($0 == "[nginx-http-auth]" || $0 == "[nginx-limit-req]") } !skip' \
      "$f" > "$f.tmp" && mv "$f.tmp" "$f"
    changed=1
  done
  if [[ $changed -eq 1 ]]; then
    systemctl reload fail2ban || true
    echo "retired the nginx fail2ban jails"
  fi
  return 0
}
retire_nginx_jails || true

systemctl restart vps-panel-agent.service
sleep 1
systemctl restart vps-panel.service
echo "updated."
