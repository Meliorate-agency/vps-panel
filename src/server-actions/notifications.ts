"use server";

import { revalidatePath } from "next/cache";
import { agent } from "@/lib/agent";
import { db } from "@/lib/db";
import { requireAdmin } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";
import { NOTIFICATION_EVENTS } from "@/lib/notificationEvents";

export type ActionResult = { ok: true } | { ok: false; error: string };

const EMAIL_RE = /^[^\s@,<>]+@[^\s@,<>]+\.[^\s@,<>]+$/;

function validTarget(channel: string, target: string): string | null {
  if (channel === "email") return EMAIL_RE.test(target) ? null : "not a valid email address";
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
  if (!["email", "webhook"].includes(channel)) {
    return { ok: false, error: "channel must be email or webhook" };
  }
  if (!target) return { ok: false, error: "target required" };
  const bad = validTarget(channel, target);
  if (bad) return { ok: false, error: bad };
  db()
    .prepare(`INSERT INTO notifications (channel, target, events) VALUES (?, ?, ?)`)
    .run(channel, target, events);
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
    .prepare(`SELECT channel, target FROM notifications WHERE id = ?`)
    .get(id) as { channel: string; target: string } | undefined;
  if (!row) return { ok: false, error: "channel not found" };
  try {
    await agent.notifications.test(row.channel, row.target);
    writeAudit({ actorId: s.userId, action: "notifications.test", target: `${row.channel}:${row.target}`, result: "ok" });
    return { ok: true };
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "notifications.test",
      target: `${row.channel}:${row.target}`,
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
}
