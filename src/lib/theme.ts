/**
 * Panel theming.
 *
 * The whole palette is generated from a small config: one accent colour, three
 * global "how loud should this be" dials, and optional per-shade overrides.
 * `deriveTheme` turns that into every colour token `globals.css` uses - the
 * accent family, the tinted dark surfaces, the borders and the text.
 *
 * The hue offsets and saturation ratios were reverse-engineered from the
 * original hand-picked purple, so `deriveTheme(DEFAULT_CONFIG)` reproduces
 * those eleven hexes byte-for-byte. Switching the accent never "re-designs"
 * the panel: it moves the same design around the colour wheel.
 *
 * Why the dials exist: saturated colour on a near-black background vibrates -
 * the chrome bleeds into the surfaces (they carry 38-50% of the accent's
 * saturation), the glow variants push saturation ABOVE the accent's own, and
 * an ambient wash sits behind everything. All three are legible design choices
 * at low chroma and eye-strain at high chroma, so all three are adjustable
 * instead of baked in.
 *
 * Shared by the server (renders the tokens onto <html>) and the client (live
 * preview in the settings form), so this file must stay free of `server-only`
 * and of any Node built-ins.
 */

export type ThemeTokens = {
  accent: string;
  accent2: string;
  accentGlow: string;
  accentHover: string;
  /** Readable foreground for text sitting ON an accent-filled surface. */
  accentFg: string;
  bg: string;
  bgElevSolid: string;
  bgElev2: string;
  border: string;
  borderStrong: string;
  text: string;
  textDim: string;
  /** "R, G, B" triplets, for `rgba(var(--accent-rgb), 0.35)` call sites. */
  accentRgb: string;
  accent2Rgb: string;
  accentGlowRgb: string;
  bgRgb: string;
  textDimRgb: string;
  /** Alpha of the two background radial washes and of the button bloom. */
  wash1: number;
  wash2: number;
  glowAlpha: number;
  /**
   * Whether the legibility floor moved the accent's lightness, and which way.
   * Null when the accent was used as picked. Reported rather than inferred
   * from "the output hex differs from the input" - the dials change the hex
   * too, without the floor ever engaging.
   */
  accentAdjusted: "lighter" | "darker" | null;
};

/** The shades an operator can pin by hand. Everything else is computed. */
export type ShadeKey =
  | "accent"
  | "accentHover"
  | "accentGlow"
  | "accent2"
  | "accentFg"
  | "bg"
  | "bgElevSolid"
  | "bgElev2"
  | "border"
  | "borderStrong"
  | "text"
  | "textDim";

export const SHADES: {
  key: ShadeKey;
  group: "Accent" | "Surfaces" | "Lines" | "Text";
  label: string;
  hint: string;
}[] = [
  { key: "accent", group: "Accent", label: "Accent", hint: "Links, focus rings, button mid-tone" },
  { key: "accentHover", group: "Accent", label: "Hover", hint: "Link hover" },
  { key: "accentGlow", group: "Accent", label: "Glow", hint: "Inline code, paths, the bright gradient stop" },
  { key: "accent2", group: "Accent", label: "Deep", hint: "The dark gradient stop" },
  { key: "accentFg", group: "Accent", label: "On accent", hint: "Label text on a filled button" },
  { key: "bg", group: "Surfaces", label: "Page", hint: "The page behind everything" },
  { key: "bgElevSolid", group: "Surfaces", label: "Card", hint: "Panels, popovers, the sidebar" },
  { key: "bgElev2", group: "Surfaces", label: "Inset", hint: "Inputs, code blocks, table headers" },
  { key: "border", group: "Lines", label: "Border", hint: "Card and input outlines" },
  { key: "borderStrong", group: "Lines", label: "Border strong", hint: "Hover and emphasis outlines" },
  { key: "text", group: "Text", label: "Text", hint: "Body copy" },
  { key: "textDim", group: "Text", label: "Muted text", hint: "Labels, hints, secondary rows" },
];

