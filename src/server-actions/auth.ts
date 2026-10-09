"use server";

import { redirect } from "next/navigation";
import { db } from "@/lib/db";
import {
  clearAuthCookies,
  getSession,
  issueSession,
  revokeSession,
  setAuthCookies,
  syncAdminFromEnv,
} from "@/lib/auth";
import { dummyPasswordHash, hashPassword, verifyPassword } from "@/lib/crypto";
import { checkLogin, recordLoginAttempt } from "@/lib/rateLimit";
import { getClientIp } from "@/lib/getClientIp";
import { newSecret, totpUri, totpQr, verifyTotp } from "@/lib/totp";
import { writeAudit } from "@/lib/audit";

export type ActionResult = { ok: true } | { ok: false; error: string };

type UserRow = {
  id: number;
  email: string;
  password_hash: string;
  totp_secret: string | null;
  totp_enabled: number;
  role: string;
};

export async function loginAction(_prev: ActionResult | null, fd: FormData): Promise<ActionResult> {
  await syncAdminFromEnv();
  const email = String(fd.get("email") ?? "").trim().toLowerCase();
  const password = String(fd.get("password") ?? "");
  // HTML checkboxes only submit a value when checked. Anything truthy here
  // means the operator ticked "Remember me" — surface it as a boolean for
  // issueSession to extend the cookie lifetime to 30 days.
  const remember = !!fd.get("remember");
  if (!email || !password) return { ok: false, error: "email and password required" };

  const ip = await getClientIp();
  const rate = checkLogin(ip, email);
  if (!rate.allowed) {
    return { ok: false, error: rate.reason };
  }

  const user = db()
    .prepare(`SELECT id, email, password_hash, totp_secret, totp_enabled, role FROM users WHERE email = ?`)
    .get(email) as UserRow | undefined;
  if (!user) {
    // Same argon2 work as a real check, so the response time does not tell
    // whether the email has an account.
    await verifyPassword(await dummyPasswordHash(), password);
    recordLoginAttempt(ip, email, false);
    writeAudit({ actorId: null, action: "login.fail", target: email, result: "error", message: "no such user" });
    return { ok: false, error: "invalid email or password" };
  }

  const ok = await verifyPassword(user.password_hash, password);
  if (!ok) {
    recordLoginAttempt(ip, email, false);
    writeAudit({ actorId: user.id, action: "login.fail", target: email, result: "error", message: "bad password" });
    return { ok: false, error: "invalid email or password" };
  }
  recordLoginAttempt(ip, email, true);

  const needsTotp = Boolean(user.totp_enabled);
  const issued = await issueSession(user.id, user.email, user.role, needsTotp, remember);
  await setAuthCookies(issued.access, issued.refresh, issued.accessSeconds, issued.refreshSeconds);

  writeAudit({
    actorId: user.id,
    action: "login.ok",
    target: email,
    result: "ok",
    message: remember ? "remember=1" : undefined,
  });
  redirect(needsTotp ? "/totp" : "/dashboard");
}

export async function logoutAction(): Promise<void> {
  const s = await getSession();
  if (s) {
    revokeSession(s.jti);
    writeAudit({ actorId: s.userId, action: "logout", result: "ok" });
  }
  await clearAuthCookies();
  redirect("/login");
}

export async function startTotpEnrollAction(): Promise<{ secret: string; qrDataUrl: string; uri: string } | { error: string }> {
  const s = await getSession();
  if (!s) return { error: "not authenticated" };
  const secret = newSecret();
  db().prepare(`UPDATE users SET totp_secret = ?, totp_enabled = 0 WHERE id = ?`).run(secret, s.userId);
  const uri = totpUri(s.email, secret);
  const qrDataUrl = await totpQr(uri);
  return { secret, qrDataUrl, uri };
}

export async function confirmTotpEnrollAction(_prev: ActionResult | null, fd: FormData): Promise<ActionResult> {
  const s = await getSession();
  if (!s) return { ok: false, error: "not authenticated" };
  const code = String(fd.get("code") ?? "").trim();
  const row = db()
    .prepare(`SELECT totp_secret FROM users WHERE id = ?`)
    .get(s.userId) as { totp_secret: string | null } | undefined;
  if (!row?.totp_secret) return { ok: false, error: "no pending TOTP enrollment" };
  const step = verifyTotp(row.totp_secret, code);
  if (step === null) return { ok: false, error: "invalid code" };
  db()
    .prepare(`UPDATE users SET totp_enabled = 1, totp_last_step = ? WHERE id = ?`)
    .run(step, s.userId);
  writeAudit({ actorId: s.userId, action: "totp.enroll", result: "ok" });
  return { ok: true };
}

