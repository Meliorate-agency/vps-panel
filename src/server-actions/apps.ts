"use server";

import { revalidatePath } from "next/cache";
import { redirect } from "next/navigation";
import { agent, type CreateApp, type AppEnvFilesResp, type GitStatusResp } from "@/lib/agent";
import { requireAdmin, requireSession } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";
import { db } from "@/lib/db";
import { getSetting, setSetting } from "@/lib/settings";
import { applyAppRouting } from "@/lib/appRouting";
import { recordAppDeploy, slotConflict } from "@/lib/appRows";

export type ActionResult<T = undefined> =
  | { ok: true; data?: T }
  | { ok: false; error: string };

/** What a domain change did, for the /apps/<name> Domain form. */
export type RoutingOutcome = {
  state: "assigned" | "unassigned";
  /** Slot the app is on now, or - after an unassign - the one that went dark. */
  domain: string;
  port: number;
  /** Assign only: the app came up on the new port within the wait window. */
  bound: boolean;
  /** journalctl tail when `bound` is false. */
  log: string[];
};

/**
 * When true (the default), /apps runs a `git fetch` for every panel-managed
 * app on each page load to populate the Git column. That's N network round
 * trips per render, so an operator with many apps can turn it off and check
 * individual apps on demand via the per-row Check button instead.
 */
const AUTO_CHECK_KEY = "apps.auto_check_updates";

export async function getAutoCheckUpdates(): Promise<boolean> {
  await requireSession();
  return getSetting(AUTO_CHECK_KEY, "true") !== "false";
}

export async function setAutoCheckUpdatesAction(
  enabled: boolean,
): Promise<void> {
  const s = await requireAdmin();
  setSetting(AUTO_CHECK_KEY, enabled ? "true" : "false");
  writeAudit({
    actorId: s.userId,
    action: "apps.auto_check_updates",
    target: "/apps",
    result: "ok",
    message: enabled ? "enabled" : "disabled",
  });
  revalidatePath("/apps");
}

/**
 * Outcome of an on-demand update check. `updating` means the app was behind
 * and an update task was already spawned — the caller navigates to
 * /tasks/<id> to watch it. Every other kind is terminal information for the
 * operator; nothing was changed on the server.
 */
export type CheckUpdateResult =
  | { ok: true; kind: "updating"; taskId: string; behind: number }
  | { ok: true; kind: "up_to_date" }
  | { ok: true; kind: "no_git" }
  | { ok: true; kind: "ahead"; ahead: number }
  | { ok: true; kind: "diverged"; ahead: number; behind: number }
  | { ok: false; error: string };

/**
 * One-click "is there an update, and if so ship it" for a single app.
 * Runs the same `git fetch` comparison the page-load path does, then —
 * only when the app is strictly behind — kicks off the normal update task
 * (pull + reinstall + build + restart). Diverged history is reported rather
 * than updated, because `git pull --ff-only` would refuse anyway.
 */
export async function checkAppUpdateAction(
  name: string,
): Promise<CheckUpdateResult> {
  await requireAdmin();
  let status;
  try {
    status = await agent.apps.gitStatus(name);
  } catch (e: unknown) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }

  if (!status.has_repo) return { ok: true, kind: "no_git" };
  if (status.error) return { ok: false, error: status.error };

  const behind = status.behind ?? 0;
  const ahead = status.ahead ?? 0;
  if (behind > 0 && ahead > 0) return { ok: true, kind: "diverged", ahead, behind };
  if (behind === 0 && ahead > 0) return { ok: true, kind: "ahead", ahead };
  if (behind === 0) return { ok: true, kind: "up_to_date" };

  // Behind with a clean fast-forward path — run the update now. Reuses
  // updateAppAction so the audit row and error shape stay identical to the
  // Update button's.
  const r = await updateAppAction(name);
  if (!r.ok) return { ok: false, error: r.error };
  return { ok: true, kind: "updating", taskId: r.taskId, behind };
}

/**
 * Run one app's `git fetch` comparison again (Retry fetch in the Git
 * column's popover after a failed check). Only reports; nothing is pulled.
 */
export async function refreshGitStatusAction(
  name: string,
): Promise<{ ok: true; status: GitStatusResp } | { ok: false; error: string }> {
  await requireAdmin();
  try {
    return { ok: true, status: await agent.apps.gitStatus(name) };
  } catch (e: unknown) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }
}

