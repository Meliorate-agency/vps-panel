/**
 * The panel's Content-Security-Policy. One list for both places that send
 * it, so they cannot drift apart:
 *
 * - src/proxy.ts, for every page: a fresh nonce per request. Next reads it
 *   from the request's copy of the header and puts it on the scripts it
 *   emits; an injected <script> has no nonce and does not run.
 * - next.config.ts, without a nonce, for what the proxy does not handle
 *   (static files, the tab icon, the upload route) and for API answers: no
 *   inline script is allowed there at all.
 *
 * Plain module: next.config.ts loads it outside the app.
 */
export function contentSecurityPolicy(nonce?: string): string {
  const dev = process.env.NODE_ENV !== "production";
  const script = [
    "script-src 'self'",
    // 'strict-dynamic': the chunks Next's own (nonced) scripts load later
    // are trusted through them. Browsers that know it ignore 'self' then.
    ...(nonce ? [`'nonce-${nonce}'`, "'strict-dynamic'"] : []),
    // React's dev build uses eval for its error overlays; production never.
    ...(dev ? ["'unsafe-eval'"] : []),
  ].join(" ");
  return [
    "default-src 'self'",
    script,
    // Inline styles are everywhere in the app (style attributes, the theme
    // on <html>), so styles stay as they were.
    "style-src 'self' 'unsafe-inline'",
    "img-src 'self' data: blob:",
    "font-src 'self'",
    "connect-src 'self'",
    "frame-ancestors 'none'",
    "base-uri 'self'",
    "form-action 'self'",
  ].join("; ");
}

/** A nonce for one response: 128 random bits, base64. */
export function newNonce(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  return btoa(String.fromCharCode(...bytes));
}
