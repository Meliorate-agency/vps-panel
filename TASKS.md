# TASKS

Production-readiness sweep for vps-panel. This is the single tracked checklist for shipping the panel to a real internet-facing host. Scope: code quality the panel needs before users touch it, security hardening that closes verified gaps, and operational gaps (backup safety, retention, rollback, /health, logging) that turn a working dev box into a maintainable production service. No new features.

Severity tags map back to the audit findings: `[C*]` = critical, `[H*]` = high, `[M*]` = medium, `[L*]` = low.

## Quick Status

- Phase 0 (P0, drop everything): 4 tasks
- Phase 1 (security hardening before launch): 7 tasks
- Phase 2 (operational readiness): 9 tasks
- Phase 3 (code quality / dedup): 6 tasks
- Phase 4 (documentation + polish): 5 tasks
- Phase 5 (optional / post-launch): 4 tasks

Total: 35 tasks. **Overall production readiness: ~40%. 4 blocking items remaining (Phase 0).**

---

## Phase 0 - Bleeding (P0, drop everything)

These block any deploy. Do not ship while any Phase 0 item is open.

### [C1] Rotate all secrets and remove `.env.local` from git history

`.env.local` is tracked. Real `JWT_SECRET`, `AGENT_TOKEN`, `ADMIN_INITIAL_PASSWORD=12345`, `ADMIN_EMAIL`, `PANEL_HOST` are in the repo. The deploy workflow installs this file as `/etc/vps-panel/.env`.

Files:
- `.env.local` (delete from working tree)
- `.gitignore` (un-comment lines 64-65: `.env.local` and `.env.*`)
- `.github/workflows/deploy.yml` lines 77-82, 119-130 (replace `.env.local` reads with GitHub Actions secret-based env injection via heredoc)
- New: `.env.example` (template only, all values placeholders) - bundled with [M7]

Acceptance:
- `git ls-files | grep -E '^\.env'` returns empty
- After history rewrite (BFG / `git filter-repo`) + force push: `git log --all --full-history -- .env.local` returns empty
- New `JWT_SECRET`, `AGENT_TOKEN`, `RESTIC_PASSWORD` are different from old values, generated with `openssl rand -hex 32`
- `ADMIN_INITIAL_PASSWORD` replaced with strong random or removed entirely (force first-login reset)
- Deploy workflow writes `/etc/vps-panel/.env` from `${{ secrets.PANEL_ENV }}` directly without committing it

### [C2] Fix all `cargo clippy --all-targets -- -D warnings` errors

Six clippy errors prevent a clean build with `-D warnings`.

Files:
- `agent/src/handlers/diag.rs:121` (drop `.trim()` before `.split_whitespace()`)
- `agent/src/handlers/ssh.rs:72` (drop `as u32` cast - `mins` is already `u32`)
- `agent/src/exec.rs:10` (remove unused `stderr` field or wire it in)
- `agent/src/handlers/apps.rs:38` (remove unused `runtime` field or use it)
- `agent/src/nginx_template.rs` (remove unused `PanelVhostTemplate`)

Acceptance:
- `cd agent && cargo clippy --all-targets -- -D warnings` exits 0
- `cargo check --all-targets` exits 0

### [C3] Replace `'unsafe-inline'` script-src with nonce-based CSP

`next.config.ts:7` allows `'unsafe-inline'` scripts in production, which neuters CSP as XSS defense.

Files:
- `next.config.ts` (drop static script CSP from headers)
- `proxy.ts` (generate per-request nonce, attach to response header `Content-Security-Policy` and forward via request header for downstream consumption)
- `src/app/layout.tsx` (consume nonce via `headers()` helper, attach `nonce={nonce}` to any `<Script>` tags)

Acceptance:
- Response `Content-Security-Policy` header in production has `script-src 'self' 'nonce-<value>'` (no `'unsafe-inline'`)
- Panel loads, login works, dashboard renders with no CSP violations in browser console
- Manual test: paste `<script>alert(1)</script>` into a server-action input - browser blocks it

### [C7] Add CI typecheck + clippy gate before deploy

Deploy workflow has no test/lint stage. A broken commit deploys.

Files:
- `.github/workflows/deploy.yml` (add `verify` job; make `deploy` job `needs: verify`)
- `package.json` (confirm `typecheck` script exists)

