import "server-only";
import { db } from "./db";
import { ipAllowed, parseIp } from "./ipMatch";
import { getSetting } from "./settings";

const PER_IP = 5;
const PER_EMAIL = 10;
const WINDOW_MIN = 15;
/** How long an address counts as the account's own after a sign-in from it.
 *  lib/housekeeping.ts keeps the successful rows this long. */
export const KNOWN_ADDRESS_DAYS = 30;

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
  if (emailCount.n >= PER_EMAIL && !knownAddressHasRoom(ip, email)) {
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

/**
 * The per-account limit is reached. Whether a sign-in from `ip` may still go
 * ahead: `ip` is one of the account's own addresses, and the failures that
 * came from those addresses alone are under the limit.
 *
 * The limit counts failures from every address, so anyone who knew the
 * admin's email could keep the account locked from anywhere: ten wrong
 * passwords every 15 minutes. An address is the account's own when it is on
 * the operator's trusted list (`panel.ip_allowlist`) or a sign-in for this
 * account succeeded from it in the last KNOWN_ADDRESS_DAYS days. Failures
 * from elsewhere no longer count against a sign-in from one of those.
 *
 * What an attacker gains: nothing from an address of their own, where both
 * limits apply as before. From one of the account's own addresses (the same
 * office, VPN exit or home connection) they have what they always had: the
 * per-IP limit, and a per-account limit of the same size, which now counts
 * the failures from the account's own addresses only. A whole trusted range
 * or IPv6 /64 therefore still shares ten failures in 15 minutes. An address
 * cannot be claimed: it is the one nginx saw the connection come from
 * (X-Real-IP), and it becomes the account's own only through a correct
 * password or the operator's list.
 *
 * IPv6: the same /64 counts as the same address. Privacy extensions give a
 * device a new address in its /64 every day, so the exact one from last
 * week's sign-in would never match.
 */
function knownAddressHasRoom(ip: string, email: string): boolean {
  const trusted = getSetting("panel.ip_allowlist", "")
    .split(",")
    .map((e) => e.trim())
    .filter(Boolean);
  const seen = db()
    .prepare(
      `SELECT DISTINCT ip FROM login_attempts
       WHERE email = ? AND success = 1 AND ts > datetime('now', '-' || ? || ' days')`,
    )
    .all(email, KNOWN_ADDRESS_DAYS) as { ip: string }[];
  const net64 = (raw: string) => {
    const a = parseIp(raw);
    return a?.v === 6 ? a.n >> BigInt(64) : null;
  };
  const exact = new Set(seen.map((r) => r.ip));
  const nets = new Set(seen.map((r) => net64(r.ip)).filter((n) => n !== null));
  const known = (addr: string) => {
    if (exact.has(addr) || ipAllowed(addr, trusted)) return true;
    const n = net64(addr);
    return n !== null && nets.has(n);
  };
  if (!known(ip)) return false;

  const failures = db()
    .prepare(
      `SELECT ip, COUNT(*) AS n FROM login_attempts
       WHERE email = ? AND success = 0 AND ts > datetime('now', '-' || ? || ' minutes')
       GROUP BY ip`,
    )
    .all(email, WINDOW_MIN) as { ip: string; n: number }[];
  const fromKnown = failures.reduce((sum, f) => (known(f.ip) ? sum + f.n : sum), 0);
  return fromKnown < PER_EMAIL;
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
