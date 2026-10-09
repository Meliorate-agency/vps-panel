"use server";

import { revalidatePath } from "next/cache";
import { db } from "@/lib/db";
import { requireAdmin } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";
import { randomUUID } from "node:crypto";
import {
  THEME_LIBRARY_SETTING_KEY,
  THEME_SETTING_KEY,
  getThemeLibrary,
} from "@/lib/accent";
import {
  MAX_SAVED_THEMES,
  normalizeThemeName,
  parseThemeConfig,
  serializeThemeConfig,
  serializeThemeLibrary,
  type SavedTheme,
} from "@/lib/theme";

export type ActionResult = { ok: true } | { ok: false; error: string };
export type SaveThemeResult =
  | { ok: true; id: string }
  | { ok: false; error: string };

/**
 * Persist the panel theme.
 *
 * Stores the CONFIG (accent + the three dials + any hand-pinned shades), never
 * the derived palette - so the palette can't go stale against a change in
 * `deriveTheme`, and a shade the operator did not pin keeps tracking the
 * accent.
 *
 * `parseThemeConfig` is the gate, and it is total: it drops unknown keys,
 * clamps the dials to 0-1, and runs every colour through `normalizeHex` so
 * nothing but `#rrggbb` survives. That matters because these values end up
 * inside an inline `style` attribute on every page. Re-serialising the PARSED
 * config (rather than the submitted string) is what guarantees only clean data
 * reaches the database.
 */
export async function setThemeAction(
  _prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
  const s = await requireAdmin();
  const raw = String(fd.get("theme") ?? "").trim();
  if (!raw) return { ok: false, error: "no theme submitted" };

  const cfg = parseThemeConfig(raw);
  const value = serializeThemeConfig(cfg);

  db()
    .prepare(
      `INSERT INTO settings (key, value) VALUES (?, ?)
       ON CONFLICT(key) DO UPDATE SET value = excluded.value`,
    )
    .run(THEME_SETTING_KEY, value);
  writeAudit({
    actorId: s.userId,
    action: "settings.set",
    target: THEME_SETTING_KEY,
    result: "ok",
    message: `${cfg.accent} v${cfg.vividness} t${cfg.tint} g${cfg.glow}${
      Object.keys(cfg.overrides).length
        ? ` +${Object.keys(cfg.overrides).length} pinned`
        : ""
    }`,
  });
  // The palette is injected by the ROOT layout, so every route in every group
  // renders it - revalidate the whole tree, the way the timezone change does.
  revalidatePath("/", "layout");
  return { ok: true };
}

/* -------------------------------------------------------------------------
 * Saved-theme library (`panel.themes`)
 *
 * Independent of the ACTIVE theme in `panel.theme`: saving a theme into the
 * library does not apply it, and applying one does not touch the library.
 * The root layout never reads the library at all.
 * ---------------------------------------------------------------------- */

function writeLibrary(list: SavedTheme[]): void {
  db()
    .prepare(
      `INSERT INTO settings (key, value) VALUES (?, ?)
       ON CONFLICT(key) DO UPDATE SET value = excluded.value`,
    )
    .run(THEME_LIBRARY_SETTING_KEY, serializeThemeLibrary(list));
}

/** Case-insensitive, so "Dragon" and "dragon" can't both sit in the grid. */
function nameTaken(list: SavedTheme[], name: string, exceptId?: string): boolean {
  const k = name.toLowerCase();
  return list.some((t) => t.id !== exceptId && t.name.toLowerCase() === k);
}

/** Save the submitted config into the library under a new name. */
export async function saveThemeAsAction(
  rawName: string,
  rawConfig: string,
): Promise<SaveThemeResult> {
  const s = await requireAdmin();
  const name = normalizeThemeName(String(rawName ?? ""));
  if (!name) return { ok: false, error: "give the theme a name" };

  const list = getThemeLibrary();
  if (list.length >= MAX_SAVED_THEMES) {
    return {
      ok: false,
      error: `you already have ${MAX_SAVED_THEMES} saved themes - delete one first`,
    };
  }
  if (nameTaken(list, name)) {
    return { ok: false, error: `"${name}" is already saved - pick another name` };
  }

  const id = randomUUID();
  writeLibrary([...list, { id, name, config: parseThemeConfig(String(rawConfig ?? "")) }]);
  writeAudit({
    actorId: s.userId,
    action: "settings.set",
    target: THEME_LIBRARY_SETTING_KEY,
    result: "ok",
    message: `saved "${name}"`,
  });
  revalidatePath("/settings");
  return { ok: true, id };
}

/** Point a saved theme at a different config, keeping its name and id. */
export async function updateSavedThemeAction(
  id: string,
  rawConfig: string,
): Promise<ActionResult> {
  const s = await requireAdmin();
  const list = getThemeLibrary();
  const found = list.find((t) => t.id === id);
  if (!found) return { ok: false, error: "that theme no longer exists" };

  writeLibrary(
    list.map((t) =>
      t.id === id ? { ...t, config: parseThemeConfig(String(rawConfig ?? "")) } : t,
    ),
  );
  writeAudit({
    actorId: s.userId,
    action: "settings.set",
    target: THEME_LIBRARY_SETTING_KEY,
    result: "ok",
    message: `updated "${found.name}"`,
  });
  revalidatePath("/settings");
  return { ok: true };
}

export async function renameSavedThemeAction(
  id: string,
  rawName: string,
): Promise<ActionResult> {
  const s = await requireAdmin();
  const name = normalizeThemeName(String(rawName ?? ""));
  if (!name) return { ok: false, error: "give the theme a name" };

  const list = getThemeLibrary();
  const found = list.find((t) => t.id === id);
  if (!found) return { ok: false, error: "that theme no longer exists" };
  if (nameTaken(list, name, id)) {
    return { ok: false, error: `"${name}" is already saved - pick another name` };
  }

  writeLibrary(list.map((t) => (t.id === id ? { ...t, name } : t)));
  writeAudit({
    actorId: s.userId,
    action: "settings.set",
    target: THEME_LIBRARY_SETTING_KEY,
    result: "ok",
    message: `renamed "${found.name}" to "${name}"`,
  });
  revalidatePath("/settings");
  return { ok: true };
}

/**
 * Remove a saved theme. Deliberately does NOT touch `panel.theme`: if the
 * operator is currently running this theme, deleting the bookmark should not
 * repaint their panel out from under them.
 */
export async function deleteSavedThemeAction(id: string): Promise<ActionResult> {
  const s = await requireAdmin();
  const list = getThemeLibrary();
  const found = list.find((t) => t.id === id);
  if (!found) return { ok: true };

  writeLibrary(list.filter((t) => t.id !== id));
  writeAudit({
    actorId: s.userId,
    action: "settings.set",
    target: THEME_LIBRARY_SETTING_KEY,
    result: "ok",
    message: `deleted "${found.name}"`,
  });
  revalidatePath("/settings");
  return { ok: true };
}
