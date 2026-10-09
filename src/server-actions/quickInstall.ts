"use server";

import { revalidatePath } from "next/cache";
import {
  agent,
  type N8nSettings,
  type PhpMyAdminSettings,
  type RoundcubeSettings,
} from "@/lib/agent";
import { requireAdmin, requireSession } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";
import { getSetting, setSetting } from "@/lib/settings";

export type QuickInstallResult =
  | { ok: true; data: { taskId: string; slug: string } }
  | { ok: false; error: string };

export type SetBindResult =
  | { ok: true; data: { taskId: string; slug: string; bind: string } }
  | { ok: false; error: string };

export type GetRoundcubeSettingsResult =
  | { ok: true; data: RoundcubeSettings }
  | { ok: false; error: string };

export type UpdateRoundcubeSettingsResult =
  | { ok: true; data: { taskId: string } }
  | { ok: false; error: string };

export type GetN8nSettingsResult =
  | { ok: true; data: N8nSettings }
  | { ok: false; error: string };

export type UpdateN8nSettingsResult =
  | { ok: true; data: { taskId: string } }
  | { ok: false; error: string };

export type UpdateN8nResult =
  | { ok: true; data: { taskId: string } }
  | { ok: false; error: string };

export type GetPhpMyAdminSettingsResult =
  | { ok: true; data: PhpMyAdminSettings }
  | { ok: false; error: string };

export type UpdatePhpMyAdminSettingsResult =
  | { ok: true; data: { taskId: string } }
  | { ok: false; error: string };

/**
 * Per-recipe page-load update probes. The apps-wide toggle
 * (apps.auto_check_updates) only covers panel-managed git apps; each
 * quick-install app with a registry probe gets its own key so the operator
 * pays for exactly the checks they want. Only n8n has a probe today - when
 * another recipe grows one, give it its own key next to this one.
 */
const N8N_AUTO_CHECK_KEY = "quickinstall.n8n.auto_check_updates";

/** Whether /apps asks Docker Hub for a newer n8nio/n8n image on each load.
 *  Until the operator flips n8n's own switch (no row yet), inherits the
 *  apps-wide toggle (`apps.auto_check_updates`, see server-actions/apps.ts):
 *  before the per-app split that toggle gated this probe too, so an operator
 *  who had opted out of page-load checks must stay opted out - not get the
 *  6s-cap registry call silently re-enabled by a deploy. */
export async function getN8nAutoCheck(): Promise<boolean> {
  await requireSession();
  const own = getSetting(N8N_AUTO_CHECK_KEY, "");
  if (own === "") {
    return getSetting("apps.auto_check_updates", "true") !== "false";
  }
  return own !== "false";
}

export async function setN8nAutoCheckAction(enabled: boolean): Promise<void> {
  const s = await requireAdmin();
  setSetting(N8N_AUTO_CHECK_KEY, enabled ? "true" : "false");
  writeAudit({
    actorId: s.userId,
    action: "quick-install.n8n_auto_check",
    target: "n8n",
    result: "ok",
    message: enabled ? "enabled" : "disabled",
  });
  revalidatePath("/apps");
}

