import "server-only";
import { agent, type Ipv6NewDomains } from "@/lib/agent";
import { optionalString, type Tool } from "../types";

// Settings > DNS & SSL > IPv6 over MCP. One zone's switch is
// domains.ipv6 / domains.set_ipv6 in tools/domains.ts.

export const TOOLS: Tool[] = [
  {
    page: "Settings",
    name: "ipv6.status",
    description:
      "The server's IPv6 address that domains with IPv6 on publish: address (in effect; null = none), source (setting = chosen by the operator, hostname = the panel hostname's AAAA record, interface = the only public IPv6 address on the server, none), setting (stored choice; empty = automatic), candidates (public IPv6 addresses on the network interfaces, with device and note), new_domains (on or off: whether new domains get IPv6), domains_on / domains_total and notes. Fast.",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    call: async () => agent.ipv6.status(),
  },
  {
    page: "Settings",
    name: "ipv6.set",
    description:
      "Change the IPv6 settings; only the ones given are changed. address: one of the candidates from ipv6.status, or an empty string for automatic. Changing it moves the IPv6 records of every domain with IPv6 on to the new address; clearing it to no address while domains have IPv6 on is refused with a conflict. new_domains: on or off, whether domains added from now on get IPv6. Returns the new status.",
    inputSchema: {
      type: "object",
      properties: {
        address: {
          type: "string",
          description: "An IPv6 address on this server (from ipv6.status candidates), or empty for automatic.",
        },
        new_domains: { type: "string", enum: ["on", "off"], description: "IPv6 for domains added from now on." },
      },
      additionalProperties: false,
    },
    call: async (args) => {
      const address = optionalString(args, "address");
      const newDomains = optionalString(args, "new_domains");
      // The route does not enforce a schema's enum, so check it here.
      if (newDomains !== undefined && newDomains !== "on" && newDomains !== "off") {
        throw new Error("argument new_domains must be one of: on, off");
      }
      if (address === undefined && newDomains === undefined) {
        throw new Error("give address, new_domains or both");
      }
      return agent.ipv6.setSettings({
        ...(address !== undefined ? { address: address.trim() } : {}),
        ...(newDomains !== undefined ? { new_domains: newDomains as Ipv6NewDomains } : {}),
      });
    },
  },
];
