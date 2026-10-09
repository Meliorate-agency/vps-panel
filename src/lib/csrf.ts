/**
 * Cross-site request forgery guard for mutating requests, shared by
 * src/proxy.ts and the route the proxy skips (/api/files/upload). No
 * "server-only" import: the proxy bundle loads this too.
 *
 * Origin must be the host the browser addressed (nginx passes it through as
 * Host), or the configured panel host. Comparing only with PANEL_HOST refused
 * every login on a box reached by another name or address than the one in
 * .env (a NAT'd VPS whose PANEL_HOST is the private IP).
 *
 * Returns why the request is refused, or null when it may go on.
 */
export function originRefusal(headers: Headers): { status: 400 | 403; message: string } | null {
  const origin = headers.get("origin");
  if (!origin) return null;
  let originHost: string;
  try {
    originHost = new URL(origin).host.toLowerCase();
  } catch {
    return { status: 400, message: "invalid origin" };
  }
  const allowed = new Set(
    [
      headers.get("host"),
      headers.get("x-forwarded-host"),
      process.env.PANEL_HOST,
      process.env.PANEL_HOST ? `${process.env.PANEL_HOST}:10000` : null,
      "localhost:10000",
      "127.0.0.1:10000",
    ]
      .filter((h): h is string => !!h)
      .map((h) => h.toLowerCase()),
  );
  if (!allowed.has(originHost)) {
    return { status: 403, message: "origin mismatch" };
  }
  return null;
}
