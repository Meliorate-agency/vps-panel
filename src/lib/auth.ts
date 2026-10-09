import "server-only";
import { cookies } from "next/headers";
import { SignJWT, jwtVerify } from "jose";
import { redirect } from "next/navigation";
import {
  ACCESS_TTL_SECONDS,
  COOKIE_NAME,
  REFRESH_COOKIE,
  REFRESH_TTL_SECONDS,
  config,
} from "./config";
import { db } from "./db";
import { getSettingNumber } from "./settings";
import { randomUUID } from "node:crypto";

export type Session = {
  userId: number;
  email: string;
  role: string;
  jti: string;
  needsTotp: boolean;
  /** True when the operator ticked "Remember me" at login. Carried through
   *  the access/refresh JWTs so the TOTP re-issuance + future refresh
   *  cycles keep using the 30-day TTL instead of the configured default. */
  remember: boolean;
};

export type IssuedSession = {
  access: string;
  refresh: string;
  accessSeconds: number;
  refreshSeconds: number;
};

const SESSION_TTL_KEY = "panel.session_ttl_minutes";
const MIN_TTL_SECONDS = 60;
const MAX_TTL_SECONDS = 30 * 24 * 60 * 60;
/** TTL applied to both access and refresh tokens when the operator ticked
 *  "Remember me" at login. Same as `MAX_TTL_SECONDS` (30 days) — chosen so
 *  the cookie survives the configured `panel.session_ttl_minutes` default
 *  but doesn't outlive the session DB-row cleanup horizon. Exported so the
 *  /api/auth/refresh route can mirror the same TTL when refreshing a
 *  remembered session. */
export const REMEMBER_TTL_SECONDS = MAX_TTL_SECONDS;

const secret = () => new TextEncoder().encode(config.jwtSecret);

export function resolveSessionTtl(): { accessSeconds: number; refreshSeconds: number } {
  const fallbackMinutes = ACCESS_TTL_SECONDS / 60;
  const minutes = getSettingNumber(SESSION_TTL_KEY, fallbackMinutes);
  let seconds = Math.floor(minutes * 60);
  if (!Number.isFinite(seconds) || seconds < MIN_TTL_SECONDS) seconds = MIN_TTL_SECONDS;
  if (seconds > MAX_TTL_SECONDS) seconds = MAX_TTL_SECONDS;
  return { accessSeconds: seconds, refreshSeconds: seconds };
}

function sqliteTimestamp(secondsAhead: number): string {
  const d = new Date(Date.now() + secondsAhead * 1000);
  return d.toISOString().replace("T", " ").slice(0, 19);
}

export async function issueSession(
  userId: number,
  email: string,
  role: string,
  needsTotp: boolean,
  remember: boolean = false,
): Promise<IssuedSession> {
  const accessJti = randomUUID();
  const refreshJti = randomUUID();
  const now = Math.floor(Date.now() / 1000);
  // Remember-me overrides the configured `panel.session_ttl_minutes` default
  // for this session only. The setting still applies to non-remembered logins.
  const { accessSeconds, refreshSeconds } = remember
    ? { accessSeconds: REMEMBER_TTL_SECONDS, refreshSeconds: REMEMBER_TTL_SECONDS }
    : resolveSessionTtl();

  const access = await new SignJWT({
    sub: String(userId),
    email,
    role,
    needsTotp,
    remember,
    typ: "access",
  })
    .setProtectedHeader({ alg: "HS256" })
    .setJti(accessJti)
    .setIssuedAt(now)
    .setExpirationTime(now + accessSeconds)
    .sign(secret());

  const refresh = await new SignJWT({ sub: String(userId), typ: "refresh", needsTotp, remember })
    .setProtectedHeader({ alg: "HS256" })
    .setJti(refreshJti)
    .setIssuedAt(now)
    .setExpirationTime(now + refreshSeconds)
    .sign(secret());

  db()
    .prepare(`INSERT INTO sessions (jti, user_id, expires_at, revoked) VALUES (?, ?, ?, 0)`)
    .run(accessJti, userId, sqliteTimestamp(accessSeconds));
  db()
    .prepare(`INSERT INTO sessions (jti, user_id, expires_at, revoked) VALUES (?, ?, ?, 0)`)
    .run(refreshJti, userId, sqliteTimestamp(refreshSeconds));

  return { access, refresh, accessSeconds, refreshSeconds };
}

