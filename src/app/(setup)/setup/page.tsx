import { config } from "@/lib/config";
import { getSetting } from "@/lib/settings";
import { agent } from "@/lib/agent";
import { tailscaleCard } from "@/lib/tailscale";
import { getSetupState, getFail2banPolicy } from "@/server-actions/setup";
import { SETUP_STEPS } from "./steps";
import { SETUP_APP_CATALOG } from "./catalog";
import Wizard from "./Wizard";

export const dynamic = "force-dynamic";

export default async function SetupPage() {
  const state = await getSetupState();
  const acmeEmail = getSetting("panel.acme_email", "");
  const nsPair = getSetting("dns.ns_pair", "");
  const policy = await getFail2banPolicy();
  const initialServices = await agent.system.services().catch(() => []);
  const pdnsZones = await agent.dns.listZones().catch(() => []);
  const recipes = await agent.quickInstall.list().catch(() => []);
  // As on /apps, only the phase and the tailnet IP reach the client; the
  // Tailscale dialog fetches the login link itself, admins only.
  const tailscale = recipes.some((r) => r.slug === "tailscale" && r.installed)
    ? tailscaleCard(await agent.tailscale.status().catch(() => null))
    : null;
  const sshConfig = await agent.ssh.getConfig().catch(() => null);
  const rootKeyCount = await agent.ssh
    .list("root")
    .then((rows) => rows.length)
    .catch(() => 0);

  return (
    <Wizard
      steps={[...SETUP_STEPS]}
      initialStep={state.step}
      panelHost={config.panelHost}
      adminEmail={config.bootstrapEmail ?? ""}
      acmeEmail={acmeEmail}
      nsPair={nsPair}
      policy={policy}
      catalog={[...SETUP_APP_CATALOG]}
      installedApps={recipes.filter((r) => r.installed).map((r) => r.slug)}
      initialServices={initialServices}
      managedZones={pdnsZones.map((z) => z.name)}
      sshConfig={sshConfig}
      rootKeyCount={rootKeyCount}
      tailscale={tailscale}
    />
  );
}
