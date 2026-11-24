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

# One update at a time. A second run (by hand while the Update button's run
# is still building, say) deleted the first one's saved copy of the panel and
# moved its half-built tree aside as "previous"; both builds then failed and
# removed what was left.
exec 9>/run/vps-panel-update.lock
if ! flock -n 9; then
  echo "another update.sh is already running" >&2
  exit 1
fi

# 9>&-: git may leave `gc --auto` running in the background, and it must not
# keep the lock.
BEFORE_PULL="$(git rev-parse HEAD)"
git pull --ff-only 9>&-
# The pull may have replaced this very file. bash goes on running the old
# one, so the steps a new version adds (migrations, the build and swap
# below) did not run until the update after. Start again from the new file.
if [[ "$(git rev-parse HEAD)" != "$BEFORE_PULL" && -z "${VPS_PANEL_UPDATE_REEXEC:-}" ]]; then
  export VPS_PANEL_UPDATE_REEXEC=1
  exec bash "$REPO_DIR/scripts/update.sh"
fi

# For every apt-get install below: a package upgraded along the way must not
# stop to ask about a configuration file that was changed on the server (the
# installer edits Dovecot's 10-auth.conf, for one). There is nobody to
# answer, and the install failed. These keep the file that is installed.
APT_KEEP_CONF=(-o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold)

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

build_agent() {
  cargo build --release --locked --manifest-path agent/Cargo.toml
}
# Nothing else ever updates Rust on the server. Once a dependency needed a
# newer rustc than install.sh had installed, this build failed on every
# update from then on. Only after a failed build, and only once: update the
# toolchain and build again.
if ! build_agent; then
  if ! command -v rustup >/dev/null 2>&1; then
    echo "agent build failed; nothing was changed" >&2
    exit 1
  fi
  echo "agent build failed - updating Rust (rustup update stable) and building once more" >&2
  rustup update --no-self-update stable || echo "rustup update stable failed" >&2
  if ! build_agent; then
    echo "agent build failed again; nothing was changed" >&2
    exit 1
  fi
fi

# The new panel is built at its real path, because Next.js output can embed the
# directory it was built in. The running install is moved aside to $PREV_DIR
# first and put back if the build fails. The old process keeps running
# throughout (its restart is the last step below); pages it has not loaded yet
# may error until then, as they did with the in-place build.
PREV_DIR="$INSTALL_DIR/app.prev"
# Exists while $APP_DIR is a build in progress and the working install is in
# $PREV_DIR.
BUILDING="$INSTALL_DIR/.update-building"
# What goes with $PREV_DIR: the agent binary and the unit files that were
# installed when the panel was moved there, so a new version that does not
# start can be replaced by the whole previous one. In root-only places, not
# in $INSTALL_DIR, which the panel's user can write to.
AGENT_BIN=/usr/local/bin/vps-panel-agent
AGENT_PREV="$AGENT_BIN.prev"
UNIT_DIR=/etc/systemd/system
UNITS_PREV=/var/backups/vps-panel-config/units.prev
UNITS=(vps-panel-agent.service vps-panel.service)

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
  # NEXT_TELEMETRY_DISABLED: no build report to telemetry.nextjs.org.
  (cd "$APP_DIR" && sudo -u vps-panel HOME=/var/lib/vps-panel NEXT_TELEMETRY_DISABLED=1 npm run build) &&
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

# The working install back in place of a build that did not finish.
restore_panel() {
  if [[ -d "$PREV_DIR" ]]; then
    rm -rf "$APP_DIR"
    mv "$PREV_DIR" "$APP_DIR"
  fi
  rm -f "$BUILDING"
}