export type ThemeConfig = {
  accent: string;
  /** 0-1 multiplier on the accent family's saturation. 1 = the colour as picked. */
  vividness: number;
  /** 0-1 multiplier on how much hue the surfaces, borders and text carry. */
  tint: number;
  /** 0-1 multiplier on the ambient background wash and the button bloom. */
  glow: number;
  /** Hand-pinned shades. A present key wins over the derived value. */
  overrides: Partial<Record<ShadeKey, string>>;
};

export const DEFAULT_ACCENT = "#b537f2";

export const DEFAULT_CONFIG: ThemeConfig = {
  accent: DEFAULT_ACCENT,
  vividness: 1,
  tint: 1,
  glow: 1,
  overrides: {},
};

/**
 * Built-in themes.
 *
 * "Bolt Purple" is the shipped design at full strength and derives to the
 * exact palette that was hard-coded in globals.css. Every other preset is
 * deliberately dialled back: at full chroma a saturated hue on a near-black
 * page reads as glare rather than as an accent, so they ship calm and the
 * operator turns them UP if they want more, rather than discovering the
 * problem by installing one.
 */
export const THEME_PRESETS: {
  id: string;
  name: string;
  config: ThemeConfig;
}[] = [
  { id: "purple", name: "Bolt Purple", config: { accent: DEFAULT_ACCENT, vividness: 1, tint: 1, glow: 1, overrides: {} } },
  { id: "plum", name: "Plum", config: { accent: "#b537f2", vividness: 0.5, tint: 0.34, glow: 0.42, overrides: {} } },
  { id: "indigo", name: "Indigo", config: { accent: "#6366f1", vividness: 0.62, tint: 0.36, glow: 0.45, overrides: {} } },
  { id: "azure", name: "Azure", config: { accent: "#3b82f6", vividness: 0.6, tint: 0.34, glow: 0.42, overrides: {} } },
  { id: "olive", name: "Olive", config: { accent: "#a3e635", vividness: 0.42, tint: 0.3, glow: 0.34, overrides: {} } },
  { id: "sand", name: "Sand", config: { accent: "#f5a524", vividness: 0.48, tint: 0.32, glow: 0.36, overrides: {} } },
  // The last three pair a strong accent and a strong wash with chrome dialled
  // almost or entirely to grey. That is the combination the tint dial exists
  // for: with no chroma in the surfaces there is nothing for a loud accent to
  // vibrate against, so it can stay loud without glaring.
  { id: "dragon", name: "Dragon", config: { accent: "#ff4400", vividness: 0.62, tint: 0.1, glow: 0.82, overrides: {} } },
  { id: "venom", name: "Venom", config: { accent: "#81c80e", vividness: 0.65, tint: 0, glow: 0.72, overrides: {} } },
  { id: "siren", name: "Siren", config: { accent: "#bb166e", vividness: 0.7, tint: 0.1, glow: 0.68, overrides: {} } },
];

/** A theme the operator saved and named. */
export type SavedTheme = {
  id: string;
  name: string;
  config: ThemeConfig;
};

/** Bounds the settings row, which is parsed on every page render. */
export const MAX_SAVED_THEMES = 40;
export const MAX_THEME_NAME = 40;

/**
 * Trim, collapse whitespace, strip control characters, cap the length.
 * Returns null for a name that is empty once cleaned.
 */
export function normalizeThemeName(input: string): string | null {
  const cleaned = input
    // C0/C1 controls, DEL, and the zero-width + bidi characters that would
    // otherwise let a saved name reorder or hide the text around it in the
    // preset grid. Replaced with a space, then collapsed.
    .replace(/[\u0000-\u001f\u007f-\u009f\u200b-\u200f\u2028\u2029\u202a-\u202e\u2066-\u2069\ufeff]/g, " ")
    .replace(/\s+/g, " ")
    .trim()
    .slice(0, MAX_THEME_NAME)
    .trim();
  return cleaned.length ? cleaned : null;
}

