import { config } from "@/lib/config";
import { getServerAddress } from "@/lib/serverAddress";
import { getMcpTokenInfo } from "@/lib/mcpToken";
import { listToolsByPage } from "@/lib/mcp/registry";
import type { Tool } from "@/lib/mcp/types";
import styles from "@/components/page.module.css";
import local from "./page.module.css";
import ConnectionInfo from "./ConnectionInfo";
import ToolPreview from "./ToolPreview";

export const metadata = { title: "MCP" };
export const dynamic = "force-dynamic";

/**
 * Strip the Tool's `call` function before shipping the registry to the
 * client (server actions / functions don't cross the RSC boundary). We
 * also lose the now-unused property so the type lines up with the
 * preview component's prop.
 */
type PreviewTool = Omit<Tool, "call">;

export default async function AiAgentPage() {
  // The token is stored only as a hash, so the config snippet carries a
  // placeholder for it.
  const resolved = getMcpTokenInfo();
  const token = resolved ? "<YOUR_MCP_TOKEN>" : null;
  const tokenSource = resolved?.source ?? null;
  const address = await getServerAddress().catch(() => ({
    domain: null as string | null,
    ip: null as string | null,
  }));
  const host = address.domain ?? address.ip ?? "<panel-host>";
  const scheme = config.cookieSecure ? "https" : "http";
  const endpoint = `${scheme}://${host}/api/mcp`;

  const groups = listToolsByPage().map((g) => ({
    page: g.page,
    tools: g.tools.map<PreviewTool>(({ call: _call, ...rest }) => rest),
  }));
  const totalTools = groups.reduce((acc, g) => acc + g.tools.length, 0);
  const readOnlyTools = groups.reduce((acc, g) => acc + g.tools.filter((t) => t.readOnly).length, 0);

  return (
    <div>
      <div className={styles.head}>
        <div>
          <h1 className={styles.title}>MCP</h1>
          <div className={styles.subtitle}>
            Expose panel operations as agentic tools an external AI client
            (Claude Desktop, Cursor, custom GPTs, …) can call over the
            Model Context Protocol.
          </div>
        </div>
      </div>

      <ConnectionInfo
        enabled={token !== null}
        endpoint={endpoint}
        token={token}
        tokenSource={tokenSource}
      />

      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>Exposed tools</h2>
        <div className={local.toolsSummary}>
          {totalTools} tool{totalTools === 1 ? "" : "s"} across{" "}
          {groups.length} page{groups.length === 1 ? "" : "s"}. A read-only token sees the{" "}
          {readOnlyTools} tools that list and check status. A full-access token also gets the
          rest: deploy, rebuild, start, stop and restart apps, edit app env files, open and
          close firewall ports, change DNS records and mailbox passwords. No tool deletes
          anything or touches SSH keys. Treat a full-access token like a root password.
        </div>
        <ToolPreview groups={groups} />
      </div>
    </div>
  );
}
