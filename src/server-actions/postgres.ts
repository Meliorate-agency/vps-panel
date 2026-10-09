"use server";

import { revalidatePath } from "next/cache";
import { agent } from "@/lib/agent";
import { requireAdmin } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";

export type ActionResult<T = undefined> =
  | { ok: true; data?: T }
  | { ok: false; error: string };

type PgOp =
  | "create_role"
  | "create_database"
  | "update_database"
  | "drop_role"
  | "drop_database"
  | "grant"
  | "enable_pgvector"
  | "disable_pgvector"
  | "set_password";

/**
 * Pull the agent's body out of `agent <status>: <body>`, then unwrap the
 * agent's own JSON envelope to leave us with the bare message. Same
 * pattern as the MariaDB action error mapper — only the engine-specific
 * prefix differs.
 */
function unwrapAgentError(raw: string): string {
  const stripped = raw.replace(/^agent \d+: /, "");
  let inner = stripped;
  try {
    const parsed = JSON.parse(stripped);
    if (parsed && typeof parsed.error === "string") inner = parsed.error;
  } catch {
    // not JSON — leave as-is
  }
  // The Postgres handler shells out to psql, so errors come back as
  // psql's stderr embedded in our `Subprocess` error variant. Strip the
  // boilerplate down to the human-meaningful bit.
  return inner
    .replace(/^subprocess `sudo` failed:[\s\S]*?stderr: /, "")
    .replace(/^subprocess [\s\S]*? failed: /, "")
    .replace(/^invalid input: /, "")
    .replace(/^psql: error: /, "")
    .trim();
}

/**
 * Turn the most common Postgres error strings into one-liner
 * operator-friendly messages. Covers the same ground as the MariaDB
 * friendlyDbError — already-exists / not-found / dependency on other
 * objects. Falls back to the unwrapped raw message otherwise.
 */
function friendlyPgError(op: PgOp, raw: string): string {
  const inner = unwrapAgentError(raw);

  // role "x" already exists / role "x" does not exist
  const roleExists = inner.match(/role "([^"]+)" already exists/i);
  if (roleExists) return `Role "${roleExists[1]}" already exists.`;
  const roleMissing = inner.match(/role "([^"]+)" does not exist/i);
  if (roleMissing) return `Role "${roleMissing[1]}" does not exist.`;

  // database "x" already exists / database "x" does not exist
  const dbExists = inner.match(/database "([^"]+)" already exists/i);
  if (dbExists) return `Database "${dbExists[1]}" already exists.`;
  const dbMissing = inner.match(/database "([^"]+)" does not exist/i);
  if (dbMissing) return `Database "${dbMissing[1]}" does not exist.`;

  // database "x" is being accessed by other users — hit when renaming a DB
  // that pgAdmin / an app still holds a connection to.
  if (/is being accessed by other users/i.test(inner)) {
    return "Can't rename — other clients are still connected to the database. Close pgAdmin query tools and any app connections to it, then retry.";
  }

  // role "x" cannot be dropped because some objects depend on it
  const dependsOn = inner.match(
    /role "([^"]+)" cannot be dropped because some objects depend on it/i,
  );
  if (dependsOn) {
    return `Role "${dependsOn[1]}" still owns objects in another database. Drop or reassign them first.`;
  }

  // permission denied for database / schema
  if (/permission denied/i.test(inner)) {
    return `Permission denied — the postgres superuser may not have access to this object.`;
  }

  // pgvector package not installed on the server yet — CREATE EXTENSION can't
  // find vector.control. Point the operator at the install card.
  if (
    /could not open extension control file/i.test(inner) ||
    /extension "vector" is not available/i.test(inner)
  ) {
    return 'The pgvector package is not installed on this server yet. Install the "pgvector" card from the strip above first.';
  }

  // DROP EXTENSION blocked because tables/indexes still use vector columns.
  if (/cannot drop extension .* because other objects depend on it/i.test(inner)) {
    return "Can't disable pgvector — tables still use vector columns. Drop those columns first, then disable.";
  }

  void op;
  return inner;
}

