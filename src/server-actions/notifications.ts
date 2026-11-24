"use server";

import { revalidatePath } from "next/cache";
import { agent, agentRequest } from "@/lib/agent";
import { db } from "@/lib/db";
import { requireAdmin } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";
import { NOTIFICATION_EVENTS } from "@/lib/notificationEvents";
import { setSetting } from "@/lib/settings";
import { agentErrorMessage } from "@/lib/redirects";

export type ActionResult = { ok: true } | { ok: false; error: string };

const EMAIL_RE = /^[^\s@,<>]+@[^\s@,<>]+\.[^\s@,<>]+$/;
/** A numeric Telegram user ID (negative for a group). */
const CHAT_ID_RE = /^-?\d{1,20}$/;
/** `<digits>:<key>`, the form @BotFather hands out. */
const BOT_TOKEN_RE = /^\d+:[A-Za-z0-9_-]{20,}$/;

function validTarget(channel: string, target: string): string | null {
  if (channel === "email") return EMAIL_RE.test(target) ? null : "not a valid email address";
  if (channel === "telegram") {
    return CHAT_ID_RE.test(target) ? null : "the Telegram user ID is a number, like 123456789";
  }
  try {
    const u = new URL(target);
    return u.protocol === "https:" || u.protocol === "http:" ? null : "webhook must be an http(s) URL";
  } catch {
    return "webhook must be an http(s) URL";
  }
}

export async function addNotificationAction(
  _prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
  const s = await requireAdmin();
  const channel = String(fd.get("channel") ?? "");
  const target = String(fd.get("target") ?? "").trim();
  const known = new Set<string>(NOTIFICATION_EVENTS.map((e) => e.id));
  const events = fd
    .getAll("events")
    .map(String)
    .filter((e) => known.has(e))
    .join(",");
  if (!["email", "webhook", "telegram"].includes(channel)) {
    return { ok: false, error: "channel must be email, webhook or telegram" };
  }
  if (!target) return { ok: false, error: "target required" };
  const bad = validTarget(channel, target);
  if (bad) return { ok: false, error: bad };
  let secret: string | null = null;
  if (channel === "telegram") {
    secret = String(fd.get("secret") ?? "").trim();
    if (!BOT_TOKEN_RE.test(secret)) {
      return { ok: false, error: "the bot token looks like 123456789:AAH... (from @BotFather)" };
    }
  }
  // Email is sent from this server, which keeps the logs anyway, and always
  // has them. A webhook or a Telegram chat is somewhere else: log lines (they
  // can hold email and IP addresses) go there only when asked for.
  const logLines = channel === "email" || fd.get("log_lines") === "on" ? 1 : 0;
  db()
    .prepare(`INSERT INTO notifications (channel, target, events, secret, log_lines) VALUES (?, ?, ?, ?, ?)`)
    .run(channel, target, events, secret, logLines);
  writeAudit({ actorId: s.userId, action: "notifications.add", target: `${channel}:${target}`, result: "ok" });
  revalidatePath("/notifications");
  return { ok: true };
}

export async function deleteNotificationAction(id: number): Promise<void> {
  const s = await requireAdmin();
  db().prepare(`DELETE FROM notifications WHERE id = ?`).run(id);
  writeAudit({ actorId: s.userId, action: "notifications.delete", target: String(id), result: "ok" });
  revalidatePath("/notifications");
}

export async function testNotificationAction(id: number): Promise<ActionResult> {
  const s = await requireAdmin();
  const row = db()
    .prepare(`SELECT channel, target, secret FROM notifications WHERE id = ?`)
    .get(id) as { channel: string; target: string; secret: string | null } | undefined;
  if (!row) return { ok: false, error: "channel not found" };
  try {
    // With the id the agent reads the channel from panel.db as a real event
    // does (its log line choice included) and records the result on it.
    await agentRequest({
      method: "POST",
      path: "/notifications/test",
      body: { id, channel: row.channel, target: row.target, secret: row.secret },
    });
    writeAudit({ actorId: s.userId, action: "notifications.test", target: `${row.channel}:${row.target}`, result: "ok" });
    return { ok: true };
  } catch (e: unknown) {
    const msg = agentErrorMessage(e).replace(/^invalid input: /, "");
    writeAudit({
      actorId: s.userId,
      action: "notifications.test",
      target: `${row.channel}:${row.target}`,
      result: "error",
      message: msg,
    });
    // A bot can only message someone who has started a chat with it.
    const hint = /chat not found/i.test(msg) ? ". Open the bot in Telegram, press Start, then test again." : "";
    return { ok: false, error: msg + hint };
  } finally {
    // The agent wrote the result on the row: the page shows it.
    revalidatePath("/notifications");
  }
}

/** Whether a webhook or Telegram channel gets the log lines of a failure. */
export async function setLogLinesAction(id: number, on: boolean): Promise<void> {
  const s = await requireAdmin();
  const row = db().prepare(`SELECT channel, target FROM notifications WHERE id = ?`).get(id) as
    | { channel: string; target: string }
    | undefined;
  if (!row || row.channel === "email") return;
  db().prepare(`UPDATE notifications SET log_lines = ? WHERE id = ?`).run(on ? 1 : 0, id);
  writeAudit({
    actorId: s.userId,
    action: "notifications.log_lines",
    target: `${row.channel}:${row.target}`,
    result: "ok",
    message: on ? "on" : "off",
  });
  revalidatePath("/notifications");
}

/**
 * `notifications.email_from`: the mailbox notification email is sent as.
 * Only an existing mailbox is accepted, so its domain's SPF and DKIM cover
 * the message and bounces have somewhere to land. (Unset, the agent falls
 * back to Postfix's root@<server hostname>, but that is never offered.)
 */
export async function setEmailFromAction(
  _prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
  const s = await requireAdmin();
  const from = String(fd.get("email_from") ?? "").trim().toLowerCase();
  if (!from) return { ok: false, error: "pick a mailbox" };
  const list = await agent.mail.listMailboxes().catch(() => null);
  if (!list) return { ok: false, error: "could not read the mailbox list from the agent" };
  if (!list.mailboxes.some((m) => m.email.toLowerCase() === from)) {
    return { ok: false, error: `${from} is not a mailbox on this server` };
  }
  setSetting("notifications.email_from", from);
  writeAudit({ actorId: s.userId, action: "notifications.email_from", target: from, result: "ok" });
  revalidatePath("/notifications");
  return { ok: true };
}
