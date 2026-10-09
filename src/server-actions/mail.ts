"use server";

import { revalidatePath } from "next/cache";
import { agent } from "@/lib/agent";
import { requireAdmin } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";
import { agentErrorMessage } from "@/lib/redirects";
import type { SslTaskResult } from "@/lib/sslTaskResult";

export type ActionResult<T = undefined> =
  | { ok: true; data?: T }
  | { ok: false; error: string };

export async function createMailboxAction(
  _prev: ActionResult<{ password: string }> | null,
  fd: FormData,
): Promise<ActionResult<{ password: string }>> {
  const s = await requireAdmin();
  const email = String(fd.get("email") ?? "").trim().toLowerCase();
  const password = String(fd.get("password") ?? "").trim() || undefined;
  const quota = Number(fd.get("quota_mb") ?? 1024);

  const atIdx = email.indexOf("@");
  if (atIdx <= 0 || atIdx === email.length - 1) {
    return { ok: false, error: "invalid email address" };
  }
  const domain = email.slice(atIdx + 1);

  try {
    // Auto-register the domain as a mail domain if it isn't already.
    // The agent's create_mailbox requires the domain to exist in virtual_domains.
    const existingDomains = await agent.mail.listDomains().catch(() => []);
    if (!existingDomains.some((d) => d.name === domain)) {
      await agent.mail.createDomain(domain);
      writeAudit({ actorId: s.userId, action: "mail.domains.create", target: domain, result: "ok" });
    }

    const resp = await agent.mail.createMailbox({ email, password, quota_mb: quota });
    writeAudit({ actorId: s.userId, action: "mail.mailboxes.create", target: email, result: "ok" });
    revalidatePath("/mailboxes");
    return { ok: true, data: { password: resp.initial_password } };
  } catch (e: unknown) {
    // e.g. "x@y.com already exists. Use Edit to change its password or quota."
    return { ok: false, error: agentErrorMessage(e).replace(/^(conflict|invalid input): /, "") };
  }
}

export async function updateMailboxAction(
  email: string,
  _prev: ActionResult<{ password?: string }> | null,
  fd: FormData,
): Promise<ActionResult<{ password?: string }>> {
  const s = await requireAdmin();
  const password = String(fd.get("password") ?? "").trim();
  const quotaRaw = String(fd.get("quota_mb") ?? "").trim();
  const quotaMb = quotaRaw === "" ? null : Number(quotaRaw);
  const currentQuotaRaw = String(fd.get("current_quota_mb") ?? "").trim();
  const currentQuota = currentQuotaRaw === "" ? null : Number(currentQuotaRaw);

  const wantsPassword = password.length > 0;
  // Only a quota the operator actually changed is sent (and checked against
  // usage). Both edit forms always submit the quota field, and the old browser
  // minimum of usage+1 blocked saving a new password for a full mailbox.
  const wantsQuota =
    quotaMb !== null && Number.isFinite(quotaMb) && quotaMb !== currentQuota;
  if (!wantsPassword && !wantsQuota) {
    return { ok: false, error: "nothing to change" };
  }
  if (wantsPassword && password.length < 10) {
    return { ok: false, error: "password must be >= 10 chars" };
  }
  if (wantsQuota && (quotaMb as number) <= 0) {
    return { ok: false, error: "quota must be a positive number of MB" };
  }

  try {
    if (wantsQuota) {
      const usage = await agent.mail.mailboxUsage(email);
      const floor = usage.usage_mb + 1;
      if ((quotaMb as number) < floor) {
        return {
          ok: false,
          error: `quota must be at least ${floor} MB (current usage is ${usage.usage_mb} MB)`,
        };
      }
    }

    await agent.mail.updateMailbox(email, {
      password: wantsPassword ? password : undefined,
      quota_mb: wantsQuota ? (quotaMb as number) : undefined,
    });

    const changed = [
      wantsPassword ? "password" : null,
      wantsQuota ? "quota" : null,
    ]
      .filter(Boolean)
      .join("+");
    writeAudit({
      actorId: s.userId,
      action: "mail.mailboxes.update",
      target: email,
      result: "ok",
      message: changed,
    });
    revalidatePath("/mailboxes");
    return { ok: true, data: wantsPassword ? { password } : {} };
  } catch (e: unknown) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }
}

export async function deleteMailboxAction(email: string): Promise<void> {
  const s = await requireAdmin();
  const resp = await agent.mail.deleteMailbox(email);
  writeAudit({
    actorId: s.userId,
    action: "mail.mailboxes.delete",
    target: email,
    result: "ok",
    message: resp?.maildir_trash_id ? `maildir moved to trash ${resp.maildir_trash_id}` : undefined,
  });
  revalidatePath("/mailboxes");
}

/**
 * Make sure a valid cert covers mail.<domain>. The agent either re-issues the
 * tenant's site cert with the mail SAN or gets a standalone mail.<domain>
 * cert, then rewires the Postfix/Dovecot SNI maps. Returns the task id so the
 * Domains page can follow it; taskId null means a cert already covers it.
 */
export async function ensureMailSslAction(domain: string): Promise<SslTaskResult> {
  const s = await requireAdmin();
  try {
    const resp = await agent.mail.ensureSsl(domain);
    writeAudit({
      actorId: s.userId,
      action: "mail.ssl.ensure",
      target: `mail.${domain}`,
      result: "ok",
      message: resp.task_id ? `task ${resp.task_id}` : "already covered",
    });
    revalidatePath("/domains");
    return {
      ok: true,
      taskId: resp.task_id,
      message: resp.already_covered
        ? "A valid certificate already covers this hostname."
        : undefined,
    };
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "mail.ssl.ensure",
      target: `mail.${domain}`,
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
}
