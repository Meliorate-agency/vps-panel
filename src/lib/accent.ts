import "server-only";
import { getSetting } from "./settings";
import { DEFAULT_TAB_TITLE, TAB_TITLE_KEY, tabTitleOrDefault } from "./tabTitle";
import {
  DEFAULT_CONFIG,
  parseThemeConfig,
  parseThemeLibrary,
  type SavedTheme,
  type ThemeConfig,
} from "./theme";

/** Settings-table key holding the operator's theme config, as JSON. */
export const THEME_SETTING_KEY = "panel.theme";
/** Settings-table key holding the saved-theme library, as a JSON array. */
export const THEME_LIBRARY_SETTING_KEY = "panel.themes";
/**
 * The key this feature originally shipped with: a bare `#rrggbb`. Still read
 * as a fallback so an install that saved an accent before the dials existed
 * keeps its colour. `parseThemeConfig` accepts a bare hex directly.
 */
export const ACCENT_SETTING_KEY = "panel.accent";

/** The stored theme config, or the shipped default. Never throws. */
export function getThemeConfig(): ThemeConfig {
  // During `next build` the panel database must not be touched at all.
  // `scripts/install.sh` and `scripts/update.sh` both build ON the VPS as the
  // `vps-panel` user, with `/var/lib/vps-panel/panel.db` readable and the
  // service still running - and opening it runs `applyMigrations()`. Any route
  // prerendered at build time would therefore issue DDL against the live
  // database just to pick a colour. Skip the read; the default is fine for a
  // build-time snapshot, and `/login` is `force-dynamic` so it re-reads per
  // request anyway.
  if (process.env.NEXT_PHASE === "phase-production-build") {
    return { ...DEFAULT_CONFIG };
  }
  // At runtime a missing or unreadable DB (a half-finished install, a bad
  // PANEL_DB_PATH) must not take down every page in the panel, so a throw
  // falls back to the shipped default.
  try {
    const raw = getSetting(THEME_SETTING_KEY, "");
    if (raw) return parseThemeConfig(raw);
    const legacy = getSetting(ACCENT_SETTING_KEY, "");
    if (legacy) return parseThemeConfig(legacy);
    return { ...DEFAULT_CONFIG };
  } catch {
    return { ...DEFAULT_CONFIG };
  }
}

/**
 * The browser tab name (Settings > Appearance, lib/tabTitle.ts). The root
 * layout reads it for every page, so it never throws and stays out of the
 * database during `next build`, for the same reasons as `getThemeConfig`.
 */
export function getTabTitle(): string {
  if (process.env.NEXT_PHASE === "phase-production-build") return DEFAULT_TAB_TITLE;
  try {
    return tabTitleOrDefault(getSetting(TAB_TITLE_KEY, ""));
  } catch {
    return DEFAULT_TAB_TITLE;
  }
}

/**
 * The operator's saved themes. Only the settings page needs these - the root
 * layout renders the ACTIVE theme, which is a plain config in `panel.theme`
 * and does not depend on the library at all. Never throws, same reasoning as
 * `getThemeConfig`.
 */
export function getThemeLibrary(): SavedTheme[] {
  try {
    const raw = getSetting(THEME_LIBRARY_SETTING_KEY, "");
    return raw ? parseThemeLibrary(raw) : [];
  } catch {
    return [];
  }
}
