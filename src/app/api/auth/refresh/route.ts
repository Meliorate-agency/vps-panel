import { NextRequest, NextResponse } from "next/server";
import { jwtVerify, SignJWT } from "jose";
import { randomUUID } from "node:crypto";
import { COOKIE_NAME, REFRESH_COOKIE, config } from "@/lib/config";
import { db } from "@/lib/db";
import { resolveSessionTtl, REMEMBER_TTL_SECONDS } from "@/lib/auth";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function sqliteTimestamp(secondsAhead: number): string {
  const d = new Date(Date.now() + secondsAhead * 1000);
  return d.toISOString().replace("T", " ").slice(0, 19);
}

export async function POST(req: NextRequest) {
  const refresh = req.cookies.get(REFRESH_COOKIE)?.value;
  if (!refresh) return new NextResponse("no refresh token", { status: 401 });

  const secret = new TextEncoder().encode(config.jwtSecret);
  let payload: Record<string, unknown>;
  try {
    const v = await jwtVerify(refresh, secret);
    payload = v.payload as Record<string, unknown>;
  } catch {
    return new NextResponse("invalid refresh token", { status: 401 });
  }
  if (payload.typ !== "refresh") {
    return new NextResponse("not a refresh token", { status: 401 });
  }

  const oldJti = payload.jti as string;
  const userId = Number(payload.sub);
  const session = db()
    .prepare(`SELECT revoked FROM sessions WHERE jti = ?`)
    .get(oldJti) as { revoked: number } | undefined;
  if (!session || session.revoked) {
    return new NextResponse("session revoked", { status: 401 });
  }

  const user = db()
    .prepare(
      `SELECT email, role, totp_enabled FROM users WHERE id = ?`,
    )
    .get(userId) as { email: string; role: string; totp_enabled: number } | undefined;
  if (!user) return new NextResponse("user gone", { status: 401 });

  const needsTotp = Boolean(payload.needsTotp) && Boolean(user.totp_enabled);
  // Carry Remember-me forward across the refresh cycle. Without this the
  // first refresh after a remembered login would silently downgrade the
  // 30-day cookie back to the configured `panel.session_ttl_minutes` default.
  const remember = Boolean(payload.remember);

  const { accessSeconds, refreshSeconds } = remember
    ? { accessSeconds: REMEMBER_TTL_SECONDS, refreshSeconds: REMEMBER_TTL_SECONDS }
    : resolveSessionTtl();
  const now = Math.floor(Date.now() / 1000);
  const newAccessJti = randomUUID();
  const newRefreshJti = randomUUID();

  const access = await new SignJWT({
    sub: String(userId),
    email: user.email,
    role: user.role,
    needsTotp,
    remember,
    typ: "access",
  })
    .setProtectedHeader({ alg: "HS256" })
    .setJti(newAccessJti)
    .setIssuedAt(now)
    .setExpirationTime(now + accessSeconds)
    .sign(secret);

  const newRefresh = await new SignJWT({ sub: String(userId), typ: "refresh", needsTotp, remember })
    .setProtectedHeader({ alg: "HS256" })
    .setJti(newRefreshJti)
    .setIssuedAt(now)
    .setExpirationTime(now + refreshSeconds)
    .sign(secret);

  const tx = db().transaction(() => {
    db().prepare(`UPDATE sessions SET revoked = 1 WHERE jti = ?`).run(oldJti);
    db()
      .prepare(`INSERT INTO sessions (jti, user_id, expires_at, revoked) VALUES (?, ?, ?, 0)`)
      .run(newAccessJti, userId, sqliteTimestamp(accessSeconds));
    db()
      .prepare(`INSERT INTO sessions (jti, user_id, expires_at, revoked) VALUES (?, ?, ?, 0)`)
      .run(newRefreshJti, userId, sqliteTimestamp(refreshSeconds));
  });
  tx();

  const res = NextResponse.json({ ok: true });
  res.cookies.set(COOKIE_NAME, access, {
    httpOnly: true,
    secure: config.cookieSecure,
    sameSite: "strict",
    path: "/",
    maxAge: accessSeconds,
  });
  res.cookies.set(REFRESH_COOKIE, newRefresh, {
    httpOnly: true,
    secure: config.cookieSecure,
    sameSite: "strict",
    path: "/api/auth/refresh",
    maxAge: refreshSeconds,
  });
  return res;
}
