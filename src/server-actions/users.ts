"use server";

import { revalidatePath } from "next/cache";
import { agent, type SystemUserRow } from "@/lib/agent";
import { requireSession } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";
import { db } from "@/lib/db";
import { hashPassword } from "@/lib/crypto";

export type ActionResult = { ok: true } | { ok: false; error: string };

// Linux account names: useradd's default NAME_REGEX. Kept in lockstep with
// the same constant in agent/src/handlers/system.rs::is_safe_unix_name so the
// panel can reject obvious bad input before the round trip to the socket.
const UNIX_NAME_RE = /^[a-z_][a-z0-9_-]{0,31}$/;

export type PanelUserRow = {
  id: number;
  email: string;
  role: string;
  totp_enabled: number;
  created_at: string;
};

const EMAIL_RE = /^[^@\s]+@[^@\s]+\.[^@\s]+$/;
const VALID_ROLES = ["admin", "user"] as const;
type Role = (typeof VALID_ROLES)[number];

/**
 * Read helper: returns every panel-login user (`users` table) ordered by id.
 * Used by the /users page server component to render the table. `async`
 * to satisfy "use server" — the SQLite read itself is synchronous.
 */
export async function listPanelUsers(): Promise<PanelUserRow[]> {
  await requireSession();
  return db()
    .prepare(
      `SELECT id, email, role, totp_enabled, created_at
         FROM users ORDER BY id ASC`,
    )
    .all() as PanelUserRow[];
}

/**
 * Create a new panel-login user. Admin-only — non-admins shouldn't be
 * able to grant access to the panel. On success the operator can immediately
 * sign in with the new credentials.
 */