function rawErrorMessage(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export async function createPgRoleAction(
  _prev: ActionResult<{ password: string; user: string }> | null,
  fd: FormData,
): Promise<ActionResult<{ password: string; user: string }>> {
  const s = await requireAdmin();
  const user = String(fd.get("user") ?? "").trim();
  const password = String(fd.get("password") ?? "").trim() || undefined;
  const superuser = String(fd.get("superuser") ?? "") === "on";
  try {
    const resp = await agent.postgres.createRole({ user, password, superuser });
    writeAudit({
      actorId: s.userId,
      action: "pg.users.create",
      target: user,
      result: "ok",
      message: superuser ? "superuser" : undefined,
    });
    revalidatePath("/databases");
    return {
      ok: true,
      data: { password: resp.initial_password, user: resp.user },
    };
  } catch (e: unknown) {
    const raw = rawErrorMessage(e);
    writeAudit({
      actorId: s.userId,
      action: "pg.users.create",
      target: user,
      result: "error",
      message: raw,
    });
    return { ok: false, error: friendlyPgError("create_role", raw) };
  }
}

export async function createPgDatabaseAction(
  _prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
  const s = await requireAdmin();
  const name = String(fd.get("name") ?? "").trim();
  const owner = String(fd.get("owner") ?? "").trim() || undefined;
  try {
    await agent.postgres.createDatabase({ name, owner });
    writeAudit({
      actorId: s.userId,
      action: "pg.databases.create",
      target: name,
      result: "ok",
      message: owner ? `owner=${owner}` : undefined,
    });
    revalidatePath("/databases");
    return { ok: true };
  } catch (e: unknown) {
    const raw = rawErrorMessage(e);
    writeAudit({
      actorId: s.userId,
      action: "pg.databases.create",
      target: name,
      result: "error",
      message: raw,
    });
    return { ok: false, error: friendlyPgError("create_database", raw) };
  }
}

export async function updatePgDatabaseAction(
  currentName: string,
  changes: { newName?: string; owner?: string },
): Promise<ActionResult> {
  const s = await requireAdmin();
  const newName = changes.newName?.trim() || undefined;
  const owner = changes.owner?.trim() || undefined;
  try {
    await agent.postgres.updateDatabase(currentName, {
      new_name: newName,
      owner,
    });
    writeAudit({
      actorId: s.userId,
      action: "pg.databases.update",
      target: currentName,
      result: "ok",
      message: [
        newName && newName !== currentName ? `rename=${newName}` : null,
        owner ? `owner=${owner}` : null,
      ]
        .filter(Boolean)
        .join(" ") || undefined,
    });
    revalidatePath("/databases");
    return { ok: true };
  } catch (e: unknown) {
    const raw = rawErrorMessage(e);
    writeAudit({
      actorId: s.userId,
      action: "pg.databases.update",
      target: currentName,
      result: "error",
      message: raw,
    });
    return { ok: false, error: friendlyPgError("update_database", raw) };
  }
}

export async function pgGrantAction(
  _prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
  const s = await requireAdmin();
  const user = String(fd.get("user") ?? "").trim();
  const database = String(fd.get("database") ?? "").trim();
  const privileges = String(fd.get("privileges") ?? "ALL PRIVILEGES");
  try {
    await agent.postgres.grant({ user, database, privileges });
    writeAudit({
      actorId: s.userId,
      action: "pg.grant",
      target: `${user}->${database}`,
      result: "ok",
      message: privileges,
    });
    revalidatePath("/databases");
    return { ok: true };
  } catch (e: unknown) {
    const raw = rawErrorMessage(e);
    writeAudit({
      actorId: s.userId,
      action: "pg.grant",
      target: `${user}->${database}`,
      result: "error",
      message: raw,
    });
    return { ok: false, error: friendlyPgError("grant", raw) };
  }
}

export async function deletePgRoleAction(user: string): Promise<void> {
  const s = await requireAdmin();
  try {
    await agent.postgres.deleteRole(user);
    writeAudit({
      actorId: s.userId,
      action: "pg.users.delete",
      target: user,
      result: "ok",
    });
  } catch (e: unknown) {
    const raw = rawErrorMessage(e);
    writeAudit({
      actorId: s.userId,
      action: "pg.users.delete",
      target: user,
      result: "error",
      message: raw,
    });
    throw new Error(friendlyPgError("drop_role", raw));
  }
  revalidatePath("/databases");
}

export async function deletePgDatabaseAction(name: string): Promise<void> {
  const s = await requireAdmin();
  try {
    await agent.postgres.deleteDatabase(name);
    writeAudit({
      actorId: s.userId,
      action: "pg.databases.delete",
      target: name,
      result: "ok",
    });
  } catch (e: unknown) {
    const raw = rawErrorMessage(e);
    writeAudit({
      actorId: s.userId,
      action: "pg.databases.delete",
      target: name,
      result: "error",
      message: raw,
    });
    throw new Error(friendlyPgError("drop_database", raw));
  }
  revalidatePath("/databases");
}

export async function enablePgvectorAction(
  database: string,
): Promise<ActionResult> {
  const s = await requireAdmin();
  const name = database.trim();
  try {
    await agent.postgres.enablePgvector(name);
    writeAudit({
      actorId: s.userId,
      action: "pg.pgvector.enable",
      target: name,
      result: "ok",
    });
    revalidatePath("/databases");
    return { ok: true };
  } catch (e: unknown) {
    const raw = rawErrorMessage(e);
    writeAudit({
      actorId: s.userId,
      action: "pg.pgvector.enable",
      target: name,
      result: "error",
      message: raw,
    });
    return { ok: false, error: friendlyPgError("enable_pgvector", raw) };
  }
}

export async function disablePgvectorAction(
  database: string,
): Promise<ActionResult> {
  const s = await requireAdmin();
  const name = database.trim();
  try {
    await agent.postgres.disablePgvector(name);
    writeAudit({
      actorId: s.userId,
      action: "pg.pgvector.disable",
      target: name,
      result: "ok",
    });
    revalidatePath("/databases");
    return { ok: true };
  } catch (e: unknown) {
    const raw = rawErrorMessage(e);
    writeAudit({
      actorId: s.userId,
      action: "pg.pgvector.disable",
      target: name,
      result: "error",
      message: raw,
    });
    return { ok: false, error: friendlyPgError("disable_pgvector", raw) };
  }
}

export async function setPgRolePasswordAction(
  user: string,
  password: string,
): Promise<ActionResult<{ password: string }>> {
  const s = await requireAdmin();
  try {
    const resp = await agent.postgres.setRolePassword(user, {
      password: password || undefined,
    });
    writeAudit({
      actorId: s.userId,
      action: "pg.users.password",
      target: user,
      result: "ok",
    });
    return { ok: true, data: { password: resp.password } };
  } catch (e: unknown) {
    const raw = rawErrorMessage(e);
    writeAudit({
      actorId: s.userId,
      action: "pg.users.password",
      target: user,
      result: "error",
      message: raw,
    });
    return { ok: false, error: friendlyPgError("set_password", raw) };
  }
}