Acceptance:
- New `verify` job runs `npm ci && npm run typecheck` and `cd agent && cargo clippy --all-targets -- -D warnings`
- Pushing a TS error or clippy warning to `main` blocks the deploy step

---

## Phase 1 - Security hardening before launch

### [C4] Reject newlines/CR in SSH `authorized_keys` add input

`agent/src/handlers/ssh.rs:224-249` does not reject `\n`/`\r` in `req.key` or `req.label`. An authenticated admin or compromised path could inject a second authorized key via newline injection.

Files:
- `agent/src/handlers/ssh.rs` (add explicit reject in `add()` before any write)
- `agent/src/validate.rs` (add `validate_ssh_key_line` helper)

Acceptance:
- `add()` returns 400 if `req.key.contains('\n') || req.key.contains('\r') || label.contains('\n') || label.contains('\r')`
- Unit test in `agent/src/handlers/ssh.rs` covers all three rejection cases

### [C5] Document cron command threat model (admin-only RCE-as-root accepted)

`agent/src/handlers/cron.rs:74-101` allows shell metacharacters in `command`. This is RCE-as-root if the admin account is compromised. Single-tenant model accepts this; document it.

Files:
- `docs/SECURITY.md` (new section: "Trust boundary: admin = root")
- `agent/src/handlers/cron.rs` (add comment above `create()` referencing the SECURITY.md section)

Acceptance:
- SECURITY.md states: admin compromise = host compromise; cron, apps, systemd handlers all execute admin-supplied strings as root by design
- No regex tightening attempted (would create a false sense of safety)

### [C6] Make `panel.db` backup WAL-safe

`agent/src/handlers/backups.rs:28-39` snapshots `/var/lib/vps-panel` while the DB is in WAL mode. A live restic copy can produce inconsistent triplet of `panel.db` + `-wal` + `-shm`.

Files:
- `agent/src/handlers/backups.rs` (in `run()`, before restic invocation: `sqlite3 /var/lib/vps-panel/panel.db ".backup /var/lib/vps-panel/panel.db.snapshot"`)
- `DEFAULT_PATHS` (replace `/var/lib/vps-panel` with explicit path list excluding `panel.db-wal` and `panel.db-shm`)

Acceptance:
- Backup includes `panel.db.snapshot` (consistent point-in-time copy)
- WAL/SHM files excluded from snapshot
- Restore path documented in `docs/OPERATIONS.md` (Phase 4)

### [H3] Persist ACME account key to `/etc/vps-panel/acme-account.key`

`agent/src/handlers/ssl.rs:119-151` creates a fresh ACME account per issuance. No revocation path. No rate-limit-friendly account reuse.

Files:
- `agent/src/handlers/ssl.rs` (load key from `/etc/vps-panel/acme-account.key` if present; otherwise `Account::create()` then write the resulting credentials JSON to disk via `fs_atomic::write`, mode 0600, owned root:root)