# The agent binary and unit files as they are now, kept for restore_agent.
save_agent() {
  local u
  rm -f "$AGENT_PREV"
  rm -rf "$UNITS_PREV"
  install -d -m 0700 "$(dirname "$UNITS_PREV")" "$UNITS_PREV"
  if [[ -f "$AGENT_BIN" ]]; then
    cp -a "$AGENT_BIN" "$AGENT_PREV"
  fi
  for u in "${UNITS[@]}"; do
    if [[ -f "$UNIT_DIR/$u" ]]; then
      cp -a "$UNIT_DIR/$u" "$UNITS_PREV/$u"
    fi
  done
}

# The saved agent binary and unit files back in place. `install` puts a new
# file there; writing into the binary of a running agent is refused (Text
# file busy). Every step is tried even when one fails.
restore_agent() {
  local u
  if [[ -f "$AGENT_PREV" ]]; then
    install -m 0755 -o root -g root "$AGENT_PREV" "$AGENT_BIN" \
      || echo "could not put $AGENT_PREV back as $AGENT_BIN" >&2
  fi
  for u in "${UNITS[@]}"; do
    if [[ -f "$UNITS_PREV/$u" ]]; then
      install -m 0644 "$UNITS_PREV/$u" "$UNIT_DIR/$u" \
        || echo "could not put $UNITS_PREV/$u back" >&2
    fi
  done
  systemctl daemon-reload || true
}

# Is the running panel the install that was moved to $PREV_DIR? A process
# keeps its working directory through a rename, and /proc shows where that
# directory is now.
panel_runs_from_prev() {
  local pid cwd
  pid="$(systemctl show -p MainPID --value vps-panel.service 2>/dev/null || true)"
  [[ "$pid" =~ ^[1-9][0-9]*$ ]] || return 1
  cwd="$(readlink "/proc/$pid/cwd" 2>/dev/null || true)"
  [[ "$cwd" == "$PREV_DIR" ]]
}

# An update that died during the panel build (killed, out of memory, a
# reboot) left the half-built tree in $APP_DIR and the working install in
# $PREV_DIR. Put it back first: the lines below would delete $PREV_DIR and
# keep the half-built tree as "previous", so a build that then failed
# "restored" a panel that cannot start.
if [[ -e "$BUILDING" && -d "$PREV_DIR" ]]; then
  echo "the last update stopped during the panel build - putting the previous panel install back first"
  restore_panel
elif [[ -d "$PREV_DIR" ]] && panel_runs_from_prev; then
  # An update that was killed after the build and before it restarted the
  # services (one that only failed there cleans up itself, further down). The
  # old panel process is still running, from $PREV_DIR. The `rm -rf` below
  # used to delete the files under it, and the build that was never started
  # became "previous". Put the install it runs from back, with its agent
  # binary and units, so what is on disk is what is running.
  echo "the last update stopped before it restarted the panel - putting the previous version back first"
  restore_agent
  restore_panel
fi
rm -f "$BUILDING"

rm -rf "$PREV_DIR"
# Before the panel moves to $PREV_DIR, so that whenever $PREV_DIR exists the
# saved binary and units are the ones that belong to it.
save_agent
# Ctrl-C, a dropped SSH session or systemd stopping the update's unit: put
# the working install back before going.
trap 'echo "interrupted - restoring the previous panel install" >&2; restore_panel; exit 130' INT TERM HUP
if [[ -d "$APP_DIR" ]]; then
  touch "$BUILDING"
  mv "$APP_DIR" "$PREV_DIR"
fi
if ! build_panel; then
  echo "panel build failed - restoring the previous panel install; nothing else was changed" >&2
  rm -rf "$APP_DIR"
  restore_panel
  exit 1
fi
rm -f "$BUILDING"
trap - INT TERM HUP
echo "new panel built in $APP_DIR (previous install kept in $PREV_DIR)"