/**
 * Parse the saved-theme library. Total, like `parseThemeConfig`: a malformed
 * entry is dropped rather than throwing, because this is read on the settings
 * page and a corrupt row must not take it down.
 */
export function parseThemeLibrary(raw: string): SavedTheme[] {
  let arr: unknown;
  try {
    arr = JSON.parse(raw);
  } catch {
    return [];
  }
  if (!Array.isArray(arr)) return [];

  const out: SavedTheme[] = [];
  const seen = new Set<string>();
  for (const item of arr) {
    if (out.length >= MAX_SAVED_THEMES) break;
    if (!item || typeof item !== "object" || Array.isArray(item)) continue;
    const o = item as Record<string, unknown>;
    if (typeof o.id !== "string" || !/^[A-Za-z0-9_-]{1,64}$/.test(o.id)) continue;
    if (seen.has(o.id)) continue;
    const name = typeof o.name === "string" ? normalizeThemeName(o.name) : null;
    if (!name) continue;
    seen.add(o.id);
    out.push({
      id: o.id,
      name,
      // Route the config through the same gate as the active theme, so a
      // saved entry can never carry a colour the live one could not.
      config: parseThemeConfig(JSON.stringify(o.config ?? {})),
    });
  }
  return out;
}

/** Round-trips through `parseThemeLibrary`, so only clean data is stored. */
export function serializeThemeLibrary(list: SavedTheme[]): string {
  return JSON.stringify(
    parseThemeLibrary(JSON.stringify(list)).map((t) => ({
      id: t.id,
      name: t.name,
      config: JSON.parse(serializeThemeConfig(t.config)),
    })),
  );
}

const clamp = (n: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, n));

/** Accepts `#rgb` / `#rrggbb` (case-insensitive, `#` optional). */
export function normalizeHex(input: string): string | null {
  const raw = input.trim().replace(/^#/, "");
  if (/^[0-9a-f]{3}$/i.test(raw)) {
    return `#${raw
      .split("")
      .map((c) => c + c)
      .join("")}`.toLowerCase();
  }
  if (/^[0-9a-f]{6}$/i.test(raw)) return `#${raw}`.toLowerCase();
  return null;
}

type Rgb = [number, number, number];

function hexToRgb(hex: string): Rgb {
  const h = hex.replace("#", "");
  return [
    parseInt(h.slice(0, 2), 16),
    parseInt(h.slice(2, 4), 16),
    parseInt(h.slice(4, 6), 16),
  ];
}

function rgbToHex([r, g, b]: Rgb): string {
  return (
    "#" +
    [r, g, b]
      .map((v) => clamp(Math.round(v), 0, 255).toString(16).padStart(2, "0"))
      .join("")
  );
}

function rgbToHsl([r0, g0, b0]: Rgb): [number, number, number] {
  const r = r0 / 255;
  const g = g0 / 255;
  const b = b0 / 255;
  const mx = Math.max(r, g, b);
  const mn = Math.min(r, g, b);
  const l = (mx + mn) / 2;
  const d = mx - mn;
  let h = 0;
  let s = 0;
  if (d !== 0) {
    s = d / (1 - Math.abs(2 * l - 1));
    if (mx === r) h = ((g - b) / d) % 6;
    else if (mx === g) h = (b - r) / d + 2;
    else h = (r - g) / d + 4;
    h *= 60;
    if (h < 0) h += 360;
  }
  return [h, s * 100, l * 100];
}

function hslToRgb(h0: number, s0: number, l0: number): Rgb {
  const h = ((h0 % 360) + 360) % 360;
  const s = clamp(s0, 0, 100) / 100;
  const l = clamp(l0, 0, 100) / 100;
  const c = (1 - Math.abs(2 * l - 1)) * s;
  const x = c * (1 - Math.abs(((h / 60) % 2) - 1));
  const m = l - c / 2;
  let r = 0;
  let g = 0;
  let b = 0;
  const seg = Math.floor(h / 60);
  if (seg === 0) [r, g] = [c, x];
  else if (seg === 1) [r, g] = [x, c];
  else if (seg === 2) [g, b] = [c, x];
  else if (seg === 3) [g, b] = [x, c];
  else if (seg === 4) [r, b] = [x, c];
  else [r, b] = [c, x];
  return [
    Math.round((r + m) * 255),
    Math.round((g + m) * 255),
    Math.round((b + m) * 255),
  ];
}

/** WCAG 2.x relative luminance. */
function luminance([r, g, b]: Rgb): number {
  const f = (v0: number) => {
    const v = v0 / 255;
    return v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
  };
  return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b);
}

