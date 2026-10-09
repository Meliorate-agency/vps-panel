import type { Metadata, Viewport } from "next";
import { getTabTitle, getThemeConfig } from "@/lib/accent";
import { deriveTheme, themeVars } from "@/lib/theme";
import "./globals.css";

export const viewport: Viewport = {
  width: "device-width",
  initialScale: 1,
};

/** `/icon/<glow>-<accent>-<deep>.svg` for the current theme. */
function iconHref(): string {
  const t = deriveTheme(getThemeConfig());
  const hex = (v: string) => v.replace("#", "").toLowerCase();
  return `/icon/${hex(t.accentGlow)}-${hex(t.accent)}-${hex(t.accent2)}.svg`;
}

// The tab icon follows the operator's accent: the colour is in its URL, so a
// theme change points the browser at a different file instead of leaving a
// cached favicon in the old colour behind. The tab name is Settings >
// Appearance's; a page with its own title shows "<title> - <name>".
export async function generateMetadata(): Promise<Metadata> {
  const name = getTabTitle();
  return {
    title: { default: name, template: `%s - ${name}` },
    description: "Self-hosted VPS control panel",
    robots: { index: false, follow: false },
    icons: { icon: [{ url: iconHref(), type: "image/svg+xml" }] },
  };
}

export default async function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode;
}>) {
  // The accent-derived palette rides on <html>'s style attribute rather than a
  // <style> block: an inline style always outranks the :root defaults in
  // globals.css, whichever order Next flushes the stylesheet link and the
  // markup in. See src/lib/theme.ts.
  const theme = themeVars(deriveTheme(getThemeConfig()));
  return (
    <html lang="en" style={theme as React.CSSProperties}>
      <body>{children}</body>
    </html>
  );
}
