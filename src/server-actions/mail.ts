"use server";

import { revalidatePath } from "next/cache";
import { agent, AgentError } from "@/lib/agent";
import { requireAdmin } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";
import { agentErrorMessage, isSafeDomain } from "@/lib/redirects";
import { mailboxDomains } from "@/lib/mailboxDomains";
import type { SslTaskResult } from "@/lib/sslTaskResult";
import type {
  MailDomainDnsCheck,
  MailIpCheck,
  MailIpDomain,
  MailIpPreference,
  MailIpProtocol,
  MailIpSettings,
  MailTrackResp,
} from "@/lib/agent";

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
  const allowed = mailboxDomains();
  if (!allowed.includes(domain)) {
    const parent = allowed.find((d) => domain.endsWith(`.${d}`));
    return {
      ok: false,
      error: parent
        ? `${domain} is a subdomain: create the mailbox on ${parent} instead.`
        : `${domain} is not a domain on this server. Add it on the Domains page first.`,
    };
  }

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

// Returns the agent's refusal instead of throwing it: Next replaces a thrown
// message in a production build, and the dialogs show what comes back here.
export async function deleteMailboxAction(email: string): Promise<ActionResult> {
  const s = await requireAdmin();
  try {
    const resp = await agent.mail.deleteMailbox(email);
    writeAudit({
      actorId: s.userId,
      action: "mail.mailboxes.delete",
      target: email,
      result: "ok",
      message:
        [
          resp?.maildir_trash_id ? `maildir moved to trash ${resp.maildir_trash_id}` : "",
          resp?.forwarders_removed ? `${resp.forwarders_removed} forwarder(s) removed` : "",
        ]
          .filter(Boolean)
          .join("; ") || undefined,
    });
    revalidatePath("/mailboxes");
    return { ok: true };
  } catch (e: unknown) {
    const error = agentErrorMessage(e);
    writeAudit({
      actorId: s.userId,
      action: "mail.mailboxes.delete",
      target: email,
      result: "error",
      message: error,
    });
    // The login may be gone even when its mail could not be moved to the trash.
    revalidatePath("/mailboxes");
    return { ok: false, error };
  }
}

/**
 * Add a forwarder: mail to `source` (an address, or `@domain` for the domain's
 * catch-all) also goes to each destination. Destinations come one per line or
 * comma-separated.
 */
export async function createAliasAction(
  _prev: ActionResult<{ added: number }> | null,
  fd: FormData,
): Promise<ActionResult<{ added: number }>> {
  const s = await requireAdmin();
  const source = String(fd.get("source") ?? "").trim().toLowerCase();
  const destinations = String(fd.get("destinations") ?? "")
    .split(/[\s,;]+/)
    .map((d) => d.trim())
    .filter(Boolean);
  if (!source.includes("@")) return { ok: false, error: "enter the address to forward" };
  if (destinations.length === 0) return { ok: false, error: "enter an address to forward to" };
  try {
    const resp = await agent.mail.createAlias(source, destinations);
    writeAudit({
      actorId: s.userId,
      action: "mail.aliases.create",
      target: source,
      result: "ok",
      message: destinations.join(", "),
    });
    revalidatePath("/mailboxes");
    return { ok: true, data: { added: resp.added } };
  } catch (e: unknown) {
    return { ok: false, error: agentErrorMessage(e).replace(/^(conflict|invalid input): /, "") };
  }
}

// Returns the agent's refusal for the Remove dialog: thrown, its message is
// replaced by Next in a production build.
export async function deleteAliasAction(id: number, label: string): Promise<ActionResult> {
  const s = await requireAdmin();
  try {
    await agent.mail.deleteAlias(id);
  } catch (e: unknown) {
    const msg = agentErrorMessage(e);
    writeAudit({ actorId: s.userId, action: "mail.aliases.delete", target: label, result: "error", message: msg });
    return { ok: false, error: msg };
  }
  writeAudit({ actorId: s.userId, action: "mail.aliases.delete", target: label, result: "ok" });
  revalidatePath("/mailboxes");
  return { ok: true };
}

