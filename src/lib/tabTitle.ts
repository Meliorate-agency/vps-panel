/**
 * `panel.tab_title`: the panel's name in browser tabs, bookmarks and
 * history. Pages put their own title in front ("Sign in - <name>") through
 * the root layout's title template. Empty = DEFAULT_TAB_TITLE. Plain module:
 * the Appearance form, its server action and the root layout share it.
 */
export const TAB_TITLE_KEY = "panel.tab_title";
export const DEFAULT_TAB_TITLE = "Bolt Panel";
export const MAX_TAB_TITLE = 60;

/** The name to show for a stored value. */
export function tabTitleOrDefault(stored: string): string {
  return stored.trim() || DEFAULT_TAB_TITLE;
}

/** The value to store for what was typed ("" = the default), or what is wrong with it. */
export function tabTitleValue(input: string): { value: string } | { error: string } {
  const name = input.replace(/\s+/g, " ").trim();
  if (name.length > MAX_TAB_TITLE) {
    return { error: `Keep the name to ${MAX_TAB_TITLE} characters; browsers cut tab titles short anyway.` };
  }
  if (/[\u0000-\u001f\u007f]/.test(name)) return { error: "The name can't contain control characters." };
  // The title template swaps every %s for the page's own title.
  if (name.includes("%s")) return { error: "Leave out %s: the page title template uses it as a placeholder." };
  return { value: name === DEFAULT_TAB_TITLE ? "" : name };
}