export async function deployAppAction(
  _prev: ActionResult<{ taskId: string }> | null,
  fd: FormData,
): Promise<ActionResult<{ taskId: string }>> {
  const s = await requireAdmin();
  // Only a form that has an env field replaces the env file. The deploy form
  // has none, and sending [] made the agent empty /etc/vps-panel/apps/<name>.env
  // (and the build's variables) on every deploy; undefined keeps the file.
  const env = fd.has("env") ? parseEnv(String(fd.get("env") ?? "")) : undefined;
  const repoUrlRaw = String(fd.get("repo_url") ?? "").trim();
  // Step comes from the clicked submit button's value (name="step").
  // Validate against the allowed set to keep junk out of the agent body.
  const stepRaw = String(fd.get("step") ?? "all");
  const step: CreateApp["step"] = (
    ["pull", "install", "build", "deploy", "all"] as const
  ).includes(stepRaw as never)
    ? (stepRaw as CreateApp["step"])
    : "all";
  const req: CreateApp = {
    name: String(fd.get("name") ?? "").trim(),
    source_dir: String(fd.get("source_dir") ?? "").trim(),
    port: Number(fd.get("port") ?? 0),
    subdomain: String(fd.get("subdomain") ?? "").trim().toLowerCase(),
    runtime: String(fd.get("runtime") ?? "node"),
    exec_start: String(fd.get("exec_start") ?? "/usr/bin/npm start"),
    env,
    issue_ssl: fd.get("issue_ssl") === "on",
    skip_dns_check: fd.get("skip_dns_check") === "on",
    repo_url: repoUrlRaw || undefined,
    step,
  };
  // Only the deploy stage (or the full pipeline) persists the app row to
  // panel.db — pull/install/build are "preview" stages that don't
  // commit the operator to anything visible in /apps yet.
  const persistsRow = step === "deploy" || step === "all";
  const auditAction = `apps.${step}`;
  if (persistsRow) {
    const conflict = slotConflict(req.name, req.port, req.subdomain);
    if (conflict) return { ok: false, error: conflict };
  }
  try {
    const accepted = await agent.apps.create(req);
    if (persistsRow) recordAppDeploy(req, accepted.task_id);
    writeAudit({
      actorId: s.userId,
      action: auditAction,
      target: req.name,
      request: req,
      result: "ok",
    });
    return { ok: true, data: { taskId: accepted.task_id } };
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({ actorId: s.userId, action: auditAction, target: req.name, request: req, result: "error", message: msg });
    return { ok: false, error: msg };
  }
}

export async function deleteAppAction(
  name: string,
  opts?: { wipe?: boolean },
): Promise<void> {
  const s = await requireAdmin();
  const wipe = !!opts?.wipe;
  await agent.apps.delete(name, { wipe });
  db().prepare(`DELETE FROM apps WHERE name = ?`).run(name);
  writeAudit({
    actorId: s.userId,
    action: "apps.delete",
    target: name,
    result: "ok",
    message: wipe ? "wiped source directory" : "kept source directory",
  });
  revalidatePath("/apps");
  redirect("/apps");
}

/**
 * Look up the saved repo URL for an app. Read from the panel's own SQLite
 * (we wrote it on deploy via `INSERT … repo_url`) — agent doesn't need to
 * know. Returns null when the app was deployed before the column existed
 * or was added without a URL.
 *
 * Marked `async` even though the underlying SQLite read is sync because
 * "use server" modules require every export to be async (Next.js
 * Turbopack rejects sync exports at build time).
 */
export async function getAppRepoUrl(name: string): Promise<string | null> {
  await requireSession();
  const row = db()
    .prepare(`SELECT repo_url FROM apps WHERE name = ?`)
    .get(name) as { repo_url: string | null } | undefined;
  return row?.repo_url ?? null;
}

export async function updateAppRepoUrlAction(
  name: string,
  _prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
  const s = await requireAdmin();
  const url = String(fd.get("repo_url") ?? "").trim();
  // Empty → store null (operator wants to disconnect). Non-empty → keep as-is.
  const value = url.length > 0 ? url : null;
  db()
    .prepare(`UPDATE apps SET repo_url = ? WHERE name = ?`)
    .run(value, name);
  writeAudit({
    actorId: s.userId,
    action: "apps.repo_url.update",
    target: name,
    result: "ok",
    message: value ?? "(cleared)",
  });
  revalidatePath(`/apps/${encodeURIComponent(name)}`);
  revalidatePath("/apps");
  return { ok: true };
}

/** Current ExecStart for the app, from panel.db. Falls back to the deploy-form
 *  default so the editor always has a sensible starting value. */
