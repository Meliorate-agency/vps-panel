"use client";

import {
  startTransition,
  useActionState,
  useEffect,
  useMemo,
  useRef,
  useState,
  useTransition,
} from "react";
import {
  DEFAULT_CONFIG,
  MAX_THEME_NAME,
  SHADES,
  THEME_PRESETS,
  type SavedTheme,
  type ShadeKey,
  type ThemeConfig,
  type ThemeTokens,
  deriveTheme,
  normalizeHex,
  normalizeThemeName,
  parseThemeConfig,
  serializeThemeConfig,
  themeVars,
} from "@/lib/theme";
import ConfirmDeleteButton from "@/components/ConfirmDeleteButton/ConfirmDeleteButton";
import styles from "@/components/page.module.css";
import own from "./ThemeForm.module.css";

type Result = { ok: true } | { ok: false; error: string };
type SaveResult = { ok: true; id: string } | { ok: false; error: string };

/** Write a derived palette straight onto <html>, exactly as the root layout does. */
function applyTheme(cfg: ThemeConfig) {
  const el = document.documentElement;
  for (const [k, v] of Object.entries(themeVars(deriveTheme(cfg)))) {
    el.style.setProperty(k, v);
  }
}

const GROUPS = ["Accent", "Surfaces", "Lines", "Text"] as const;

const DIALS: {
  key: "vividness" | "tint" | "glow";
  label: string;
  hint: string;
}[] = [
  {
    key: "vividness",
    label: "Vividness",
    hint: "How saturated the accent itself is. Turn it down for a muted, low-glare accent.",
  },
  {
    key: "tint",
    label: "Chrome tint",
    hint: "How much of the accent's colour bleeds into surfaces, borders and text. At 0 the UI furniture is neutral grey.",
  },
  {
    key: "glow",
    label: "Ambient glow",
    hint: "The two washes behind the page and the bloom on hovered buttons. The biggest single source of on-screen colour.",
  },
];

const pct = (n: number) => `${Math.round(n * 100)}%`;

/**
 * A miniature of the panel: that theme's accent gradient on that theme's own
 * card surface, over its own border. The chrome is most of what a theme
 * changes, so a bare accent chip would hide the difference between the loud
 * and the calm ones.
 *
 * Module scope, not nested in ThemeForm: a component declared inside a render
 * is a new type every time, which makes React unmount and rebuild the whole
 * swatch grid on every keystroke and dial drag.
 */
function Chip({ t }: { t: ThemeTokens }) {
  return (
    <span className={own.chip} style={{ background: t.bgElevSolid }}>
      <span
        className={own.chipDot}
        style={{
          background: `linear-gradient(135deg, ${t.accentGlow} 0%, ${t.accent} 50%, ${t.accent2} 100%)`,
        }}
      />
      <span className={own.chipLine} style={{ background: t.borderStrong }} />
    </span>
  );
}