# ---------------------------------------------------------------------------
# Both builds succeeded: install them.
#
# From here until both services are confirmed running, the new version is
# either installed as a whole or not at all. Whatever ends the script on the
# way (a failed step, Ctrl-C, a new agent or panel that does not stay up)
# puts the previous agent binary, unit files and panel install back together.
# An agent that crashed at start used to leave the server without agent and
# panel, the previous binary gone; and an update that stopped before the
# restarts left the old panel process running from $PREV_DIR, the files the
# next update deletes first.
#
# Not put back:
# - panel.db and the mail database. The new panel (src/lib/db.ts) and the new
#   agent (ensure_agent_schema, migrate_mail_schema) may have migrated them
#   before failing. Their migrations only create tables and add columns with
#   defaults, which the previous version does not notice. A migration that
#   the previous version could not live with must not ship without a way
#   back here.
# - What the steps below change in nginx, Postfix, Dovecot, PowerDNS and
#   fail2ban. Each checks its own result, puts its own files back when the
#   check fails, and does not depend on which version of the panel runs.
# ---------------------------------------------------------------------------

# What `finish` has to undo. install: files are being replaced, the old
# processes still run. restart: the services are being restarted. Anything
# else: nothing.
STAGE=install

# The update is running inside the agent's own service (started from the
# panel's terminal, or by the Update button of a very old agent): restarting
# the agent ends this script too, so it can neither check the restart nor
# undo it.
INSIDE_AGENT=0
if grep -qs '/vps-panel-agent\.service' /proc/self/cgroup; then
  INSIDE_AGENT=1
fi

# The previous version back after a restart that did not work, and both
# services restarted on it.
roll_back() {
  echo "putting the previous agent, unit files and panel install back" >&2
  # Stopped first: the new panel must not run on while its files go.
  systemctl stop vps-panel.service || true
  restore_agent
  restore_panel
  systemctl restart vps-panel-agent.service || true
  sleep 1
  systemctl restart vps-panel.service || true
  sleep 3
  local unit down=0
  for unit in "${UNITS[@]}"; do
    if ! systemctl is-active --quiet "$unit"; then
      echo "$unit is not running on the previous version either. Check: journalctl -u $unit -n 50" >&2
      down=1
    fi
  done
  if [[ $down -eq 0 ]]; then
    echo "the update was rolled back: the previous version is running again. The checkout stays at the new commit; a later update tries it again." >&2
  else
    echo "the update was rolled back, but not everything came up (see above)." >&2
  fi
}

finish() {
  local rc=$?
  # Nothing may cut the clean-up short: not a failing command, not a second
  # Ctrl-C, not a terminal that is gone.
  set +e
  trap - EXIT
  trap '' INT TERM HUP PIPE
  case "$STAGE" in
    install)
      echo "the update stopped before the restart - putting the previous agent, unit files and panel install back. The running services were not touched." >&2
      restore_agent
      restore_panel
      ;;
    restart)
      roll_back
      ;;
    *)
      exit "$rc"
      ;;
  esac
  [[ $rc -ne 0 ]] || rc=1
  exit "$rc"
}
trap finish EXIT
trap 'exit 130' INT TERM HUP

install -m 0755 -o root -g root \
  "$REPO_DIR/target/release/vps-panel-agent" \
  "$AGENT_BIN"

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

# /root/.ssh and /root/.npm (npm's cache, for builds that shell out to a
# child `npm install`) exist before the agent restarts, as install.sh leaves
# them. The unit used to whitelist both with ReadWritePaths=, which only
# pins a path that is there at start; it no longer sandboxes /root at all.
# Idempotent.
install -d -m 0700 -o root -g root /root/.ssh
install -d -m 0755 -o root -g root /root/.npm

install -m 0644 "$REPO_DIR/scripts/systemd/vps-panel-agent.service" \
  "$UNIT_DIR/vps-panel-agent.service"
install -m 0644 "$REPO_DIR/scripts/systemd/vps-panel.service" \
  "$UNIT_DIR/vps-panel.service"
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
    # `|| true`: a stopped nginx must not end the update between installing
    # the new agent and panel and restarting them.
    systemctl reload nginx || true
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

