import "server-only";
import { db } from "./db";

const PER_IP = 5;
const PER_EMAIL = 10;
const WINDOW_MIN = 15;

export type RateCheck = {
  allowed: boolean;
  remaining: number;
  retryAfterSec: number;
  reason: string;
};

export function checkLogin(ip: string, email: string): RateCheck {
  const ipCount = db()
    .prepare(
      `SELECT COUNT(*) AS n FROM login_attempts
       WHERE ip = ? AND success = 0 AND ts > datetime('now', '-' || ? || ' minutes')`,
    )
    .get(ip, WINDOW_MIN) as { n: number };
  const emailCount = db()
    .prepare(
      `SELECT COUNT(*) AS n FROM login_attempts
       WHERE email = ? AND success = 0 AND ts > datetime('now', '-' || ? || ' minutes')`,
    )
    .get(email, WINDOW_MIN) as { n: number };

  if (ipCount.n >= PER_IP) {
    return {
      allowed: false,
      remaining: 0,
      retryAfterSec: WINDOW_MIN * 60,
      reason: `too many attempts from this IP; wait ${WINDOW_MIN} minutes`,
    };
  }
  if (emailCount.n >= PER_EMAIL) {
    return {
      allowed: false,
      remaining: 0,
      retryAfterSec: WINDOW_MIN * 60,
      reason: `too many attempts for this account; wait ${WINDOW_MIN} minutes`,
    };
  }
  return {
    allowed: true,
    remaining: Math.max(0, PER_IP - ipCount.n - 1),
    retryAfterSec: 0,
    reason: "",
  };
}

export function recordLoginAttempt(ip: string, email: string, success: boolean) {
  db()
    .prepare(`INSERT INTO login_attempts (ip, email, success) VALUES (?, ?, ?)`)
    .run(ip, email, success ? 1 : 0);
}