function contrast(a: Rgb, b: Rgb): number {
  const la = luminance(a);
  const lb = luminance(b);
  const [hi, lo] = la > lb ? [la, lb] : [lb, la];
  return (hi + 0.05) / (lo + 0.05);
}

/**
 * Lightness window the accent has to live in. Below it, accent-coloured text
 * and 1px borders vanish against the near-black background; above it, the
 * "glow" variants clip to white and the family collapses into one colour.
 * The purple default (L 58.2) sits comfortably inside, so it is untouched.
 */
const L_MIN = 42;
const L_MAX = 74;
/**
 * `a { color: var(--accent) }` and `.code { color: var(--accent-glow) }` make
 * both of those tokens body text, so both have to clear WCAG AA for normal
 * text against the page background. The shipped purple sits at 4.61 / 6.05,
 * just over the line - which is why the floor is enforced rather than assumed.
 *
 * The glow is genuinely the binding constraint for some hues: it is +11deg
 * around the wheel, which pushes a mid blue further into the darkest part of
 * the spectrum faster than its +3.6 lightness bump lifts it back out.
 */
const MIN_INK_CONTRAST = 4.5;
/**
 * White stays the label colour on accent fills down to this contrast ratio.
 * Calibrated to the shipped purple, whose worst gradient stop sits at 3.34 -
 * anything at least as legible as the design we already ship keeps the
 * dark-UI look; only genuinely pale accents (cyan, lime, amber) flip to a
 * dark label.
 */
const WHITE_FG_FLOOR = 3.3;
/**
 * Below this, the label is failing badly enough that looking good stops
 * mattering. Only then does flat black get considered - see `deriveTheme`.
 */
const ABSOLUTE_FG_FLOOR = 3.0;

function normalizeAccent(
  hex: string,
  s: number,
  chroma: number,
): { h: number; l: number; adjusted: "lighter" | "darker" | null } {
  const [h, , l0] = rgbToHsl(hexToRgb(hex));
  let l = clamp(l0, L_MIN, L_MAX);
  // The background is a function of hue and saturation only, so it is already
  // settled before the accent's lightness is: measure against the real one.
  const bg = hslToRgb(h - 30, chroma * 0.38, 3.5);
  // Equal HSL lightness is not equal perceived brightness - a blue at L=42 is
  // far darker than a yellow at L=42 - so lift dark hues until they actually
  // read against that background rather than trusting the HSL floor alone.
  const readable = (li: number) =>
    contrast(hslToRgb(h, s, li), bg) >= MIN_INK_CONTRAST &&
    contrast(hslToRgb(h + 11, s * 1.092, li + 3.6), bg) >= MIN_INK_CONTRAST;
  for (let i = 0; i < 100 && l < 92 && !readable(l); i++) l += 0.5;
  // Reported so the settings form can say what actually happened. It must not
  // infer it from "the hex changed": the dials change the hex too, and telling
  // an operator their colour was lightened for contrast when it was really
  // just desaturated by their own slider is worse than saying nothing.
  const adjusted =
    l > l0 + 0.5 ? "lighter" : l < l0 - 0.5 ? "darker" : null;
  return { h, l, adjusted };
}