# HSTS for the name itself only, in vhosts written before the agent's
# templates dropped includeSubDomains. With it, a browser that had visited
# https://example.com refused plain HTTP on every subdomain for two years,
# also the ones served elsewhere or without a certificate. Only vhosts the
# panel wrote are touched (the first line is one of the markers the agent
# knows its own files by: the panel vhost, a site, an app, a vhost it
# patched), and only the header exactly as the templates wrote it: a vhost
# made by hand, or a header someone changed, stays as it is.
# Checked with nginx -t and put back on failure. Idempotent, and never fails
# the update.
hsts_drop_subdomains() {
  local dir=/etc/nginx/sites-available backup_dir=/var/backups/vps-panel-config
  local old='add_header Strict-Transport-Security "max-age=63072000; includeSubDomains" always;'
  local new='add_header Strict-Transport-Security "max-age=63072000" always;'
  [[ -d "$dir" ]] && command -v nginx >/dev/null 2>&1 || return 0
  local f first stamp files=()
  for f in "$dir"/*; do
    [[ -f "$f" && ! -L "$f" ]] || continue
    case "$f" in
      *.bak*|*.orig*|*.old*|*~) continue ;;
    esac
    first=""
    IFS= read -r first < "$f" || true
    case "$first" in
      '# vps-panel-host:'*|'# vps-site:'*|'# vps-app:'*|'# patched-by-vps-panel'*) ;;
      *) continue ;;
    esac
    grep -qF "$old" "$f" || continue
    files+=("$f")
  done
  [[ ${#files[@]} -gt 0 ]] || return 0

  stamp="$(date +%Y%m%d%H%M%S)"
  install -d -m 0700 "$backup_dir"
  for f in "${files[@]}"; do
    cp -a "$f" "$backup_dir/$(basename "$f").hsts-$stamp"
    sed -i "s|$old|$new|" "$f"
  done
  if nginx -t >/dev/null 2>&1; then
    systemctl reload nginx || true
    echo "HSTS no longer covers subdomains in ${#files[@]} vhost(s)"
  else
    echo "nginx -t failed after the HSTS change - reverting" >&2
    for f in "${files[@]}"; do cp -a "$backup_dir/$(basename "$f").hsts-$stamp" "$f"; done
    nginx -t >/dev/null 2>&1 || echo "WARNING: nginx config still invalid" >&2
  fi
  return 0
}
hsts_drop_subdomains || true

# Back to root what a re-run of an older install.sh gave to the panel's user:
# its recursive chown of /var/lib/vps-panel handed over the agent's root-only
# folders (database dumps, the Files trash, build snapshots). install.sh
# repairs this on a re-run; a server that only ever updates got it nowhere.
# The folders themselves only, as in install.sh. The backup directory, where
# the agent keeps its database dumps from now on, is there for it, root-only.
# The old dump folder stays: the agent empties it itself after a backup that
# worked. Idempotent, and never fails the update.
repair_root_dirs() {
  local d mode
  for d in /var/lib/vps-panel/db-dumps /var/lib/vps-panel/trash /var/lib/vps-panel/build-snapshots; do
    [[ -d "$d" && ! -L "$d" ]] || continue
    # The last three digits: the folder also carries its parent's setgid bit.
    mode="$(stat -c '%a' "$d" 2>/dev/null || true)"
    if [[ "$(stat -c '%U' "$d" 2>/dev/null || true)" != "root" || "${mode: -3}" != "700" ]]; then
      chown root "$d" && chmod 0700 "$d" && echo "$d is root-only again"
    fi
  done
  install -d -m 0750 -o root -g root /var/backups/vps-panel
  return 0
}
repair_root_dirs || true

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
      # It holds the mail database password: unreadable to others from the
      # start, not only after the chmod.
      install -m 0640 -o root -g postfix /dev/null "$map"
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
    if ! DEBIAN_FRONTEND=noninteractive apt-get install -y "${APT_KEEP_CONF[@]}" --no-install-recommends \
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

# Standard folders and spam filing on boxes installed before them (install.sh
# writes the same two files): Dovecot creates and subscribes Drafts, Sent,
# Trash and Junk for every mailbox on its next login or delivery, and a global
# Sieve script run before each mailbox's own filters files Rspamd's
# `X-Spam: Yes` mail into Junk. Mail already stored does not move. Checked with
# sievec and doveconf before Dovecot reloads; put back on failure. The binary
# is recompiled on every update, in case a dovecot-sieve upgrade outdated it.
# Idempotent, and never fails the update.
enable_junk_folder() {
  local conf=/etc/dovecot/conf.d/92-vps-panel-junk.conf
  local dir=/etc/dovecot/sieve
  local script=$dir/vps-panel-spam.sieve bin=$dir/vps-panel-spam.svbin
  [[ -d /etc/dovecot/conf.d ]] && command -v doveconf >/dev/null 2>&1 \
    && command -v sievec >/dev/null 2>&1 || return 0

  local want_script want_conf
  want_script="$(cat <<'EOF'
# Managed by vps-panel: mail Rspamd tagged as spam goes to Junk. Runs before
# each mailbox's own Sieve script; stop keeps that script (filters, Out of
# Office) from acting on spam.
require ["fileinto", "mailbox"];
if header :is "X-Spam" "Yes" {
  fileinto :create "Junk";
  stop;
}
EOF
)"
  want_conf="$(cat <<'EOF'
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
)"
  if [[ "$(cat "$script" 2>/dev/null)" == "$want_script" \
        && "$(cat "$conf" 2>/dev/null)" == "$want_conf" ]]; then
    sievec "$script" >/dev/null 2>&1 && chmod 0644 "$bin" || true
    return 0
  fi

  local backup_dir=/var/backups/vps-panel-config stamp f
  stamp="$(date +%Y%m%d%H%M%S)"
  install -d -m 0700 "$backup_dir"
  for f in "$script" "$conf"; do
    [[ -f "$f" ]] && cp -a "$f" "$backup_dir/$(basename "$f").bak-$stamp"
  done
  install -d -m 0755 "$dir"
  printf '%s\n' "$want_script" > "$script"
  printf '%s\n' "$want_conf" > "$conf"

  local ok=1
  sievec "$script" >/dev/null 2>&1 || ok=0
  chmod 0644 "$script" "$conf" "$bin" 2>/dev/null || true
  doveconf -n >/dev/null 2>&1 || ok=0
  if [[ $ok -eq 1 ]]; then
    systemctl reload dovecot || true
    echo "dovecot: Junk folder for every mailbox; spam goes there"
  else
    echo "the Junk folder config failed its checks - putting it back" >&2
    for f in "$script" "$conf"; do
      if [[ -f "$backup_dir/$(basename "$f").bak-$stamp" ]]; then
        cp -a "$backup_dir/$(basename "$f").bak-$stamp" "$f"
      else
        rm -f "$f"
      fi
    done
    rm -f "$bin"
    [[ -f "$script" ]] && sievec "$script" >/dev/null 2>&1 || true
  fi
  return 0
}
enable_junk_folder || true

# dig, on boxes that came without it (minimal images; install.sh installs it
# now). The agent looks records up with it before it orders a certificate, and
# for the mail checks: without dig every such check failed as "no public A or
# AAAA record". Never fails the update.
ensure_dig() {
  command -v dig >/dev/null 2>&1 && return 0
  if DEBIAN_FRONTEND=noninteractive apt-get install -y "${APT_KEEP_CONF[@]}" --no-install-recommends \
       bind9-dnsutils >/dev/null 2>&1; then
    echo "installed bind9-dnsutils (dig)"
  else
    echo "could not install bind9-dnsutils (dig); certificate and mail DNS checks need it" >&2
  fi
  return 0
}
ensure_dig || true

# PowerDNS answers over IPv6 too, on boxes installed before it did (install.sh
# writes the same line now). It was bound to the public IPv4 and 127.0.0.1
# only, while a zone gets AAAA records for its own ns1/ns2 once the server has
# a published IPv6 address: name servers announced over IPv6 that did not
# answer there. `::` is every IPv6 address the server has or gets, and cannot
# collide with systemd-resolved (IPv4 only). Only a local-address line of the
# shape install.sh wrote is changed, and only while pdns is running, so the
# result can be checked: pdns is restarted to open the new socket, and if it
# does not come up the old line is put back and pdns restarted again.
# Idempotent, and never fails the update.
pdns_listen_ipv6() {
  local conf=/etc/powerdns/pdns.conf
  [[ -f "$conf" && -e /proc/net/if_inet6 ]] || return 0
  local cur
  cur="$(sed -n 's/^local-address=//p' "$conf" | tail -1)"
  [[ "$cur" =~ ^[0-9.]+,127\.0\.0\.1$ ]] || return 0
  systemctl is-active --quiet pdns || return 0
  local backup_dir=/var/backups/vps-panel-config stamp
  stamp="$(date +%Y%m%d%H%M%S)"
  install -d -m 0700 "$backup_dir"
  cp -a "$conf" "$backup_dir/pdns.conf.bak-$stamp"
  sed -i "s|^local-address=.*|local-address=$cur,::|" "$conf"
  if systemctl restart pdns && systemctl is-active --quiet pdns; then
    echo "pdns: answers over IPv6 too now"
  else
    echo "pdns did not start with an IPv6 listener - putting pdns.conf back" >&2
    cp -a "$backup_dir/pdns.conf.bak-$stamp" "$conf"
    systemctl restart pdns || true
  fi
  return 0
}
pdns_listen_ipv6 || true

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

# How often systemd has restarted the unit by itself since its last start.
unit_restarts() {
  local n
  n="$(systemctl show -p NRestarts --value "$1" 2>/dev/null || true)"
  [[ "$n" =~ ^[0-9]+$ ]] || n=0
  echo "$n"
}

if [[ $INSIDE_AGENT -eq 1 ]]; then
  # The script is killed with the agent it restarts; systemd restarts the
  # panel along with the agent (Requires=). As before, nothing checks the
  # result, and there is no rollback: STAGE says so to `finish`.
  echo "running inside the agent's own service: this script ends when the agent restarts and cannot check the result. Use the Update button or run it over SSH."
  STAGE=handoff
else
  STAGE=restart
fi
systemctl restart vps-panel-agent.service
AGENT_RESTARTS="$(unit_restarts vps-panel-agent.service)"
sleep 1
systemctl restart vps-panel.service
PANEL_RESTARTS="$(unit_restarts vps-panel.service)"
# Both units are Type=simple: restart succeeds once the process is started,
# even if it exits right after, and with Restart=on-failure a unit that
# crashes at every start is "active" again for a moment every two seconds.
# So: both stay active, and systemd restarts neither by itself, for six
# seconds. An update that fails this is rolled back (`finish`) and does not
# end with "updated." and exit 0.
DOWN=""
for _ in 1 2 3 4 5 6; do
  sleep 1
  if ! systemctl is-active --quiet vps-panel-agent.service \
     || [[ "$(unit_restarts vps-panel-agent.service)" -gt "$AGENT_RESTARTS" ]]; then
    DOWN=vps-panel-agent.service
  elif ! systemctl is-active --quiet vps-panel.service \
     || [[ "$(unit_restarts vps-panel.service)" -gt "$PANEL_RESTARTS" ]]; then
    DOWN=vps-panel.service
  fi
  [[ -z "$DOWN" ]] || break
done
if [[ -n "$DOWN" ]]; then
  echo "$DOWN did not stay up after the update. Its last log lines:" >&2
  journalctl -u "$DOWN" -n 30 --no-pager >&2 || true
  exit 1
fi
STAGE=done
trap - EXIT INT TERM HUP
echo "updated."
