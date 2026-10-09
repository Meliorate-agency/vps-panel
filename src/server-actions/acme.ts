"use server";

import { revalidatePath } from "next/cache";
import { db } from "@/lib/db";
import { requireAdmin } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";

export type ActionResult = { ok: true } | { ok: false; error: string };

const KEY = "panel.acme_email";

function validateAcmeEmail(raw: string): string | null {
  if (raw === "") return null;
  if (/\s/.test(raw)) return "email must not contain whitespace";
  const at = raw.indexOf("@");
  if (at < 0) return "email must contain '@'";
  const local = raw.slice(0, at);
  const domain = raw.slice(at + 1);
  if (local.length === 0) return "email local part is empty";
  if (!domain.includes(".")) {
    return "email domain has no dot. Let's Encrypt rejects single-label domains";
  }
  if (raw.length > 254) return "email is too long";
  return null;
}

export async function setAcmeEmailAction(value: string): Promise<ActionResult> {
  const s = await requireAdmin();
  const trimmed = value.trim();
  const err = validateAcmeEmail(trimmed);
  if (err) {
    writeAudit({
      actorId: s.userId,
      action: "settings.set",
      target: KEY,
      result: "error",
      message: err,
    });
    return { ok: false, error: err };
  }
  db()
    .prepare(
      `INSERT INTO settings (key, value) VALUES (?, ?)
       ON CONFLICT(key) DO UPDATE SET value = excluded.value`,
    )
    .run(KEY, trimmed);
  writeAudit({ actorId: s.userId, action: "settings.set", target: KEY, result: "ok" });
  revalidatePath("/settings");
  return { ok: true };
}