/** Alpha of the two background washes and the button bloom at glow = 1. */
const WASH_1 = 0.22;
const WASH_2 = 0.14;
const GLOW_ALPHA = 0.35;

const round3 = (n: number) => Math.round(n * 1000) / 1000;

/**
 * Derive the full token set.
 *
 * The hue offsets / saturation ratios / absolute lightnesses below are the
 * measured relationships inside the original purple palette. Surface
 * lightness is absolute (not relative to the accent) so the panel stays a
 * dark theme no matter how bright or dim the chosen accent is.
 *
 * `vividness` scales the accent family; `tint` scales the chrome on top of
 * that, so "vivid accent, near-grey chrome" is reachable - which is the
 * combination that stops a saturated hue from vibrating against near-black.
 * At vividness = tint = glow = 1 every line below is arithmetically identical
 * to the palette that shipped hard-coded.
 */
export function deriveTheme(input: ThemeConfig | string): ThemeTokens {
  const cfg = typeof input === "string" ? configFromAccent(input) : input;
  const hex = normalizeHex(cfg.accent) ?? DEFAULT_ACCENT;
  // Re-validated here, not just in parseThemeConfig: this is the last function
  // before the values land in an inline style attribute, and it is also called
  // with hand-built configs (presets, live form state). A NaN slipping through
  // would render every token as the literal string "#NaNNaNNaN".
  const vividness = clamp01(cfg.vividness, 1);
  const tint = clamp01(cfg.tint, 1);
  const glow01 = clamp01(cfg.glow, 1);

  const [h0, sRaw] = rgbToHsl(hexToRgb(hex));
  const s = sRaw * vividness;
  // Chrome saturation is downstream of the accent's: dialling the accent to
  // grey should take the surfaces with it unless `tint` says otherwise.
  const chroma = s * tint;
  const { h, l, adjusted } = normalizeAccent(hex, s, chroma);

  const mk = (dh: number, sat: number, absL: number): Rgb =>
    hslToRgb(h + dh, sat, absL);

  const base = {
    accent: hslToRgb(h, s, l),
    accent2: mk(-11, s * 0.854, l - 9.6),
    accentGlow: mk(11, s * 1.092, l + 3.6),
    accentHover: mk(0.4, s * 1.139, l + 6.9),
    bg: mk(-30, chroma * 0.38, 3.5),
    bgElevSolid: mk(-24, chroma * 0.5, 9.8),
    bgElev2: mk(-22, chroma * 0.49, 13.7),
    border: mk(-24, chroma * 0.453, 20.2),
    borderStrong: mk(-21, chroma * 0.475, 28.2),
    text: mk(-30, chroma * 1.139, 97.6),
    textDim: mk(-23, chroma * 0.325, 69.8),
  };

  // Overrides land BEFORE --accent-fg is chosen, so a hand-pinned gradient
  // still gets a label colour measured against the colours actually used.
  const pin = (key: ShadeKey, fallback: Rgb): Rgb => {
    const o = cfg.overrides?.[key];
    const hexed = o ? normalizeHex(o) : null;
    return hexed ? hexToRgb(hexed) : fallback;
  };
  const out = {} as Record<Exclude<ShadeKey, "accentFg">, Rgb>;
  for (const k of Object.keys(base) as (keyof typeof base)[]) {
    out[k] = pin(k, base[k]);
  }

  // `.btn` fills with the accent gradient (glow -> accent -> accent-2), so the
  // label has to be legible on all three stops. Prefer white - this is a dark
  // UI - and only fall back to a near-black tint of the same hue when white
  // genuinely loses (pale accents: sand, olive, teal).
  const white: Rgb = [255, 255, 255];
  const darkFg = mk(-30, Math.min(s, 60), 8);
  const worst = (fg: Rgb) =>
    Math.min(
      contrast(fg, out.accent),
      contrast(fg, out.accentGlow),
      contrast(fg, out.accent2),
    );
  const worstWhite = worst(white);
  let fg =
    worstWhite >= WHITE_FG_FLOOR || worstWhite >= worst(darkFg)
      ? white
      : darkFg;
  // Last resort. A mid-dark saturated accent - a deep orange or brown - can
  // leave BOTH preferred candidates under 3:1, because the hue-tinted ink is
  // not actually the darkest available colour. Flat black is uglier than ink
  // that shares the accent's hue, so it is only reached once the nice options
  // have already failed; at that point legibility outranks the tint.
  const black: Rgb = [0, 0, 0];
  if (worst(fg) < ABSOLUTE_FG_FLOOR && worst(black) > worst(fg)) fg = black;
  const accentFg = pin("accentFg", fg);

  const triplet = ([r, g, b]: Rgb) => `${r}, ${g}, ${b}`;

  return {
    accent: rgbToHex(out.accent),
    accent2: rgbToHex(out.accent2),
    accentGlow: rgbToHex(out.accentGlow),
    accentHover: rgbToHex(out.accentHover),
    accentFg: rgbToHex(accentFg),
    bg: rgbToHex(out.bg),
    bgElevSolid: rgbToHex(out.bgElevSolid),
    bgElev2: rgbToHex(out.bgElev2),
    border: rgbToHex(out.border),
    borderStrong: rgbToHex(out.borderStrong),
    text: rgbToHex(out.text),
    textDim: rgbToHex(out.textDim),
    accentRgb: triplet(out.accent),
    accent2Rgb: triplet(out.accent2),
    accentGlowRgb: triplet(out.accentGlow),
    bgRgb: triplet(out.bg),
    textDimRgb: triplet(out.textDim),
    wash1: round3(WASH_1 * glow01),
    wash2: round3(WASH_2 * glow01),
    glowAlpha: round3(GLOW_ALPHA * glow01),
    // A pinned accent bypasses the floor entirely, so nothing was adjusted.
    accentAdjusted: cfg.overrides?.accent ? null : adjusted,
  };
}