export async function setAuthCookies(
  access: string,
  refresh: string,
  accessSeconds: number = ACCESS_TTL_SECONDS,
  refreshSeconds: number = REFRESH_TTL_SECONDS,
) {
  const jar = await cookies();
  jar.set(COOKIE_NAME, access, {
    httpOnly: true,
    secure: config.cookieSecure,
    sameSite: "strict",
    path: "/",
    maxAge: accessSeconds,
  });
  jar.set(REFRESH_COOKIE, refresh, {
    httpOnly: true,
    secure: config.cookieSecure,
    sameSite: "strict",
    path: "/api/auth/refresh",
    maxAge: refreshSeconds,
  });
}

export async function clearAuthCookies() {
  const jar = await cookies();
  jar.delete(COOKIE_NAME);
  jar.delete(REFRESH_COOKIE);
}

export async function getSession(): Promise<Session | null> {
  const jar = await cookies();
  const token = jar.get(COOKIE_NAME)?.value;
  if (!token) return null;
  try {
    const { payload } = await jwtVerify(token, secret());
    if (payload.typ !== "access") return null;
    const jti = payload.jti as string;
    const row = db()
      .prepare(`SELECT revoked FROM sessions WHERE jti = ?`)
      .get(jti) as { revoked: number } | undefined;
    if (!row || row.revoked) return null;
    return {
      userId: Number(payload.sub),
      email: payload.email as string,
      role: payload.role as string,
      jti,
      needsTotp: Boolean(payload.needsTotp),
      remember: Boolean(payload.remember),
    };
  } catch {
    return null;
  }
}

export async function requireSession(): Promise<Session> {
  const s = await getSession();
  if (!s) redirect("/login");
  if (s.needsTotp) redirect("/totp");
  return s;
}

/**
 * Like requireSession(), but additionally requires the `admin` role. EVERY
 * state-changing / privileged server action must call this instead of
 * requireSession(): server actions are POST-invokable endpoints, so the form
 * UI is no gate — without a role check here a non-admin ("user" role) session
 * could reach root-capable operations (deploy code, reset root password, plant
 * SSH keys, open the firewall, schedule root cron, self-update, restore
 * backups). Throws for non-admins (matching the existing users.ts pattern); the
 * thrown error surfaces as a rejected server action for the rare unauthorized
 * caller. Pure read-only/list actions may stay on requireSession().
 */
export async function requireAdmin(): Promise<Session> {
  const s = await requireSession();
  if (s.role !== "admin") {
    throw new Error("forbidden: admin role required");
  }
  return s;
}

export async function revokeSession(jti: string) {
  db().prepare(`UPDATE sessions SET revoked = 1 WHERE jti = ?`).run(jti);
}

export async function syncAdminFromEnv(): Promise<void> {
  if (!config.bootstrapEmail || !config.bootstrapPassword) return;

  // Only seed when there is NO admin yet. .env is a one-time bootstrap value
  // from install time; once an admin exists in the DB the user's chosen
  // email + password are authoritative. Without this guard, changing the
  // admin email from the UI gets overwritten on next login (we'd find no
  // user with the OLD .env email and create a fresh one), and the old
  // behavior also silently reset the password back to ADMIN_INITIAL_PASSWORD
  // every time the user logged in after changing it.
  const existing = db()
    .prepare(`SELECT COUNT(*) as c FROM users WHERE role = 'admin'`)
    .get() as { c: number };
  if (existing.c > 0) return;

  const email = config.bootstrapEmail.trim().toLowerCase();
  const envPassword = config.bootstrapPassword;
  const { hashPassword } = await import("./crypto");
  const hash = await hashPassword(envPassword);
  db()
    .prepare(`INSERT INTO users (email, password_hash, role) VALUES (?, ?, 'admin')`)
    .run(email, hash);
}