export async function quickInstallAction(
  _prev: QuickInstallResult | null,
  fd: FormData,
): Promise<QuickInstallResult> {
  const s = await requireAdmin();
  const slug = String(fd.get("slug") ?? "").trim();
  if (!slug) {
    return { ok: false, error: "missing slug" };
  }
  try {
    const accepted = await agent.quickInstall.run(slug);
    writeAudit({
      actorId: s.userId,
      action: "quick-install.run",
      target: slug,
      result: "ok",
    });
    return { ok: true, data: { taskId: accepted.task_id, slug } };
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "quick-install.run",
      target: slug,
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
}

/**
 * Read the current Roundcube container env (mail host, IMAP/SMTP ports) so
 * the panel UI can show them as editable fields. Wraps as a result so the
 * client can surface "Roundcube isn't installed yet" gracefully instead of
 * throwing.
 */
export async function getRoundcubeSettingsAction(): Promise<GetRoundcubeSettingsResult> {
  await requireSession();
  try {
    const data = await agent.quickInstall.getRoundcubeSettings();
    return { ok: true, data };
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    return { ok: false, error: msg };
  }
}

/**
 * Apply edited Roundcube settings — tears down + recreates the container
 * with the new IMAP/SMTP target. The agent task wipes the cached
 * config.inc.php in the roundcube_config volume so the new values
 * actually take effect (the docker image's entrypoint only regenerates
 * config on first run). User prefs in roundcube_db are preserved.
 */
export async function updateRoundcubeSettingsAction(
  _prev: UpdateRoundcubeSettingsResult | null,
  fd: FormData,
): Promise<UpdateRoundcubeSettingsResult> {
  const s = await requireAdmin();
  const mailHost = String(fd.get("mail_host") ?? "").trim();
  const imapPort = Number(fd.get("imap_port") ?? 993);
  const smtpPort = Number(fd.get("smtp_port") ?? 587);
  if (!mailHost) return { ok: false, error: "mail host required" };
  if (!mailHost.includes(".")) {
    return { ok: false, error: "mail host must be an FQDN" };
  }
  if (!Number.isFinite(imapPort) || imapPort < 1 || imapPort > 65535) {
    return { ok: false, error: "invalid IMAP port" };
  }
  if (!Number.isFinite(smtpPort) || smtpPort < 1 || smtpPort > 65535) {
    return { ok: false, error: "invalid SMTP port" };
  }
  try {
    const accepted = await agent.quickInstall.updateRoundcubeSettings({
      mail_host: mailHost,
      imap_port: imapPort,
      smtp_port: smtpPort,
    });
    writeAudit({
      actorId: s.userId,
      action: "quick-install.roundcube_settings",
      target: mailHost,
      result: "ok",
      message: `imap=${imapPort} smtp=${smtpPort}`,
    });
    revalidatePath("/apps");
    return { ok: true, data: { taskId: accepted.task_id } };
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "quick-install.roundcube_settings",
      target: mailHost,
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
}

/** Read current n8n container env (host / protocol / webhook_url) + the
 *  domain suggestions the agent extracted from /domains. Wrapped as a
 *  Result so the client can render "n8n not installed yet" gracefully. */
export async function getN8nSettingsAction(): Promise<GetN8nSettingsResult> {
  await requireSession();
  try {
    const data = await agent.quickInstall.getN8nSettings();
    return { ok: true, data };
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    return { ok: false, error: msg };
  }
}

/** Apply edited n8n public-URL settings. Recreates the container with
 *  the new env vars; n8n_data volume (workflows, credentials, sqlite DB)
 *  is left untouched so the operator doesn't lose anything. */
export async function updateN8nSettingsAction(
  _prev: UpdateN8nSettingsResult | null,
  fd: FormData,
): Promise<UpdateN8nSettingsResult> {
  const s = await requireAdmin();
  const host = String(fd.get("host") ?? "").trim();
  const protocolRaw = String(fd.get("protocol") ?? "").trim().toLowerCase();
  const webhookUrl = String(fd.get("webhook_url") ?? "").trim();
  if (!host) return { ok: false, error: "host required" };
  if (!host.includes(".")) return { ok: false, error: "host must be an FQDN" };
  if (protocolRaw !== "http" && protocolRaw !== "https") {
    return { ok: false, error: "protocol must be http or https" };
  }
  if (!webhookUrl.startsWith("http://") && !webhookUrl.startsWith("https://")) {
    return { ok: false, error: "webhook URL must start with http(s)://" };
  }
  try {
    const accepted = await agent.quickInstall.updateN8nSettings({
      host,
      protocol: protocolRaw,
      webhook_url: webhookUrl,
    });
    writeAudit({
      actorId: s.userId,
      action: "quick-install.n8n_settings",
      target: host,
      result: "ok",
      message: `protocol=${protocolRaw} webhook=${webhookUrl}`,
    });
    revalidatePath("/apps");
    return { ok: true, data: { taskId: accepted.task_id } };
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "quick-install.n8n_settings",
      target: host,
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
}

/** Pull the latest n8nio/n8n image and recreate the container on it.
 *  Preserves the operator's public-URL env + port bind + the n8n_data
 *  volume (workflows, credentials, sqlite DB). Returns a task id so the
 *  card can stream the docker update log. Shaped for useActionState so the
 *  N8nUpdateButton can dispatch it like the other card actions. */
export async function updateN8nAction(
  _prev: UpdateN8nResult | null,
  _fd: FormData,
): Promise<UpdateN8nResult> {
  const s = await requireAdmin();
  try {
    const accepted = await agent.quickInstall.updateN8n();
    writeAudit({
      actorId: s.userId,
      action: "quick-install.n8n_update",
      target: "n8n",
      result: "ok",
    });
    revalidatePath("/apps");
    return { ok: true, data: { taskId: accepted.task_id } };
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "quick-install.n8n_update",
      target: "n8n",
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
}

/** Read the phpMyAdmin container's current PMA_ABSOLUTE_URI + suggested
 *  domains from /domains (port-mode sites mapped to port 8081). Wrapped
 *  as a Result so the UI can render "phpMyAdmin not installed yet"
 *  gracefully instead of throwing. */
export async function getPhpMyAdminSettingsAction(): Promise<GetPhpMyAdminSettingsResult> {
  await requireSession();
  try {
    const data = await agent.quickInstall.getPhpMyAdminSettings();
    return { ok: true, data };
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    return { ok: false, error: msg };
  }
}

/** Apply edited phpMyAdmin PMA_ABSOLUTE_URI — recreates the container
 *  with the new env so the cookie-secure detection picks it up.
 *  Preserves the current bind. No persistent state is touched (phpMyAdmin
 *  has no data volume in this recipe — every login is independent). */
export async function updatePhpMyAdminSettingsAction(
  _prev: UpdatePhpMyAdminSettingsResult | null,
  fd: FormData,
): Promise<UpdatePhpMyAdminSettingsResult> {
  const s = await requireAdmin();
  const absoluteUri = String(fd.get("absolute_uri") ?? "").trim();
  // Empty is allowed — means "clear the env" (revert to default). Any
  // non-empty value must look like a URL; the agent re-validates.
  if (absoluteUri.length > 0) {
    if (
      !absoluteUri.startsWith("http://") &&
      !absoluteUri.startsWith("https://")
    ) {
      return {
        ok: false,
        error: "URL must start with http:// or https://",
      };
    }
  }
  try {
    const accepted = await agent.quickInstall.updatePhpMyAdminSettings({
      absolute_uri: absoluteUri,
    });
    writeAudit({
      actorId: s.userId,
      action: "quick-install.phpmyadmin_settings",
      target: absoluteUri || "(cleared)",
      result: "ok",
    });
    revalidatePath("/databases");
    revalidatePath("/apps");
    return { ok: true, data: { taskId: accepted.task_id } };
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "quick-install.phpmyadmin_settings",
      target: absoluteUri || "(cleared)",
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
}

export async function setBindAction(
  _prev: SetBindResult | null,
  fd: FormData,
): Promise<SetBindResult> {
  const s = await requireAdmin();
  const slug = String(fd.get("slug") ?? "").trim();
  const bind = String(fd.get("bind") ?? "").trim();
  if (!slug) return { ok: false, error: "missing slug" };
  if (bind !== "127.0.0.1" && bind !== "0.0.0.0") {
    return { ok: false, error: "bind must be 127.0.0.1 or 0.0.0.0" };
  }
  try {
    const accepted = await agent.quickInstall.setBind(slug, bind);
    writeAudit({
      actorId: s.userId,
      action: "quick-install.bind",
      target: `${slug}:${bind}`,
      result: "ok",
    });
    revalidatePath("/apps");
    return { ok: true, data: { taskId: accepted.task_id, slug, bind } };
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "quick-install.bind",
      target: `${slug}:${bind}`,
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
}