export async function getAppEntrypoint(name: string): Promise<string> {
  await requireSession();
  const row = db()
    .prepare(`SELECT entrypoint FROM apps WHERE name = ?`)
    .get(name) as { entrypoint: string | null } | undefined;
  return row?.entrypoint ?? "/usr/bin/npm start";
}

/**
 * Rewrite the app's systemd ExecStart (via the agent) and restart it, then
 * persist the new value to panel.db so the record matches what's running.
 */
export async function updateAppEntrypointAction(
  name: string,
  _prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
  const s = await requireAdmin();
  const execStart = String(fd.get("exec_start") ?? "").trim();
  if (!execStart) {
    return { ok: false, error: "Exec start must not be empty." };
  }
  try {
    await agent.apps.setEntrypoint(name, execStart);
    db()
      .prepare(`UPDATE apps SET entrypoint = ? WHERE name = ?`)
      .run(execStart, name);
    writeAudit({
      actorId: s.userId,
      action: "apps.entrypoint.update",
      target: name,
      result: "ok",
      message: execStart,
    });
    revalidatePath(`/apps/${encodeURIComponent(name)}`);
    return { ok: true };
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "apps.entrypoint.update",
      target: name,
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
}

/**
 * systemd lifecycle actions. Return an ActionResult instead of throwing so
 * <AppServiceControls> can show the real agent error inline — a thrown
 * server-action error gets masked by Next in production. Revalidate on both
 * paths: even a failed start/stop can leave the unit in a new state
 * (e.g. "failed") that the status badge should reflect.
 */
async function appLifecycleAction(
  name: string,
  verb: "start" | "stop" | "restart",
): Promise<ActionResult> {
  const s = await requireAdmin();
  // The agent refuses too (409); answering here keeps the audit log free of
  // a failed attempt for something the UI already greys out.
  if (verb !== "stop" && isUnassigned(name)) {
    return {
      ok: false,
      error: "No domain assigned - pick a routing slot in the Domain section first.",
    };
  }
  try {
    await agent.apps[verb](name);
    writeAudit({ actorId: s.userId, action: `apps.${verb}`, target: name, result: "ok" });
    revalidatePath(`/apps/${encodeURIComponent(name)}`);
    return { ok: true };
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({ actorId: s.userId, action: `apps.${verb}`, target: name, result: "error", message: msg });
    revalidatePath(`/apps/${encodeURIComponent(name)}`);
    return { ok: false, error: msg };
  }
}

export async function startAppAction(name: string): Promise<ActionResult> {
  return appLifecycleAction(name, "start");
}

export async function stopAppAction(name: string): Promise<ActionResult> {
  return appLifecycleAction(name, "stop");
}

/** port=0 is the unassigned marker written by applyAppRouting. */
function isUnassigned(name: string): boolean {
  const row = db()
    .prepare(`SELECT port FROM apps WHERE name = ?`)
    .get(name) as { port: number } | undefined;
  return row !== undefined && row.port === 0;
}

/**
 * Domain form on /apps/[name]. `slot` is "<domain>|<port>" or "" for
 * "No domain". The port is re-read from the sites row, never trusted.
 */
export async function setAppDomainAction(
  name: string,
  _prev: ActionResult<RoutingOutcome> | null,
  fd: FormData,
): Promise<ActionResult<RoutingOutcome>> {
  const s = await requireAdmin();
  const raw = String(fd.get("slot") ?? "").trim();
  const domain = raw === "" ? null : raw.split("|")[0].trim().toLowerCase();
  try {
    const outcome = await applyAppRouting(s.userId, name, domain);
    revalidatePath("/apps");
    revalidatePath(`/apps/${encodeURIComponent(name)}`);
    revalidatePath("/domains");
    return { ok: true, data: outcome };
  } catch (e: unknown) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }
}

export async function restartAppAction(name: string): Promise<ActionResult> {
  return appLifecycleAction(name, "restart");
}

/**
 * One-click "pull latest + reinstall + restart" for a panel-managed app.
 * Wraps agent.apps.update which spawns a task on the agent side. Returns
 * the task id so the caller can redirect to /tasks/<id> and stream the
 * live log. Errors are surfaced via the discriminated result so the popover
 * can render an inline error without throwing on the client.
 */
