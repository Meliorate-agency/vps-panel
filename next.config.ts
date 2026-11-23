import type { NextConfig } from "next";
import { contentSecurityPolicy } from "./src/lib/csp";

const securityHeaders = [
  { key: "X-Content-Type-Options", value: "nosniff" },
  { key: "X-Frame-Options", value: "DENY" },
  { key: "Referrer-Policy", value: "same-origin" },
  { key: "Permissions-Policy", value: "camera=(), microphone=(), geolocation=()" },
  // The policy without a nonce: no inline script at all. It is what static
  // files, the tab icon, the upload route and API answers get. For a page,
  // src/proxy.ts replaces it with one that carries that response's nonce.
  { key: "Content-Security-Policy", value: contentSecurityPolicy() },
];

const nextConfig: NextConfig = {
  // Note: output: "standalone" is intentionally NOT set. Next.js 16's Turbopack
  // bundler silently ignores it (no .next/standalone/ is produced), and we run
  // the panel via `next start` from the app root instead. Re-enable only if/when
  // the build switches back to --webpack.
  reactStrictMode: true,
  poweredByHeader: false,
  serverExternalPackages: ["better-sqlite3", "@node-rs/argon2"],
  async headers() {
    return [{ source: "/:path*", headers: securityHeaders }];
  },
};

export default nextConfig;