export default function ThemeForm({
  value,
  savedThemes,
  action,
  saveAsAction,
  updateSavedAction,
  renameSavedAction,
  deleteSavedAction,
}: {
  value: string;
  savedThemes: SavedTheme[];
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
  saveAsAction: (name: string, config: string) => Promise<SaveResult>;
  updateSavedAction: (id: string, config: string) => Promise<Result>;
  renameSavedAction: (id: string, name: string) => Promise<Result>;
  deleteSavedAction: (id: string) => Promise<Result>;
}) {
  const [state, formAction, pending] = useActionState<Result | null, FormData>(
    action,
    null,
  );
  // `value` is the raw settings row and may be anything (or the legacy bare
  // hex). Normalise before it reaches state - the hidden field posts on the
  // very first render, before any effect can correct it.
  //
  // The canonical STRING is the memo that matters. React is allowed to discard
  // a useMemo cache, which would hand back a fresh object for an unchanged
  // `value`; anything keyed on that object identity would then re-run and wipe
  // the operator's half-dragged dials. A string compares by value, so the
  // reset effect below only fires when the server's theme genuinely changed.
  const savedJson = useMemo(
    () => serializeThemeConfig(parseThemeConfig(value)),
    [value],
  );
  const saved = useMemo(() => parseThemeConfig(savedJson), [savedJson]);

  const [cfg, setCfg] = useState<ThemeConfig>(saved);
  // The hex field is allowed to hold mid-typing garbage without disturbing the
  // preview, so it is tracked separately from the committed accent.
  const [accentText, setAccentText] = useState(saved.accent);

  const json = serializeThemeConfig(cfg);
  const dirty = json !== savedJson;
  const theme = useMemo(() => deriveTheme(cfg), [cfg]);
  const pinned = Object.keys(cfg.overrides).length;

  // Which SAVED theme the editor is currently working on. Tracked explicitly
  // rather than inferred by comparing configs: the whole point of Update is
  // that you have changed something, so a config match would make the button
  // disappear exactly when it is wanted.
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [name, setName] = useState("");
  const [libError, setLibError] = useState<string | null>(null);
  const [libNotice, setLibNotice] = useState<string | null>(null);
  const [busy, startLib] = useTransition();
  const selected = savedThemes.find((t) => t.id === selectedId) ?? null;
  // Exactly what the server will store, so the success notice and the
  // Rename-enabled check cannot disagree with the name that lands in the DB.
  const cleanName = normalizeThemeName(name);

  // A theme deleted in another tab must not leave a dangling selection.
  useEffect(() => {
    if (selectedId && !savedThemes.some((t) => t.id === selectedId)) {
      setSelectedId(null);
    }
  }, [savedThemes, selectedId]);

  function runLib(fn: () => Promise<Result | SaveResult>, ok: string) {
    if (busy) return;
    setLibError(null);
    setLibNotice(null);
    startLib(async () => {
      try {
        const r = await fn();
        // Everything after the await needs its OWN startTransition. React
        // restores the transition context when useTransition's callback
        // suspends, so a bare set here would be a default-lane update and
        // would commit BEFORE the router's revalidated props - handing the
        // dangling-selection effect below a savedThemes array that does not
        // contain the id we just selected, which it would then clear.
        startTransition(() => {
          if (!r.ok) {
            setLibError(r.error);
            return;
          }
          if ("id" in r) setSelectedId(r.id);
          setLibNotice(ok);
        });
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        startTransition(() => setLibError(msg));
      }
    });
  }

  // Live preview: the same tokens the server would have injected, applied to
  // the live document so the whole panel - sidebar, topbar, tables - repaints.
  useEffect(() => {
    applyTheme(cfg);
  }, [cfg]);

  // Re-sync to the server's value after a save revalidates this page, and drop
  // any unsaved preview if the operator navigates away instead of saving.
  const savedRef = useRef(savedJson);
  savedRef.current = savedJson;
  useEffect(() => {
    const next = parseThemeConfig(savedJson);
    setCfg(next);
    setAccentText(next.accent);
  }, [savedJson]);
  useEffect(() => () => applyTheme(parseThemeConfig(savedRef.current)), []);

  const patch = (p: Partial<ThemeConfig>) => setCfg((c) => ({ ...c, ...p }));

  function pickPreset(preset: ThemeConfig) {
    // A built-in is a fresh start: it drops hand-pinned shades, otherwise the
    // swatch you clicked is not the theme you get, and it drops the saved-theme
    // selection so Update / Rename / Delete stop pointing at something else.
    setCfg({ ...preset, overrides: {} });
    setAccentText(preset.accent);
    setSelectedId(null);
    setName("");
    setLibError(null);
    setLibNotice(null);
  }

  function pickSaved(t: SavedTheme) {
    // Unlike a built-in, a saved theme restores its pinned shades too - they
    // are part of what was saved.
    setCfg(t.config);
    setAccentText(t.config.accent);
    setSelectedId(t.id);
    setName(t.name);
    setLibError(null);
    setLibNotice(null);
  }

  function setAccent(hex: string) {
    patch({ accent: hex });
    setAccentText(hex);
  }

  function setShade(key: ShadeKey, hex: string | null) {
    setCfg((c) => {
      const next = { ...c.overrides };
      if (hex) next[key] = hex;
      else delete next[key];
      return { ...c, overrides: next };
    });
  }

  return (
    <form action={formAction}>
      <input type="hidden" name="theme" value={json} />

      <div className={own.swatches}>
        {THEME_PRESETS.map((p) => {
          // Show the DERIVED palette, not the raw preset colour, so the swatch
          // is exactly what clicking it produces. A built-in only reads as
          // selected while no saved theme is loaded, so the two groups can
          // never both light up.
          const t = deriveTheme(p.config);
          const active =
            selectedId === null &&
            serializeThemeConfig({ ...p.config, overrides: {} }) ===
              serializeThemeConfig({ ...cfg, overrides: {} });
          return (
            <button
              key={p.id}
              type="button"
              className={own.swatch}
              aria-pressed={active}
              title={t.accent}
              onClick={() => pickPreset(p.config)}
            >
              <Chip t={t} />
              <span className={own.swatchName}>{p.name}</span>
            </button>
          );
        })}
      </div>

      {savedThemes.length > 0 && (
        <>
          <div className={own.swatchGroupName}>Your themes</div>
          <div className={own.swatches}>
            {savedThemes.map((st) => {
              const t = deriveTheme(st.config);
              return (
                <button
                  key={st.id}
                  type="button"
                  className={`${own.swatch} ${own.swatchSaved}`}
                  aria-pressed={selectedId === st.id}
                  title={t.accent}
                  onClick={() => pickSaved(st)}
                >
                  <Chip t={t} />
                  <span className={own.swatchName}>{st.name}</span>
                </button>
              );
            })}
          </div>
        </>
      )}

      <div className={own.library}>
        <label className={styles.field}>
          Theme name
          <input
            type="text"
            className={`${styles.input} ${own.nameInput}`}
            value={name}
            maxLength={MAX_THEME_NAME}
            placeholder="Dragon"
            autoComplete="off"
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key !== "Enter") return;
              // This form has a submit button, so Enter here would otherwise
              // trigger implicit submission and APPLY the theme - the opposite
              // of what someone typing a name into "save as" is asking for.
              e.preventDefault();
              if (busy || !cleanName) return;
              if (selected && cleanName !== selected.name) {
                runLib(
                  () => renameSavedAction(selected.id, name),
                  `Renamed to "${cleanName}".`,
                );
              } else if (!selected) {
                runLib(
                  () => saveAsAction(name, json),
                  `Saved as "${cleanName}".`,
                );
              }
            }}
          />
        </label>
        <div className={own.libraryActions}>
          <button
            type="button"
            className={styles.btnGhost}
            disabled={busy || !cleanName}
            onClick={() =>
              runLib(
                () => saveAsAction(name, json),
                `Saved as "${cleanName}".`,
              )
            }
            title="Store the current settings in your themes"
          >
            {busy ? "Working..." : "Save as new theme"}
          </button>
          {selected && (
            <>
              <button
                type="button"
                className={styles.btnGhost}
                disabled={busy}
                onClick={() =>
                  runLib(
                    () => updateSavedAction(selected.id, json),
                    `Updated "${selected.name}".`,
                  )
                }
                title="Overwrite this saved theme with the current settings"
              >
                Update &ldquo;{selected.name}&rdquo;
              </button>
              <button
                type="button"
                className={styles.btnGhost}
                disabled={busy || !cleanName || cleanName === selected.name}
                onClick={() =>
                  runLib(
                    () => renameSavedAction(selected.id, name),
                    `Renamed to "${cleanName}".`,
                  )
                }
                title="Rename this saved theme to the name above"
              >
                Rename
              </button>
              <ConfirmDeleteButton
                buttonLabel="Delete"
                title={`Delete "${selected.name}"?`}
                body={
                  <>
                    This removes the saved theme. The panel keeps whatever
                    theme is currently applied &mdash; only the bookmark goes.
                  </>
                }
                confirmLabel="Delete theme"
                action={async () => {
                  const r = await deleteSavedAction(selected.id);
                  if (!r.ok) throw new Error(r.error);
                  setSelectedId(null);
                  setName("");
                }}
              />
            </>
          )}
        </div>
      </div>
      {libError && <div className={styles.error}>{libError}</div>}
      {libNotice && !libError && (
        <div className={styles.notice}>{libNotice}</div>
      )}
      <p className={styles.muted} style={{ fontSize: 12, margin: "0 0 16px" }}>
        Saving a theme here only bookmarks it &mdash; it does not apply it. Use
        &quot;Save theme&quot; at the bottom to make the current settings the
        panel&apos;s live theme.
      </p>

      <div className={own.custom}>
        <label className={styles.field}>
          Accent colour
          <span className={own.pickerRow}>
            {/* Seeded with the operator's OWN accent, not the derived one.
                Whatever this returns is stored as the base accent and gets the
                dials applied to it, so showing the derived colour here would
                re-open the picker on an already-dialled-down value and apply
                vividness a second time - each nudge walking the accent toward
                grey. The derived result is on show in the preview, the preset
                chips, and the "renders as" line below. */}
            <input
              type="color"
              className={own.picker}
              value={normalizeHex(cfg.accent) ?? DEFAULT_CONFIG.accent}
              onChange={(e) => setAccent(e.target.value.toLowerCase())}
              aria-label="Pick a custom accent colour"
            />
            <input
              type="text"
              className={`${styles.input} ${own.hexInput}`}
              value={accentText}
              spellCheck={false}
              autoComplete="off"
              placeholder={DEFAULT_CONFIG.accent}
              onChange={(e) => {
                setAccentText(e.target.value);
                const hex = normalizeHex(e.target.value);
                if (hex) patch({ accent: hex });
              }}
            />
          </span>
        </label>
      </div>

      <div className={own.dials}>
        {DIALS.map((d) => (
          <label key={d.key} className={own.dial}>
            <span className={own.dialHead}>
              <span className={own.dialLabel}>{d.label}</span>
              <span className={own.dialValue}>{pct(cfg[d.key])}</span>
            </span>
            <input
              type="range"
              className={own.range}
              min={0}
              max={100}
              step={1}
              value={Math.round(cfg[d.key] * 100)}
              onChange={(e) => patch({ [d.key]: Number(e.target.value) / 100 })}
            />
            <span className={own.dialHint}>{d.hint}</span>
          </label>
        ))}
      </div>

      <div className={own.preview}>
        <span className={own.previewBtn}>Primary button</span>
        <span className={own.previewLink}>A link</span>
        <span className={own.previewCode}>/etc/vps-panel</span>
        <span className={own.previewCard}>
          <span className={own.previewCardText}>Card surface</span>
          <span className={own.previewCardDim}>muted label</span>
        </span>
      </div>

      <details className={own.fine}>
        <summary className={own.fineSummary}>
          Fine-tune each shade
          {pinned > 0 && (
            <span className={own.pinnedCount}>
              {pinned} pinned
            </span>
          )}
        </summary>
        <p className={styles.muted} style={{ fontSize: 12, margin: "8px 0 12px" }}>
          Every shade below is computed from the accent and the dials above. Pin
          one to a fixed colour and it stops tracking them; reset it and it
          starts again.
        </p>
        {GROUPS.map((group) => (
          <div key={group} className={own.shadeGroup}>
            <div className={own.shadeGroupName}>{group}</div>
            {SHADES.filter((sh) => sh.group === group).map((sh) => {
              const current = theme[sh.key];
              const isPinned = cfg.overrides[sh.key] !== undefined;
              return (
                <div key={sh.key} className={own.shadeRow}>
                  <input
                    type="color"
                    className={own.shadePicker}
                    value={current}
                    onChange={(e) =>
                      setShade(sh.key, e.target.value.toLowerCase())
                    }
                    aria-label={`${sh.label} colour`}
                  />
                  <span className={own.shadeText}>
                    <span className={own.shadeLabel}>
                      {sh.label}
                      {isPinned && <span className={own.pin}>pinned</span>}
                    </span>
                    <span className={own.shadeHint}>{sh.hint}</span>
                  </span>
                  <code className={own.shadeHex}>{current}</code>
                  <button
                    type="button"
                    className={own.shadeReset}
                    onClick={() => setShade(sh.key, null)}
                    disabled={!isPinned}
                    title={
                      isPinned
                        ? "Go back to the computed colour"
                        : "Already computed from the accent"
                    }
                  >
                    Reset
                  </button>
                </div>
              );
            })}
          </div>
        ))}
      </details>

      <p className={styles.muted} style={{ fontSize: 12, margin: "12px 0" }}>
        Accent renders as <span className={styles.code}>{theme.accent}</span>
        {theme.accentAdjusted && (
          <>
            {" "}
            &mdash;{" "}
            {theme.accentAdjusted === "lighter" ? "lightened" : "darkened"} from{" "}
            <span className={styles.code}>
              {normalizeHex(cfg.accent) ?? cfg.accent}
            </span>{" "}
            so accent-coloured links and code still read against the dark
            background.
          </>
        )}
      </p>

      {state?.ok === false && <div className={styles.error}>{state.error}</div>}
      {state?.ok === true && !dirty && (
        <div className={styles.notice}>Theme saved.</div>
      )}

      <div className={own.actions}>
        <button type="submit" className={styles.btn} disabled={pending || !dirty}>
          {pending ? "Saving..." : "Save theme"}
        </button>
        {dirty && (
          <button
            type="button"
            className={styles.btnGhost}
            onClick={() => {
              setCfg(saved);
              setAccentText(saved.accent);
            }}
            disabled={pending}
          >
            Discard preview
          </button>
        )}
      </div>
    </form>
  );
}
