/**
 * The browser-tab icon, in the operator's accent colour.
 *
 * The three gradient stops are in the path (`/icon/<glow>-<accent>-<deep>.svg`)
 * rather than read here, so saving a new theme changes the URL: browsers
 * cache a favicon hard, and a fixed path would keep showing the old colour
 * until the cache was cleared. The root layout builds the URL from the
 * derived theme, and each colour's icon is then immutable.
 */
const STOPS_RE = /^([0-9a-f]{6})-([0-9a-f]{6})-([0-9a-f]{6})\.svg$/;

export const runtime = "nodejs";

export async function GET(_req: Request, { params }: { params: Promise<{ stops: string }> }) {
  const { stops } = await params;
  const m = STOPS_RE.exec(decodeURIComponent(stops).toLowerCase());
  if (!m) return new Response("not an icon", { status: 404 });
  const [, glow, accent, deep] = m;
  // Same mark as components/BoltMark and the sidebar logo.
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
  <defs>
    <linearGradient id="b" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="#${glow}"/>
      <stop offset="50%" stop-color="#${accent}"/>
      <stop offset="100%" stop-color="#${deep}"/>
    </linearGradient>
  </defs>
  <path d="M13 2L3 14h7l-1 8 11-12h-7l1-8z" fill="url(#b)"/>
</svg>
`;
  return new Response(svg, {
    headers: {
      "content-type": "image/svg+xml; charset=utf-8",
      // The colour is in the URL, so this file never changes.
      "cache-control": "public, max-age=31536000, immutable",
    },
  });
}
