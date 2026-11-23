import { config } from "@/lib/config";
import { requireSession } from "@/lib/auth";
import { db } from "@/lib/db";
import { getSetting } from "@/lib/settings";
import { agent } from "@/lib/agent";
import { tailscaleCard } from "@/lib/tailscale";
import { getSetupState, getFail2banPolicy } from "@/server-actions/setup";
import { SETUP_STEPS } from "./steps";
import { SETUP_APP_CATALOG } from "./catalog";
import Wizard from "./Wizard";

export const dynamic = "force-dynamic";

export default async function SetupPage() {
  const session = await requireSession();
  const state = await getSetupState();
  // The Password step only lets Finish through once the installer's password
  // was changed. Remembered by the browser alone, a reload forgot it. Every
  // successful change writes an ok `password.change` audit row for the user
  // (changePasswordAction; audit rows are never deleted). Comparing the
  // stored hash with ADMIN_INITIAL_PASSWORD would not do: that only seeds
  // the first admin, so for anyone else it reads as "changed".
  const passwordChanged =
    db()
      .prepare(
        `SELECT 1 FROM audit_log
          WHERE actor_id = ? AND action = 'password.change' AND result = 'ok'
          LIMIT 1`,
      )
      .get(session.userId) !== undefined;
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
      passwordChanged={passwordChanged}
    />
  );
}
