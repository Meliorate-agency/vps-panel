import "server-only";

function required(name: string): string {
  const v = process.env[name];
  if (!v) throw new Error(`env ${name} is not set`);
  return v;
}

function optional(name: string, fallback: string): string {
  return process.env[name] ?? fallback;
}

export const config = {
  get jwtSecret(): string {
    return required("JWT_SECRET");
  },
  get agentToken(): string {
    return required("AGENT_TOKEN");
  },
  get agentSocket(): string {
    return optional("AGENT_SOCKET", "/run/vps-panel/agent.sock");
  },
  get panelDbPath(): string {
    return optional("PANEL_DB_PATH", "/var/lib/vps-panel/panel.db");
  },
  // Panel host can be empty in "IP-only" mode (operator hasn't picked a real
  // FQDN yet). Don't throw — let callers decide whether they need a hostname.
  get panelHost(): string {
    return optional("PANEL_HOST", "");
  },
  get cookieSecure(): boolean {
    return optional("COOKIE_SECURE", "1") !== "0";
  },
  get bootstrapEmail(): string | null {
    return process.env.ADMIN_EMAIL ?? null;
  },
  get bootstrapPassword(): string | null {
    return process.env.ADMIN_INITIAL_PASSWORD ?? null;
  },
  /**
   * Bearer token external MCP clients (Claude Desktop, Cursor, ChatGPT
   * custom GPTs, …) present at `/api/mcp`. Null when unset → the MCP
   * endpoint returns 503 and the /ai-agent preview UI shows a disabled
   * status pill. Set in `/etc/vps-panel/.env` and restart the panel.
   */
  get mcpToken(): string | null {
    const v = process.env.MCP_TOKEN;
    return v && v.length > 0 ? v : null;
  },
};

export const COOKIE_NAME = "vps_panel_session";
export const REFRESH_COOKIE = "vps_panel_refresh";
export const ACCESS_TTL_SECONDS = 60 * 60;
export const REFRESH_TTL_SECONDS = 60 * 60 * 24 * 7;