export async function updateAppAction(
  name: string,
): Promise<{ ok: true; taskId: string } | { ok: false; error: string }> {
  const s = await requireAdmin();
  try {
    const accepted = await agent.apps.update(name);
    writeAudit({
      actorId: s.userId,
      action: "apps.update",
      target: name,
      result: "ok",
    });
    return { ok: true, taskId: accepted.task_id };
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "apps.update",
      target: name,
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
}

// External apps (drift-detected from /srv/apps/) have a systemd unit named
// "<name>.service" — no vps-app- prefix — so the panel-managed restart
// endpoint can't reach them. Route through the generic systemd action
// endpoint instead, which accepts any unit name with the start|stop|restart
// whitelist.
export async function restartExternalAppAction(name: string): Promise<void> {
  const s = await requireAdmin();
  await agent.services.action(`${name}.service`, "restart");
  writeAudit({ actorId: s.userId, action: "apps.restart_external", target: name, result: "ok" });
  revalidatePath("/apps");
}

/**
 * List every `.env*` file inside the app's source directory (the files the
 * app's own framework loads, e.g. Next.js `.env.local`) with their content
 * and full path. Used by the EnvFilesEditor on the app detail + deploy pages.
 */
export async function listAppEnvFilesAction(
  name: string,
): Promise<{ ok: true; data: AppEnvFilesResp } | { ok: false; error: string }> {
  await requireAdmin();
  try {
    const data = await agent.apps.listEnvFiles(name);
    return { ok: true, data };
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    return { ok: false, error: msg };
  }
}

/**
 * Create or overwrite a `.env*` file inside the app's source directory, then
 * optionally restart the app. `path` is relative to the source dir; the agent
 * locks it to a `.env*` path that can't escape the folder. The audit row
 * records the path + byte count only, never the secret values.
 */
export async function saveAppEnvFileAction(
  name: string,
  path: string,
  content: string,
  restart: boolean,
): Promise<
  { ok: true; restarted: string | null } | { ok: false; error: string }
> {
  const s = await requireAdmin();
  try {
    const r = await agent.apps.saveEnvFile(name, { path, content, restart });
    writeAudit({
      actorId: s.userId,
      action: "apps.save_env_file",
      target: name,
      request: { path, restart, bytes: content.length },
      result: "ok",
      message: r.restarted ? `restarted ${r.restarted}` : "saved (no restart)",
    });
    revalidatePath(`/apps/${encodeURIComponent(name)}`);
    return { ok: true, restarted: r.restarted };
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "apps.save_env_file",
      target: name,
      request: { path, restart },
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
}

/**
 * Delete a `.env*` file from the app's source directory (via the agent, which
 * locks `path` to a `.env*` path that can't escape the folder). The audit row
 * records the path only, never file contents.
 */
export async function deleteAppEnvFileAction(
  name: string,
  path: string,
  restart: boolean,
): Promise<
  { ok: true; restarted: string | null } | { ok: false; error: string }
> {
  const s = await requireAdmin();
  try {
    const r = await agent.apps.deleteEnvFile(name, { path, restart });
    writeAudit({
      actorId: s.userId,
      action: "apps.delete_env_file",
      target: name,
      request: { path, restart },
      result: "ok",
      message: r.restarted ? `deleted; restarted ${r.restarted}` : "deleted",
    });
    revalidatePath(`/apps/${encodeURIComponent(name)}`);
    return { ok: true, restarted: r.restarted };
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "apps.delete_env_file",
      target: name,
      request: { path, restart },
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
}

/**
 * Probe whether `/srv/apps/<name>/` exists and has any entries. Used by the
 * deploy form to disable the Deploy button until the operator has pulled
 * (or otherwise populated) the source folder — Deploy would otherwise
 * blindly write a systemd unit pointing at an empty folder.
 *
 * Returns the classification verbatim from the agent ("missing" / "empty"
 * / "populated") with a quiet null-fallback on transport errors so the
 * client can treat any failure as "unknown" without throwing.
 */
export async function checkAppSourceStateAction(
  name: string,
): Promise<"missing" | "empty" | "populated" | null> {
  await requireAdmin();
  // Same regex the agent enforces; reject early so we don't paint a noisy
  // 400 in the network log while the operator is mid-keystroke.
  if (!/^[a-z0-9][a-z0-9-]{1,30}[a-z0-9]$/.test(name)) return null;
  try {
    const r = await agent.apps.sourceState(name);
    return r.state;
  } catch {
    return null;
  }
}

function parseEnv(text: string): { key: string; value: string }[] {
  return text
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter((l) => l && !l.startsWith("#"))
    .map((l) => {
      const i = l.indexOf("=");
      if (i === -1) return { key: l, value: "" };
      return { key: l.slice(0, i).trim(), value: l.slice(i + 1) };
    })
    .filter((kv) => /^[A-Za-z_][A-Za-z0-9_]*$/.test(kv.key));
}
