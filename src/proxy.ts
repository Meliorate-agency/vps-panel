import { NextResponse } from "next/server";
import type { NextRequest } from "next/server";
import { jwtVerify } from "jose";
import { originRefusal } from "@/lib/csrf";

// Next 16 runs `proxy.ts` only when it sits next to the `app` directory. This
// file used to be at the repo root while the app lives in `src/app`, so none
// of it ran.

const COOKIE_NAME = "vps_panel_session";
// `/api/mcp` is the MCP server endpoint consumed by external AI clients
// (Claude Desktop, Cursor, ...). It authenticates with a bearer token inside
// the route handler, not the cookie + JWT flow of the rest of the panel.
const PUBLIC_PATHS = ["/login", "/totp", "/api/auth/refresh", "/api/mcp"];

export async function proxy(req: NextRequest) {
  const { pathname } = req.nextUrl;

  // Cross-site request forgery: a browser always sends Origin on a
  // cross-site POST, so a foreign one is refused here for every route.
  if (isMutating(req.method)) {
    const refused = originRefusal(req.headers);
    if (refused) return new NextResponse(refused.message, { status: refused.status });
  }

  if (PUBLIC_PATHS.includes(pathname)) return NextResponse.next();

  // Server actions and API routes check the session themselves and answer in
  // a form their callers understand (a redirect() the client follows, a 401).
  // A redirect from here would break both.
  if (req.headers.has("next-action") || pathname.startsWith("/api/")) {
    return NextResponse.next();
  }

  const token = req.cookies.get(COOKIE_NAME)?.value;
  if (!token) return redirectToLogin(req);

  const secret = process.env.JWT_SECRET;
  if (!secret) {
    return new NextResponse("server misconfigured: JWT_SECRET missing", { status: 500 });
  }

  try {
    const { payload } = await jwtVerify(token, new TextEncoder().encode(secret));
    if (payload.typ !== "access") return redirectToLogin(req);
    if (payload.needsTotp && pathname !== "/totp") {
      return NextResponse.redirect(new URL("/totp", req.url));
    }
    return NextResponse.next();
  } catch {
    return redirectToLogin(req);
  }
}

function redirectToLogin(req: NextRequest) {
  const url = new URL("/login", req.url);
  url.searchParams.set("from", req.nextUrl.pathname);
  const res = NextResponse.redirect(url);
  res.cookies.delete(COOKIE_NAME);
  return res;
}

function isMutating(method: string): boolean {
  return method === "POST" || method === "PUT" || method === "PATCH" || method === "DELETE";
}

export const config = {
  // `icon/` is the themed tab icon (src/app/icon/[stops]/route.ts): the
  // login page has to be able to load it without a session.
  // `api/files/upload` streams files of up to 5 GB. Next copies the body of
  // every request the proxy sees into memory and keeps only the first 10 MB
  // (proxyClientMaxBodySize), so while it matched here every upload was cut
  // to 10 MB and saved that way. The route checks Origin and the session
  // itself.
  matcher: [
    "/((?!_next/static|_next/image|favicon.ico|icon.svg|icon/|robots.txt|sitemap.xml|api/files/upload).*)",
  ],
};