export async function verifyTotpAction(_prev: ActionResult | null, fd: FormData): Promise<ActionResult> {
  const s = await getSession();
  if (!s) return { ok: false, error: "not authenticated" };
  const code = String(fd.get("code") ?? "").trim();
  const ip = await getClientIp();
  const totpKey = `totp:${s.email}`;
  const rate = checkLogin(ip, totpKey);
  if (!rate.allowed) {
    return { ok: false, error: rate.reason };
  }
  const row = db()
    .prepare(`SELECT totp_secret, totp_enabled, email, role FROM users WHERE id = ?`)
    .get(s.userId) as { totp_secret: string | null; totp_enabled: number; email: string; role: string } | undefined;
  if (!row?.totp_secret || !row.totp_enabled) {
    return { ok: false, error: "TOTP not enabled" };
  }
  const step = verifyTotp(row.totp_secret, code);
  // Conditional update: only a step newer than the last accepted one
  // counts, which also settles two concurrent submissions of one code.
  const fresh =
    step !== null &&
    db()
      .prepare(`UPDATE users SET totp_last_step = ? WHERE id = ? AND totp_last_step < ?`)
      .run(step, s.userId, step).changes === 1;
  if (!fresh) {
    recordLoginAttempt(ip, totpKey, false);
    writeAudit({
      actorId: s.userId,
      action: "totp.verify.fail",
      result: "error",
      message: step === null ? undefined : "code already used",
    });
    return {
      ok: false,
      error: step === null ? "invalid code" : "that code was already used - wait for the next one",
    };
  }
  recordLoginAttempt(ip, totpKey, true);
  db().prepare(`UPDATE sessions SET revoked = 1 WHERE user_id = ? AND revoked = 0`).run(s.userId);
  // Carry the Remember-me flag from the pre-TOTP session through to the
  // post-TOTP one — otherwise the operator who ticked the box would get
  // dropped back to the default session TTL after passing the second
  // factor, which defeats the whole point of remember-me.
  const issued = await issueSession(s.userId, row.email, row.role, false, s.remember);
  await setAuthCookies(issued.access, issued.refresh, issued.accessSeconds, issued.refreshSeconds);
  writeAudit({ actorId: s.userId, action: "totp.verify.ok", result: "ok" });
  redirect("/dashboard");
}

export async function changePasswordAction(_prev: ActionResult | null, fd: FormData): Promise<ActionResult> {
  const s = await getSession();
  if (!s) return { ok: false, error: "not authenticated" };
  const current = String(fd.get("current") ?? "");
  const next = String(fd.get("next") ?? "");
  if (next.length < 12) return { ok: false, error: "new password must be at least 12 chars" };
  const row = db().prepare(`SELECT password_hash FROM users WHERE id = ?`).get(s.userId) as { password_hash: string } | undefined;
  if (!row) return { ok: false, error: "user missing" };
  if (!(await verifyPassword(row.password_hash, current))) {
    return { ok: false, error: "current password is wrong" };
  }
  const newHash = await hashPassword(next);
  db().prepare(`UPDATE users SET password_hash = ? WHERE id = ?`).run(newHash, s.userId);
  writeAudit({ actorId: s.userId, action: "password.change", result: "ok" });
  return { ok: true };
}

export async function changeAdminEmailAction(
  _prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
  const s = await getSession();
  if (!s) return { ok: false, error: "not authenticated" };
  const next = String(fd.get("email") ?? "").trim().toLowerCase();
  if (!next) return { ok: false, error: "email required" };
  if (/\s/.test(next)) return { ok: false, error: "email must not contain whitespace" };
  const at = next.indexOf("@");
  if (at <= 0 || at === next.length - 1) {
    return { ok: false, error: "invalid email format" };
  }
  if (next.length > 254) return { ok: false, error: "email too long" };
  if (next === s.email) return { ok: false, error: "that's already your email" };

  // Reject if a different user already owns the new email.
  const clash = db()
    .prepare(`SELECT id FROM users WHERE email = ? AND id != ?`)
    .get(next, s.userId) as { id: number } | undefined;
  if (clash) {
    return { ok: false, error: "another user already has that email" };
  }

  db().prepare(`UPDATE users SET email = ? WHERE id = ?`).run(next, s.userId);
  // Revoke every session for this user. The JWT carries the email as a
  // claim — leaving sessions live would show the old email in the Topbar
  // until the next login, plus the audit log would attribute new actions
  // to the stale claim. Forcing re-login keeps the JWT and DB in sync.
  db()
    .prepare(`UPDATE sessions SET revoked = 1 WHERE user_id = ? AND revoked = 0`)
    .run(s.userId);
  writeAudit({
    actorId: s.userId,
    action: "admin-email.change",
    target: next,
    result: "ok",
    message: `from ${s.email}`,
  });
  await clearAuthCookies();
  return { ok: true };
}