export async function createPanelUserAction(
  _prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
  const s = await requireSession();
  if (s.role !== "admin") {
    return { ok: false, error: "admin role required" };
  }
  const email = String(fd.get("email") ?? "").trim().toLowerCase();
  const password = String(fd.get("password") ?? "");
  const confirm = String(fd.get("confirm") ?? "");
  const roleRaw = String(fd.get("role") ?? "admin").trim().toLowerCase();

  if (!email) return { ok: false, error: "email required" };
  if (!EMAIL_RE.test(email)) return { ok: false, error: "invalid email" };
  if (email.length > 254) return { ok: false, error: "email too long" };
  if (password.length < 12) {
    return { ok: false, error: "password must be at least 12 characters" };
  }
  if (password !== confirm) {
    return { ok: false, error: "passwords do not match" };
  }
  if (/[\r\n]/.test(password)) {
    return { ok: false, error: "password must not contain newlines" };
  }
  if (!(VALID_ROLES as readonly string[]).includes(roleRaw)) {
    return { ok: false, error: "invalid role" };
  }
  const role = roleRaw as Role;

  const existing = db()
    .prepare(`SELECT id FROM users WHERE email = ?`)
    .get(email) as { id: number } | undefined;
  if (existing) {
    return { ok: false, error: "a user with that email already exists" };
  }

  try {
    const hash = await hashPassword(password);
    db()
      .prepare(
        `INSERT INTO users (email, password_hash, role) VALUES (?, ?, ?)`,
      )
      .run(email, hash, role);
    writeAudit({
      actorId: s.userId,
      action: "users.panel.create",
      target: email,
      result: "ok",
      message: `role=${role}`,
    });
    revalidatePath("/users");
    return { ok: true };
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "users.panel.create",
      target: email,
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
}

/**
 * Admin-reset for another panel user's password. Revokes all live sessions
 * for that user so the next request bounces them to /login — prevents a
 * stale session from outliving the password change. The current user can't
 * reset their OWN password here (use /settings → change password, which
 * requires the existing password as a defense-in-depth check).
 */
export async function resetPanelUserPasswordAction(
  userId: number,
  _prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
  const s = await requireSession();
  if (s.role !== "admin") {
    return { ok: false, error: "admin role required" };
  }
  if (userId === s.userId) {
    return {
      ok: false,
      error: "use /settings to change your own password (requires current pw)",
    };
  }
  const password = String(fd.get("password") ?? "");
  const confirm = String(fd.get("confirm") ?? "");
  if (password.length < 12) {
    return { ok: false, error: "password must be at least 12 characters" };
  }
  if (password !== confirm) {
    return { ok: false, error: "passwords do not match" };
  }
  if (/[\r\n]/.test(password)) {
    return { ok: false, error: "password must not contain newlines" };
  }

  const target = db()
    .prepare(`SELECT email FROM users WHERE id = ?`)
    .get(userId) as { email: string } | undefined;
  if (!target) return { ok: false, error: "user not found" };

  try {
    const hash = await hashPassword(password);
    db()
      .prepare(`UPDATE users SET password_hash = ? WHERE id = ?`)
      .run(hash, userId);
    // Force re-login on every device for the targeted user.
    db()
      .prepare(`UPDATE sessions SET revoked = 1 WHERE user_id = ? AND revoked = 0`)
      .run(userId);
    writeAudit({
      actorId: s.userId,
      action: "users.panel.reset_password",
      target: target.email,
      result: "ok",
    });
    revalidatePath("/users");
    return { ok: true };
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "users.panel.reset_password",
      target: target.email,
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
}

/**
 * Delete a panel user. Self-protection plus a last-admin guard so the
 * operator can't lock themselves out of the panel by removing the only
 * account that can log in. Sessions for the deleted user are explicitly
 * marked revoked first so any open browser tab gets bounced on its next
 * request, even if the DELETE FROM sessions cascades silently.
 */
export async function deletePanelUserAction(userId: number): Promise<void> {
  const s = await requireSession();
  if (s.role !== "admin") throw new Error("admin role required");
  if (userId === s.userId) throw new Error("cannot delete yourself");

  const target = db()
    .prepare(`SELECT email, role FROM users WHERE id = ?`)
    .get(userId) as { email: string; role: string } | undefined;
  if (!target) throw new Error("user not found");

  if (target.role === "admin") {
    const others = db()
      .prepare(`SELECT COUNT(*) AS c FROM users WHERE role = 'admin' AND id != ?`)
      .get(userId) as { c: number };
    if (others.c === 0) {
      throw new Error("cannot delete the last admin");
    }
  }

  db().prepare(`UPDATE sessions SET revoked = 1 WHERE user_id = ?`).run(userId);
  db().prepare(`DELETE FROM sessions WHERE user_id = ?`).run(userId);
  db().prepare(`DELETE FROM users WHERE id = ?`).run(userId);

  writeAudit({
    actorId: s.userId,
    action: "users.panel.delete",
    target: target.email,
    result: "ok",
    message: `role=${target.role}`,
  });
  revalidatePath("/users");
}

// Reset the OS-level `root` password via the agent. Audit-logged. The actual
// password is never stored anywhere by the panel — it only travels:
//   browser  -- HTTPS -->  panel  -- unix socket -->  agent  --> chpasswd
// SSH accepts it only when PasswordAuthentication is on and PermitRootLogin
// is yes; otherwise it is only for the Hetzner/OVH web console. The /users
// page reads the live sshd settings and says which case applies.
export async function setRootPasswordAction(
  _prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
  const s = await requireSession();
  if (s.role !== "admin") {
    return { ok: false, error: "forbidden: admin role required" };
  }
  const password = String(fd.get("password") ?? "");
  const confirm = String(fd.get("confirm") ?? "");

  if (password.length < 12) {
    return { ok: false, error: "password must be at least 12 characters" };
  }
  if (password !== confirm) {
    return { ok: false, error: "passwords do not match" };
  }
  if (/[\r\n]/.test(password)) {
    return { ok: false, error: "password must not contain newlines" };
  }

  try {
    await agent.system.setRootPassword(password);
    writeAudit({
      actorId: s.userId,
      action: "users.root_password_reset",
      target: "root",
      result: "ok",
    });
    return { ok: true };
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "users.root_password_reset",
      target: "root",
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
}

// ─── Linux (system) users ────────────────────────────────────────────────

/**
 * Read helper for the /users page server component: returns every Linux user
 * the agent's filter considers a candidate (root + UID >= 33, minus service
 * blocklist). The page splits these into "human" (root + UID >= 1000) and
 * service users for display. Empty list on socket / agent error so the page
 * still renders.
 */
export async function listSystemUsers(): Promise<SystemUserRow[]> {
  await requireSession();
  try {
    return await agent.system.users();
  } catch {
    return [];
  }
}

/**
 * Create a Linux account via the agent (`useradd -m` + `chpasswd`, optional
 * `usermod -aG sudo`). Admin-only — anyone with shell access on the host can
 * pivot to root if they're in the sudo group, so this is a privileged op.
 * The password travels: browser → HTTPS → panel → unix socket → agent →
 * chpasswd; it is never persisted by the panel.
 */
export async function createSystemUserAction(
  _prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
  const s = await requireSession();
  if (s.role !== "admin") {
    return { ok: false, error: "admin role required" };
  }

  const name = String(fd.get("name") ?? "").trim().toLowerCase();
  const password = String(fd.get("password") ?? "");
  const confirm = String(fd.get("confirm") ?? "");
  const sudo = String(fd.get("sudo") ?? "") === "on";
  const shellRaw = String(fd.get("shell") ?? "").trim();
  const shell = shellRaw === "" ? "/bin/bash" : shellRaw;

  if (!name) return { ok: false, error: "username required" };
  if (!UNIX_NAME_RE.test(name)) {
    return {
      ok: false,
      error:
        "username must start with a-z or _, contain only a-z, 0-9, _ or -, max 32 chars",
    };
  }
  if (password.length < 12) {
    return { ok: false, error: "password must be at least 12 characters" };
  }
  if (password !== confirm) {
    return { ok: false, error: "passwords do not match" };
  }
  if (/[\r\n:]/.test(password)) {
    return {
      ok: false,
      error: "password must not contain newlines or colons",
    };
  }

  try {
    await agent.system.createUser({ name, password, sudo, shell });
    writeAudit({
      actorId: s.userId,
      action: "users.linux.create",
      target: name,
      result: "ok",
      message: `sudo=${sudo} shell=${shell}`,
    });
    revalidatePath("/users");
    return { ok: true };
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "users.linux.create",
      target: name,
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
}

/**
 * Reset an existing Linux user's password. Admin-only. Refuses `root`
 * (operators use the dedicated /system/root-password endpoint via
 * setRootPasswordAction). The agent does the same check; mirrored here so we
 * fail fast with a clearer message.
 */
export async function setSystemUserPasswordAction(
  name: string,
  _prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
  const s = await requireSession();
  if (s.role !== "admin") {
    return { ok: false, error: "admin role required" };
  }
  const target = name.trim().toLowerCase();
  if (!UNIX_NAME_RE.test(target)) {
    return { ok: false, error: "invalid username" };
  }
  if (target === "root") {
    return {
      ok: false,
      error: "use the Root password section to change root's password",
    };
  }
  const password = String(fd.get("password") ?? "");
  const confirm = String(fd.get("confirm") ?? "");
  if (password.length < 12) {
    return { ok: false, error: "password must be at least 12 characters" };
  }
  if (password !== confirm) {
    return { ok: false, error: "passwords do not match" };
  }
  if (/[\r\n:]/.test(password)) {
    return {
      ok: false,
      error: "password must not contain newlines or colons",
    };
  }

  try {
    await agent.system.setUserPassword(target, password);
    writeAudit({
      actorId: s.userId,
      action: "users.linux.reset_password",
      target,
      result: "ok",
    });
    revalidatePath("/users");
    return { ok: true };
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "users.linux.reset_password",
      target,
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
}

/**
 * Delete a Linux account via the agent's `userdel` wrapper. Admin-only.
 * `removeHome=true` passes `-r` to wipe /home/<name> and the mail spool;
 * default false leaves the home dir intact in case the operator changes
 * their mind. The agent itself refuses to touch `root` and reserved system
 * accounts; we mirror those guards client-side for a faster, clearer error.
 */
export async function deleteSystemUserAction(
  name: string,
  removeHome: boolean,
): Promise<void> {
  const s = await requireSession();
  if (s.role !== "admin") throw new Error("admin role required");
  const target = name.trim().toLowerCase();
  if (!UNIX_NAME_RE.test(target)) throw new Error("invalid username");
  if (target === "root") throw new Error("cannot delete root");

  try {
    await agent.system.deleteUser(target, { removeHome });
    writeAudit({
      actorId: s.userId,
      action: "users.linux.delete",
      target,
      result: "ok",
      message: `remove_home=${removeHome}`,
    });
    revalidatePath("/users");
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "users.linux.delete",
      target,
      result: "error",
      message: msg,
    });
    throw new Error(msg);
  }
}