/** Track delivery: what Postfix did with mail to or from one address. */
export async function trackDeliveryAction(
  _prev: ActionResult<MailTrackResp> | null,
  fd: FormData,
): Promise<ActionResult<MailTrackResp>> {
  await requireAdmin();
  const address = String(fd.get("address") ?? "").trim().toLowerCase();
  const days = Number(fd.get("days") ?? 3);
  if (!address.includes("@")) return { ok: false, error: "enter a full email address" };
  try {
    const resp = await agent.mail.trackDelivery(address, Number.isFinite(days) ? days : 3);
    return { ok: true, data: resp };
  } catch (e: unknown) {
    return { ok: false, error: agentErrorMessage(e).replace(/^invalid input: /, "") };
  }
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

// ─── Outgoing mail over IPv4 or IPv6 ─────────────────────────────────────

/** A write the agent may refuse with 409 because the IPv6 side is not ready:
 *  `needsForce` asks the operator to confirm and send it again with force. */
export type MailIpWriteResult<T> =
  | { ok: true; data: T }
  | { ok: false; error: string; needsForce?: boolean };

export type MailIpReadResult<T> = { ok: true; data: T } | { ok: false; error: string };

const IP_PROTOCOLS: readonly string[] = ["ipv4", "ipv6"] satisfies MailIpProtocol[];
const IP_PREFERENCES: readonly string[] = ["default", "ipv4", "ipv6"] satisfies MailIpPreference[];
const PREFERENCE_TEXT: Record<MailIpPreference, string> = {
  default: "server default",
  ipv4: "IPv4 first",
  ipv6: "IPv6 first",
};

/** The agent's message without its "conflict: " style prefix. An agent
 *  older than this setting answers these routes with a bare 404. */
function mailIpError(e: unknown, domain?: string): string {
  if (e instanceof AgentError && e.status === 404) {
    if (!e.body) {
      return "The agent on this server does not have this setting yet. Update the panel, then try again.";
    }
    if (domain) return `${domain} is not a mail domain on this server.`;
  }
  return agentErrorMessage(e).replace(/^(conflict|invalid input): /, "");
}

/** Server-wide: which protocol outgoing mail tries first. `force` is only
 *  true after the operator confirmed past the agent's 409. */
export async function setMailIpDefaultAction(
  protocol: string,
  force: boolean,
): Promise<MailIpWriteResult<MailIpSettings>> {
  const s = await requireAdmin();
  if (!IP_PROTOCOLS.includes(protocol)) return { ok: false, error: "Choose IPv4 or IPv6." };
  const forced = force === true;
  try {
    const data = await agent.mail.setIpSettings({
      default_protocol: protocol as MailIpProtocol,
      ...(forced ? { force: true } : {}),
    });
    writeAudit({
      actorId: s.userId,
      action: "mail.ip.default.set",
      target: protocol,
      result: "ok",
      message: forced ? "applied although IPv6 was not ready" : undefined,
    });
    revalidatePath("/settings");
    revalidatePath("/mailboxes");
    return { ok: true, data };
  } catch (e: unknown) {
    const error = mailIpError(e);
    // Refused because IPv6 is not ready: nothing changed, the operator decides.
    if (e instanceof AgentError && e.status === 409 && !forced) {
      return { ok: false, error, needsForce: true };
    }
    writeAudit({
      actorId: s.userId,
      action: "mail.ip.default.set",
      target: protocol,
      result: "error",
      message: error,
    });
    return { ok: false, error };
  }
}

/** One mail domain: follow the server default, or a protocol of its own. */
export async function setMailDomainIpAction(
  domain: string,
  preference: string,
  force: boolean,
): Promise<MailIpWriteResult<MailIpDomain>> {
  const s = await requireAdmin();
  const name = String(domain ?? "").trim().toLowerCase();
  if (!isSafeDomain(name)) return { ok: false, error: "Not a valid domain name." };
  if (!IP_PREFERENCES.includes(preference)) {
    return { ok: false, error: "Choose the server default, IPv4 or IPv6." };
  }
  const forced = force === true;
  try {
    const data = await agent.mail.setDomainIpPreference(name, {
      preference: preference as MailIpPreference,
      ...(forced ? { force: true } : {}),
    });
    writeAudit({
      actorId: s.userId,
      action: "mail.ip.domain.set",
      target: name,
      result: "ok",
      message:
        PREFERENCE_TEXT[preference as MailIpPreference] +
        (forced ? ", applied although IPv6 was not ready" : ""),
    });
    revalidatePath("/settings");
    revalidatePath("/mailboxes");
    return { ok: true, data };
  } catch (e: unknown) {
    const error = mailIpError(e, name);
    if (e instanceof AgentError && e.status === 409 && !forced) {
      return { ok: false, error, needsForce: true };
    }
    writeAudit({
      actorId: s.userId,
      action: "mail.ip.domain.set",
      target: name,
      result: "error",
      message: error,
    });
    return { ok: false, error };
  }
}

/** Live check of both protocols (reverse DNS and so on). Slow: DNS lookups. */
export async function mailIpCheckAction(): Promise<MailIpReadResult<MailIpCheck>> {
  await requireAdmin();
  try {
    return { ok: true, data: await agent.mail.ipCheck() };
  } catch (e: unknown) {
    return { ok: false, error: mailIpError(e) };
  }
}

/** Live check of one mail domain's DNS records for both protocols. Slow. */
export async function mailDomainDnsCheckAction(
  domain: string,
): Promise<MailIpReadResult<MailDomainDnsCheck>> {
  await requireAdmin();
  const name = String(domain ?? "").trim().toLowerCase();
  if (!isSafeDomain(name)) return { ok: false, error: "Not a valid domain name." };
  try {
    return { ok: true, data: await agent.mail.domainDnsCheck(name) };
  } catch (e: unknown) {
    return { ok: false, error: mailIpError(e, name) };
  }
}