/**
 * The accent-derived half of the palette, as a custom-property map.
 *
 * These land in the `style` attribute of `<html>` rather than in a `<style>`
 * block, for two reasons: an inline style always outranks the `:root` rule in
 * `globals.css` no matter which order Next flushes the stylesheet link and the
 * markup in, and the settings form can mirror the exact same map onto
 * `document.documentElement.style` for a live preview.
 *
 * `globals.css` keeps the full purple defaults so the app is never unstyled if
 * this map is missing - the static half of the palette (radius, danger / warn
 * / ok, fonts) lives only there and is deliberately not themed.
 */
export function themeVars(t: ThemeTokens): Record<string, string> {
  return {
    "--accent": t.accent,
    "--accent-2": t.accent2,
    "--accent-glow": t.accentGlow,
    "--accent-hover": t.accentHover,
    "--accent-fg": t.accentFg,
    "--accent-rgb": t.accentRgb,
    "--accent-2-rgb": t.accent2Rgb,
    "--accent-glow-rgb": t.accentGlowRgb,
    "--bg": t.bg,
    "--bg-rgb": t.bgRgb,
    // Deliberately the same opaque colour as --bg-elev-solid. The old value
    // was `rgba(20, 14, 36)` with no alpha, which browsers drop - see the
    // note at Topbar.module.css .updatePop.
    "--bg-elev": t.bgElevSolid,
    "--bg-elev-solid": t.bgElevSolid,
    "--bg-elev-2": t.bgElev2,
    "--border": t.border,
    "--border-strong": t.borderStrong,
    "--text": t.text,
    "--text-dim": t.textDim,
    "--text-dim-rgb": t.textDimRgb,
    "--scrim": `rgba(${t.bgRgb}, 0.62)`,
    // Alpha of the two body radial washes. Separate tokens rather than baked
    // into --gradient-* because the wash is the single biggest source of
    // ambient chroma, and turning it down is most of what "calmer" means.
    "--wash-1": String(t.wash1),
    "--wash-2": String(t.wash2),
    "--gradient-accent": `linear-gradient(135deg, ${t.accentGlow} 0%, ${t.accent} 50%, ${t.accent2} 100%)`,
    "--gradient-stripe": `linear-gradient(90deg, transparent 0%, ${t.accent} 50%, transparent 100%)`,
    "--shadow-glow": `0 0 24px rgba(${t.accentRgb}, ${t.glowAlpha})`,
  };
}