Acceptance:
- First cert issuance writes `/etc/vps-panel/acme-account.key` (0600, root:root)
- Second issuance reuses it (verified by Let's Encrypt account URL stable across issues)
- Note added to `docs/OPERATIONS.md`: backup includes this file (see [M10] in Phase 5 batch)

### [H4] Account lockout + TOTP backup codes + recovery doc

No lockout after N failed logins. No TOTP backup codes. Lost TOTP = SSH-only recovery, undocumented.

Files:
- `src/lib/db.ts` (migration: add `users.locked_until INTEGER`, add `totp_backup_codes` table with `code_hash`, `consumed_at`)
- `src/lib/auth.ts` (after 3 failed in 5 min from same user, set `locked_until = now + 30min`; deny login while locked)
- `src/app/(auth)/totp/page.tsx` (on enroll, generate 10 random 8-char codes, hash with argon2id, present once, store hashes)
- `src/app/(auth)/login/LoginForm.tsx` (add "Use backup code" link)
- `docs/OPERATIONS.md` (Phase 4: SSH recovery procedure)

Acceptance:
- 3 wrong passwords in 5 min returns "account locked, try again at HH:MM"
- TOTP enrollment shows backup codes once; copy/download button works
- Each backup code accepted exactly once; second use rejected
- OPERATIONS.md describes recovery via SSH + sqlite3 hand-edit

### [H8] Tighten systemd hardening for the agent

`scripts/systemd/vps-panel-agent.service` has `NoNewPrivileges=false` and missing several cheap flags. Agent runs as root and needs many syscalls, so full sandboxing isn't possible, but several flags are safe.

Files:
- `scripts/systemd/vps-panel-agent.service`

Acceptance:
- File adds: `NoNewPrivileges=true`, `ProtectKernelTunables=true`, `ProtectControlGroups=true`, `RestrictRealtime=true`, `LockPersonality=true`, `SystemCallArchitectures=native`
- After deploy, `systemctl status vps-panel-agent` shows active (running)
- Smoke test: nginx reload, app deploy, cert issue all still succeed

### [H7] Add nginx rate limiting on the panel vhost

`scripts/nginx/panel.conf.template` has no `limit_req_zone` / `limit_req`. Any attacker can brute-force `/login`.

Files:
- `scripts/nginx/panel.conf.template` (add `limit_req zone=panel_general burst=50 nodelay;` and `/login` location with `limit_req zone=panel_login burst=2 nodelay;`)
- New: `scripts/nginx/vps-panel-ratelimit.conf` (zone definitions: `limit_req_zone $binary_remote_addr zone=panel_general:10m rate=30r/s;` and `zone=panel_login:10m rate=5r/m;`)
- `scripts/install.sh` (install the ratelimit conf to `/etc/nginx/conf.d/vps-panel-ratelimit.conf`)

Acceptance:
- General endpoints: rate-limited with burst 50
- `/login`: rate-limited at 5/min with burst 2
- Manual test: 10 rapid POSTs to `/login` returns 429 starting at the 6th-8th

---

## Phase 2 - Operational readiness

### [H5] Add daily DB retention cleanup

`login_attempts`, `tasks`, `audit_log`, revoked `sessions` grow unbounded.

Files:
- New: `scripts/cleanup.ts` (or inline into `src/lib/db.ts` startup hook)
- New: `scripts/systemd/vps-panel-cleanup.service` + `vps-panel-cleanup.timer` (daily 04:00)
- `scripts/install.sh` (install + enable timer)
- `.github/workflows/deploy.yml` (install timer files alongside existing units)

Acceptance:
- Retention defaults: `login_attempts` 90d, `tasks` 30d, `audit_log` 365d, `sessions WHERE revoked=1` 7d
- Timer fires; row counts visible in Settings page (post-cleanup)

### [H6] Deploy rollback + version compatibility check

`git reset --hard origin/main` blows away local state with no rollback. Panel and agent versions can diverge silently.

Files:
- `.github/workflows/deploy.yml` (capture `git rev-parse HEAD` before reset; on any later failure, `git reset --hard $PREV_SHA` and rebuild)
- `agent/src/handlers/health.rs` (confirm `version` exposure via Cargo pkg version)
- `src/lib/agent.ts` (on panel startup, fetch agent `/health`, log warning if `agent.version !== panel.version`)
- `package.json` + `agent/Cargo.toml` (sync `version` field manually for v1.0)

Acceptance:
- Failed deploy resets to previous SHA and reports it in workflow logs
- Panel logs `agent version mismatch: panel=x.y.z agent=a.b.c` when versions differ
- `/health` on agent returns version string

### [H10] Make agent `/health` actually check dependencies

`agent/src/handlers/health.rs` returns 200 unconditionally. Useless as a probe.

Files:
- `agent/src/handlers/health.rs`

Acceptance:
- Returns 503 with body listing failed checks if any of: panel.db open fails, MariaDB ping fails, `/etc/nginx` not writable, `/run/vps-panel` not writable
- Returns 200 only when all pass
- Manual test: `chmod 0` on `/etc/nginx` (or equivalent) makes `/health` return 503

### [H9] Add "Revoke all sessions" admin action

After secret rotation no way to invalidate active sessions short of DB hand-edit.

Files:
- New: `src/server-actions/sessions.ts` (`revokeAllSessions()`)
- `src/app/(panel)/settings/page.tsx` (add button gated by `<ConfirmDialog>` from [M5])
- `src/lib/auth.ts` (audit log entry on revoke)

Acceptance:
- Click triggers ConfirmDialog; on confirm, all `sessions.revoked = 1` in panel.db
- Current admin gets logged out on next request and must re-auth

### [H11] Move `install.sh` admin password to a one-time-readable file

`scripts/install.sh:515` echoes admin password to stdout. Visible in scrollback, terminal recording, ssh-action logs.

Files:
- `scripts/install.sh`

Acceptance:
- Password is written to `/etc/vps-panel/initial-admin.txt` (mode 0600, root:root)
- stdout prints only: `Admin credentials written to /etc/vps-panel/initial-admin.txt (read once and delete)`

### [M8] Structured logging in the panel

No structured logging. Failed agent calls only surface as user-facing error toasts.

Files:
- New: `src/lib/log.ts` (JSON-line writer, falls back to stderr if `/var/log/vps-panel/panel.log` not writable)
- `src/lib/agent.ts` (log every agent call: method, path, status, latency_ms, error)
- `src/lib/auth.ts` (log auth events at info level)

Acceptance:
- `tail -f /var/log/vps-panel/panel.log` shows JSON lines with `ts`, `level`, `msg`, `fields`
- Log file is mode 0640, owner `vps-panel:vps-panel`

### [M9] Verify and instrument agent tracing

Confirm `tracing-subscriber` emits structured logs; add `#[instrument]` on hot paths.

Files:
- `agent/src/main.rs` (configure `tracing-subscriber` with `with_target(true)`, JSON formatter behind `RUST_LOG_JSON=1`)
- `agent/src/handlers/{ssl,sites,apps,backups}.rs` (add `#[instrument(skip_all, fields(domain = %req.domain))]` on hot paths)

Acceptance:
- `journalctl -u vps-panel-agent -o cat` shows latency-tagged entries
- `RUST_LOG=info` produces structured output with handler name + fields

### [M12] logrotate for `/var/log/vps-panel/`

No log rotation. panel.log grows forever.

Files:
- New: `scripts/logrotate/vps-panel`
- `scripts/install.sh` (install to `/etc/logrotate.d/vps-panel`)

Acceptance:
- File rotates daily, keeps 14 days, compressed, `copytruncate`
- `logrotate -d /etc/logrotate.d/vps-panel` reports no errors

### [M14] Cap `journalctl -f` log streams

`agent/src/handlers/logs.rs:24-65` follows journalctl forever per client.

Files:
- `agent/src/handlers/logs.rs`

Acceptance:
- Stream auto-closes after 30 min wall-clock (`tokio::time::timeout`)
- Stream caps at 50000 lines per session
- Client receives a final `event: end` SSE message

---

## Phase 3 - Code quality / dedup

### [M1] Extract `wrapServerAction` helper

~60 instances of `requireSession() -> try -> agent call -> writeAudit -> revalidatePath` across 15 server-actions. Single helper deletes ~300 LOC and makes audit logging consistent.

Files:
- New: `src/lib/wrapServerAction.ts`
- All `src/server-actions/*.ts` (refactor to use wrapper)

Acceptance:
- Helper signature: `wrapServerAction({ action, target, revalidate, fn })`
- After refactor, `git diff --shortstat` shows net negative LOC in `src/server-actions/`
- Existing audit log behavior preserved (manual spot-check on 3 actions)

### [M2] Extract `parseFormString` / `parseFormNumber` helpers

~23 inline `formData.get()` + parse + validate sites.

Files:
- New: `src/lib/formData.ts` (`parseFormString(fd, key, { required, maxLen, pattern })`, `parseFormNumber(fd, key, { required, min, max })`)
- All forms under `src/app/(panel)/**/*.tsx` and `src/server-actions/*.ts` that parse FormData

Acceptance:
- Helpers throw a typed `FormValidationError` with field name on bad input
- All call sites use helpers (`git grep -n 'formData.get(' src/server-actions/` returns empty)

### [M3] Centralize validators

Domain/folder/port regex inlined per-handler (e.g. `src/server-actions/sites.ts:14-15`).

Files:
- New: `src/lib/validators.ts` (export `isValidFqdn`, `isValidFolderName`, `isValidIdentifier`, `isValidPort`, `isValidUsername`)
- All inline regex in `src/server-actions/*.ts` (TS side only - agent already centralizes in `validate.rs`)

Acceptance:
- `git grep -n 'new RegExp\|/\\^.*\\$/' src/server-actions/` returns empty (or only re-exports)
- Existing forms behave identically

### [M4] Build minimal Form component library

ARCHITECTURE.md claims `src/components/Form/{Field,Input,Select,Toggle,Button}.tsx` exists; it does not. Forms use bare `<input>`. CSS modules per project convention.

Files:
- New: `src/components/Form/Field.tsx`, `Input.tsx`, `Select.tsx`, `Toggle.tsx`, `Button.tsx` and matching `.module.css`
- Refactor 3-5 highest-traffic forms first: `src/app/(panel)/domains/AddDomainForm.tsx`, `src/app/(panel)/apps/new/DeployForm.tsx`, `src/app/(panel)/cron/CronForm.tsx`

Acceptance:
- Each component exports typed props and renders error state
- No Tailwind, no global CSS - only CSS modules
- Refactored forms render identically (visual diff)

### [M5] Replace `window.confirm()` with `<ConfirmDialog>` component

`src/app/(panel)/domains/AddDomainForm.tsx:42` and others use `window.confirm`. Inaccessible, ugly.

Files:
- New: `src/components/ConfirmDialog/ConfirmDialog.tsx` + `.module.css` (async, returns `Promise<boolean>`, focus-trap, ESC-to-cancel)
- `git grep -n 'window.confirm' src/` (replace all sites)

Acceptance:
- `git grep -n 'window.confirm' src/` returns empty
- Keyboard: Tab cycles between Cancel/Confirm; Escape cancels; Enter confirms
- Manual test: delete domain triggers dialog, Cancel aborts, Confirm proceeds

### [M6] Add `loading.tsx`, `error.tsx`, `not-found.tsx` boundaries

Pages block on `Promise.all` from agent with no progressive UI. Errors bubble as Next.js default 500.

Files:
- New: `src/app/(panel)/loading.tsx`, `src/app/(panel)/error.tsx`
- New: `src/app/not-found.tsx`
- Optional per-section: `src/app/(panel)/{domains,apps,backups}/error.tsx`

Acceptance:
- Slow agent call shows skeleton/spinner instead of blank page
- Throw in a server component shows the error.tsx UI with Retry button
- Bogus URL `/foo` shows the not-found page

---

## Phase 4 - Documentation + polish

### [H1] Add LICENSE file at repo root

`Cargo.toml` may declare MIT but no `LICENSE` file exists.

Files:
- New: `LICENSE` (MIT or Apache-2.0 - see Open Question 5)
- `package.json` (add `"license": "MIT"` if not present)
- `Cargo.toml` (verify `license` field matches)

Acceptance:
- File exists and matches the license declared in package.json + Cargo.toml

### [H2] Sync ARCHITECTURE.md with reality

Drift: claims Form components and ConfirmDialog that don't exist; missing real files like `src/components/RefreshKeepAlive/`, `src/lib/acme.ts`, `src/lib/getClientIp.ts`, `src/lib/quickInstallRecipes.ts`, `src/server-actions/acme.ts`, `src/app/(panel)/domains/[domain]/EnableDnsForm.tsx`.

Files:
- `ARCHITECTURE.md` (rewrite Next.js panel section)

Acceptance:
- Every file ARCHITECTURE.md mentions exists in `git ls-files`
- Every directory under `src/` is referenced (cross-check with `find src -type d`)
- After Phase 3 lands, re-update to add the new Form/ConfirmDialog files

### [M7] Create `.env.example`

Operators have no env template; bundled with [C1].

Files:
- New: `.env.example`

Acceptance:
- Mirrors keys generated by `install.sh` and consumed by panel + agent
- All values are placeholders (`change-me`, `replace-with-openssl-rand-hex-32`)
- Comments explain each var (especially `COOKIE_SECURE`, `PANEL_HOST`, `ACME_EMAIL`)

### [L4] Write `docs/OPERATIONS.md` runbook

No troubleshooting guide for common failures.

Files:
- New: `docs/OPERATIONS.md`

Acceptance:
- Covers: agent socket missing, panel can't reach agent (token mismatch), MariaDB slow/unreachable, cert renewal failed, panel.db corrupt, restoring from a restic backup, recovering from lost TOTP, rotating secrets
- Each scenario: symptom, diagnosis commands, fix steps

### [L5] README/DEPLOY/SECURITY accuracy pass

Update after Phase 0-3 changes land.

Files:
- `README.md`, `docs/DEPLOY.md`, `docs/SECURITY.md`

Acceptance:
- README: install steps match current `install.sh`, mentions LICENSE, links OPERATIONS.md
- DEPLOY.md: deploy workflow secrets, rollback behavior, version compatibility check documented
- SECURITY.md: trust model (admin = root), CSP nonce architecture, authentication lockout/backup codes, ACME account persistence

---

## Phase 5 - Optional / post-launch

### [L1] Verify TaskTracker memory cleanup

`agent/src/ipc/tasks.rs` - confirm tasks expire/are pruned.

Files: `agent/src/ipc/tasks.rs`

Acceptance: Task records older than 24h removed by a periodic pruner; memory usage stable over 7 days.

### [L2] Prometheus `/metrics` endpoint on the agent

Operability nice-to-have.

Files: `agent/src/handlers/metrics.rs` (new), `agent/src/main.rs` (route)

Acceptance: `/metrics` returns Prometheus text format; counters for handler calls, ACME issuance, backup runs.

### [L3] Dependency update automation

No Dependabot/Renovate.

Files: New `.github/dependabot.yml` (npm + cargo + github-actions ecosystems)

Acceptance: Dependabot opens PRs weekly for dependency updates.

### [M10][M11][M13][M15][M16] Batched smaller polish

Files:
- [M10] `agent/src/handlers/backups.rs` (add `mariadb-dump --all-databases | gzip` snapshot before restic; include `agent.token`, `.env`, `acme-account.key` in DEFAULT_PATHS)
- [M11] `agent/src/handlers/ssl.rs` (when rewriting nginx vhost to add 443 block, append `add_header Strict-Transport-Security "max-age=31536000; includeSubDomains; preload" always;`)
- [M13] `agent/src/handlers/mail.rs:194-196` (add `validate::is_valid_fqdn(domain)?` before `format!()`; audit other SQL string-formatting sites for the same pattern)
- [M15] `agent/src/main.rs:74-88` (set `umask(0o077)` before `bind`, restore after `chmod`)
- [M16] `proxy.ts` and `src/app/api/{tasks,logs}/[id]/stream/route.ts` (add comment documenting the single-tenant assumption: no per-task ownership check)

Acceptance: Each bullet's behavior verified by manual test or unit test where feasible.

---

## Open questions for the user

These need a call before the corresponding tasks can be executed. Listed in priority order.

1. **[C1] Git history scrub**: BFG / `git filter-repo` on `.env.local` requires a force push to `main`. Are there other clones of this repo (other contributors, CI caches) that need to be informed before/after the rewrite? If solo development with only your local + GitHub, a force push is safe.

2. **[C1] `ADMIN_INITIAL_PASSWORD`**: Two options - (a) generate a strong random password at install time and write it to `/etc/vps-panel/initial-admin.txt` (forces ops to read once and discard), or (b) skip the password and force a first-login reset flow via email or one-time link. (a) is much less work; (b) is the cleaner long-term answer. Recommend (a) for now.

3. **[C3] Nonce-based CSP**: Removing `'unsafe-inline'` from `script-src` may break Next.js's hydration scripts on some configurations, especially with Turbopack. Recommend testing on a staging branch first. OK with a 1-2 day timeline on this task to validate, or keep `'unsafe-inline'` and accept the residual XSS risk for v1.0?

4. **[C5] Cron RCE-as-root**: Recommendation is to document the threat model (admin = root) rather than attempt regex hardening. Tightening cron command syntax creates a false sense of safety because apps deploys, systemd unit creation, and SSH key management already give the same level of access. Confirm acceptance to write the SECURITY.md section accordingly.

5. **[H1] License choice**: MIT or Apache-2.0? MIT is shorter and more permissive; Apache-2.0 includes an explicit patent grant. MIT recommended for a small internal-use panel.

6. **[H4] Backup code count + algorithm**: Standard is 8-10 codes, 8-10 chars each, base32. OK with 10 x 8-char base32, hashed with argon2id matching the password column? Or prefer SHA-256 (faster verify, weaker against offline attack on a leaked DB)?

7. **[H6] Version compatibility**: Should panel hard-fail on agent version mismatch (refuse to start) or just log a warning? Recommend warning for v1.0 since both binaries deploy from the same commit; hard-fail when there are external agent installations.

8. **[M4] Form component scope**: Phase 3 - all forms refactored to the new component lib, or just the high-traffic ones listed? Full refactor is ~20 forms. Incremental is faster but leaves visual inconsistency until done.

9. **[Phase 0 ordering]**: Comfortable with a freeze on deploys until Phase 0 lands? If not, ship Phase 0 in a single PR rather than spread it across the four tasks.
