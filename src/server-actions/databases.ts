"use server";

import { revalidatePath } from "next/cache";
import { agent } from "@/lib/agent";
import { requireAdmin } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";

export type ActionResult<T = undefined> =
  | { ok: true; data?: T }
  | { ok: false; error: string };

type DbOp =
  | "create_user"
  | "create_database"
  | "drop_user"
  | "drop_database"
  | "grant"
  | "revoke"
  | "set_password";

// Pull the agent's body out of `agent <status>: <body>`, then unwrap the
// agent's own `mariadb: Server error:` / backtick wrapper or
// `invalid input:` prefix so we're left with the bare message.
function unwrapAgentError(raw: string): string {
  const stripped = raw.replace(/^agent \d+: /, "");
  let inner = stripped;
  try {
    const parsed = JSON.parse(stripped);
    if (parsed && typeof parsed.error === "string") inner = parsed.error;
  } catch {
    // not JSON — leave as-is
  }
  return inner
    .replace(/^mariadb: Server error: `?/, "")
    .replace(/`$/, "")
    .replace(/^invalid input: /, "");
}

// Translate MariaDB error text into one-liner operator-friendly messages.
// Falls back to the unwrapped message when we don't recognize the code.
function friendlyDbError(op: DbOp, raw: string): string {
  const inner = unwrapAgentError(raw);

  // 1396: Operation CREATE/DROP/RENAME/ALTER USER failed for 'name'@'host'
  // CREATE → already exists; DROP/RENAME/ALTER → does not exist (e.g. trying
  // to change the password of a user@host that isn't there).
  const m1396 = inner.match(
    /ERROR HY000 \(1396\): Operation (CREATE|DROP|RENAME|ALTER) USER failed for ('[^']*'@'[^']*')/i,
  );
  if (m1396) {
    const verb = m1396[1].toUpperCase();
    const target = m1396[2];
    return verb === "CREATE"
      ? `User ${target} already exists.`
      : `User ${target} does not exist.`;
  }

  // 1007: Can't create database '<name>'; database exists
  const m1007 = inner.match(/\(1007\): Can't create database '([^']+)'/i);
  if (m1007) return `Database '${m1007[1]}' already exists.`;

  // 1008: Can't drop database '<name>'; database doesn't exist
  const m1008 = inner.match(/\(1008\): Can't drop database '([^']+)'/i);
  if (m1008) return `Database '${m1008[1]}' does not exist.`;

  // 1141: There is no such grant defined for user — e.g. the grant was
  // already revoked in another tab and the chip clicked again.
  if (/\(1141\):/.test(inner)) return "That grant no longer exists.";

  // 1064: SQL syntax error — shouldn't happen, but kept for completeness.
  if (/\(1064\):/.test(inner))
    return "SQL syntax error in the panel — please report this.";

  return inner;
}

function rawErrorMessage(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export async function createDbUserAction(
  _prev: ActionResult<{ password: string; user: string; host: string }> | null,
  fd: FormData,
): Promise<ActionResult<{ password: string; user: string; host: string }>> {
  const s = await requireAdmin();
  const user = String(fd.get("user") ?? "").trim();
  const host = String(fd.get("host") ?? "localhost").trim();
  const password = String(fd.get("password") ?? "").trim() || undefined;
  try {
    const resp = await agent.databases.createUser({ user, host, password });
    writeAudit({ actorId: s.userId, action: "db.users.create", target: `${user}@${host}`, result: "ok" });
    revalidatePath("/databases");
    return {
      ok: true,
      data: { password: resp.initial_password, user: resp.user, host: resp.host },
    };
  } catch (e: unknown) {
    const raw = rawErrorMessage(e);
    writeAudit({
      actorId: s.userId,
      action: "db.users.create",
      target: `${user}@${host}`,
      result: "error",
      message: raw,
    });
    return { ok: false, error: friendlyDbError("create_user", raw) };
  }
}

export async function createDatabaseAction(
  _prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
  const s = await requireAdmin();
  const name = String(fd.get("name") ?? "").trim();
  try {
    await agent.databases.createDatabase(name);
    writeAudit({ actorId: s.userId, action: "db.databases.create", target: name, result: "ok" });
    revalidatePath("/databases");
    return { ok: true };
  } catch (e: unknown) {
    const raw = rawErrorMessage(e);
    writeAudit({
      actorId: s.userId,
      action: "db.databases.create",
      target: name,
      result: "error",
      message: raw,
    });
    return { ok: false, error: friendlyDbError("create_database", raw) };
  }
}

export async function grantAction(
  _prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
  const s = await requireAdmin();
  const user = String(fd.get("user") ?? "").trim();
  const host = String(fd.get("host") ?? "localhost").trim();
  const database = String(fd.get("database") ?? "").trim();
  const privileges = String(fd.get("privileges") ?? "ALL PRIVILEGES");
  try {
    await agent.databases.grant({ user, host, database, privileges });
    writeAudit({ actorId: s.userId, action: "db.grant", target: `${user}@${host}->${database}`, result: "ok" });
    revalidatePath("/databases");
    return { ok: true };
  } catch (e: unknown) {
    const raw = rawErrorMessage(e);
    writeAudit({
      actorId: s.userId,
      action: "db.grant",
      target: `${user}@${host}->${database}`,
      result: "error",
      message: raw,
    });
    return { ok: false, error: friendlyDbError("grant", raw) };
  }
}

export async function deleteDbUserAction(user: string, host: string): Promise<void> {
  const s = await requireAdmin();
  try {
    await agent.databases.deleteUser(user, host);
    writeAudit({ actorId: s.userId, action: "db.users.delete", target: `${user}@${host}`, result: "ok" });
  } catch (e: unknown) {
    const raw = rawErrorMessage(e);
    writeAudit({
      actorId: s.userId,
      action: "db.users.delete",
      target: `${user}@${host}`,
      result: "error",
      message: raw,
    });
    throw new Error(friendlyDbError("drop_user", raw));
  }
  revalidatePath("/databases");
}

export async function deleteDatabaseAction(name: string): Promise<void> {
  const s = await requireAdmin();
  try {
    await agent.databases.deleteDatabase(name);
    writeAudit({ actorId: s.userId, action: "db.databases.delete", target: name, result: "ok" });
  } catch (e: unknown) {
    const raw = rawErrorMessage(e);
    writeAudit({
      actorId: s.userId,
      action: "db.databases.delete",
      target: name,
      result: "error",
      message: raw,
    });
    throw new Error(friendlyDbError("drop_database", raw));
  }
  revalidatePath("/databases");
}

/**
 * Revoke a user's db-level grants on one database. Bound into the × on the
 * Granted-users chips; returns ActionResult (never throws) so the chip
 * component can render the error inline in its confirm dialog.
 */
export async function revokeDbGrantAction(
  user: string,
  host: string,
  database: string,
): Promise<ActionResult> {
  const s = await requireAdmin();
  try {
    await agent.databases.revoke({ user, host, database });
    writeAudit({
      actorId: s.userId,
      action: "db.revoke",
      target: `${user}@${host} -x ${database}`,
      result: "ok",
    });
    revalidatePath("/databases");
    return { ok: true };
  } catch (e: unknown) {
    const raw = rawErrorMessage(e);
    writeAudit({
      actorId: s.userId,
      action: "db.revoke",
      target: `${user}@${host} -x ${database}`,
      result: "error",
      message: raw,
    });
    return { ok: false, error: friendlyDbError("revoke", raw) };
  }
}

/**
 * Make sure the staging folder for SQL dumps exists in the Drive root.
 * Uploads land there via the existing /api/files/upload route (which reuses
 * the nginx streaming carve-out), so this is the only prep step the import
 * form needs. A 409 means the folder is already there — success for us.
 */
export async function prepareSqlImportAction(): Promise<ActionResult> {
  await requireAdmin();
  try {
    await agent.files.mkdir("drive", "", "sql-dumps");
    return { ok: true };
  } catch (e: unknown) {
    const raw = rawErrorMessage(e);
    if (/^agent 409: /.test(raw)) return { ok: true };
    return { ok: false, error: unwrapAgentError(raw) };
  }
}

/**
 * Kick off the import of an already-staged dump. Returns the agent task id
 * so the form can stream the live log; the import itself keeps running even
 * if the browser goes away. Audited at dispatch — the outcome lives in the
 * task log on /tasks.
 */
export async function importSqlDumpAction(input: {
  /** Path relative to the Drive root, e.g. `sql-dumps/backup.sql`. */
  path: string;
  /** Target database — headerless dumps only. */
  targetDb?: string;
  createTarget?: boolean;
  dropFirst?: boolean;
}): Promise<ActionResult<{ taskId: string; database: string }>> {
  const s = await requireAdmin();
  try {
    const resp = await agent.databases.importSql({
      root: "drive",
      path: input.path,
      target_db: input.targetDb || undefined,
      create_target: input.createTarget ?? false,
      drop_first: input.dropFirst ?? false,
    });
    writeAudit({
      actorId: s.userId,
      action: "db.import",
      target: `drive:${input.path} -> ${resp.database}`,
      result: "ok",
      message: input.dropFirst ? "drop_first" : undefined,
    });
    return { ok: true, data: { taskId: resp.task_id, database: resp.database } };
  } catch (e: unknown) {
    const raw = rawErrorMessage(e);
    writeAudit({
      actorId: s.userId,
      action: "db.import",
      target: `drive:${input.path}`,
      result: "error",
      message: raw,
    });
    return { ok: false, error: unwrapAgentError(raw) };
  }
}

export async function setDbUserPasswordAction(
  user: string,
  host: string,
  password: string,
): Promise<ActionResult<{ password: string }>> {
  const s = await requireAdmin();
  try {
    const resp = await agent.databases.setUserPassword(user, {
      host,
      password: password || undefined,
    });
    writeAudit({
      actorId: s.userId,
      action: "db.users.password",
      target: `${user}@${host}`,
      result: "ok",
    });
    return { ok: true, data: { password: resp.password } };
  } catch (e: unknown) {
    const raw = rawErrorMessage(e);
    writeAudit({
      actorId: s.userId,
      action: "db.users.password",
      target: `${user}@${host}`,
      result: "error",
      message: raw,
    });
    return { ok: false, error: friendlyDbError("set_password", raw) };
  }
}