const SHADE_KEYS = new Set<string>(SHADES.map((s) => s.key));

/**
 * Strict on purpose: only a real, finite number counts. Coercing instead would
 * turn `null`, `[]` and `""` into 0 - a silently colourless panel - when what
 * those actually mean is "absent", which should keep the default.
 */
function clamp01(v: unknown, fallback: number): number {
  return typeof v === "number" && Number.isFinite(v)
    ? clamp(v, 0, 1)
    : fallback;
}

/**
 * Parse a stored theme config.
 *
 * Deliberately total: any shape of junk yields a usable config rather than
 * throwing, because this runs in the root layout above every route. Accepts a
 * bare hex string too, which is what the setting held before the dials and
 * per-shade overrides existed.
 *
 * Every value is re-validated here rather than trusted from the database -
 * these end up in an inline `style` attribute, so `normalizeHex` is the only
 * gate between a settings row and the markup.
 */
export function parseThemeConfig(raw: string): ThemeConfig {
  const bare = normalizeHex(raw);
  if (bare) return { ...DEFAULT_CONFIG, accent: bare };

  let obj: unknown;
  try {
    obj = JSON.parse(raw);
  } catch {
    return { ...DEFAULT_CONFIG };
  }
  if (!obj || typeof obj !== "object" || Array.isArray(obj)) {
    return { ...DEFAULT_CONFIG };
  }
  const o = obj as Record<string, unknown>;

  const overrides: Partial<Record<ShadeKey, string>> = {};
  const rawOverrides = o.overrides;
  if (rawOverrides && typeof rawOverrides === "object" && !Array.isArray(rawOverrides)) {
    for (const [k, v] of Object.entries(rawOverrides as Record<string, unknown>)) {
      if (!SHADE_KEYS.has(k) || typeof v !== "string") continue;
      const hex = normalizeHex(v);
      if (hex) overrides[k as ShadeKey] = hex;
    }
  }

  return {
    accent:
      (typeof o.accent === "string" ? normalizeHex(o.accent) : null) ??
      DEFAULT_ACCENT,
    vividness: clamp01(o.vividness, 1),
    tint: clamp01(o.tint, 1),
    glow: clamp01(o.glow, 1),
    overrides,
  };
}

/**
 * Round-trips through `parseThemeConfig`, so only clean data is ever stored.
 *
 * Overrides are emitted in a canonical order rather than insertion order: the
 * settings form compares two serialised configs to decide whether there is
 * anything to save, and pinning shades in a different sequence must not read
 * as a different theme.
 */
export function serializeThemeConfig(cfg: ThemeConfig): string {
  const c = parseThemeConfig(JSON.stringify(cfg));
  const overrides: Partial<Record<ShadeKey, string>> = {};
  for (const { key } of SHADES) {
    const v = c.overrides[key];
    if (v) overrides[key] = v;
  }
  return JSON.stringify({
    accent: c.accent,
    vividness: c.vividness,
    tint: c.tint,
    glow: c.glow,
    overrides,
  });
}

export function configFromAccent(accent: string): ThemeConfig {
  return { ...DEFAULT_CONFIG, accent: normalizeHex(accent) ?? DEFAULT_ACCENT };
}
