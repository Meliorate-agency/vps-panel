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

/** Returns the row id, for markLoginSucceeded. */
export function recordLoginAttempt(ip: string, email: string, success: boolean): number {
  const res = db()
    .prepare(`INSERT INTO login_attempts (ip, email, success) VALUES (?, ?, ?)`)
    .run(ip, email, success ? 1 : 0);
  return Number(res.lastInsertRowid);
}

/**
 * A login counts as failed from the moment it passes checkLogin until its
 * password is verified. Recording the failure only after the (slow) password
 * check let any number of attempts sent at once all pass the limit first.
 */
export function markLoginSucceeded(attemptId: number) {
  db().prepare(`UPDATE login_attempts SET success = 1 WHERE id = ?`).run(attemptId);
}
