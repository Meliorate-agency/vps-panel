import Link from "next/link";
import DeployForm from "./DeployForm";
import { checkAppSourceStateAction, deployAppAction } from "@/server-actions/apps";
import { agent } from "@/lib/agent";
import { freeSlotsFor } from "@/lib/appSlots";
import styles from "@/components/page.module.css";

export const metadata = { title: "Deploy app" };

export const dynamic = "force-dynamic";

type PortSiteRow = {
  domain: string;
  upstream_port: number;
};

export default async function NewAppPage({
  searchParams,
}: {
  searchParams: Promise<{ method?: string }>;
}) {
  const sp = await searchParams;
  // method=github → ask for a repo URL; the agent will git-clone + npm
  // install into source_dir before starting the unit. No method param =
  // legacy flow (operator already put code into source_dir manually).
  const sourceMethod = sp.method === "github" ? "github" : "manual";

  // Unowned slots only: one bound to another app or a quick-install service
  // is never offered to a new app (the agent would 409 on the port anyway).
  const portSites: PortSiteRow[] = freeSlotsFor("").map((s) => ({
    domain: s.domain,
    upstream_port: s.port,
  }));

  // For the GitHub-source flow, surface the aliases the operator can use as
  // hostnames in `git@<alias>:owner/repo.git` URLs. The agent runs git as
  // root, so only root's ~/.ssh/<name> keypairs are usable. Filter to
  // keypair_name-bearing login-key rows — the panel-generated keypairs,
  // which have a Host block in ~/.ssh/config and a private key on disk. A
  // keypair put in ~/.ssh by hand (authorized: false) may have no Host block.
  const rootKeypairs =
    sourceMethod === "github"
      ? await agent.ssh
          .list("root")
          .then((rows) =>
            rows
              .filter((r) => r.authorized)
              .map((r) => r.keypair_name)
              .filter((n): n is string => !!n),
          )
          .catch(() => [])
      : [];

  return (
    <div>
      <Link href="/apps" className={styles.backArrow} aria-label="Back to apps">
        <svg width="16" height="16" viewBox="0 0 16 16" fill="none" aria-hidden="true">
          <path
            d="M10 3L5 8l5 5"
            stroke="currentColor"
            strokeWidth="1.8"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </svg>
      </Link>
      <div className={styles.head}>
        <div>
          <h1 className={styles.title}>Deploy app</h1>
          <div className={styles.subtitle}>
            {sourceMethod === "github" ? (
              <>
                Cloning code from a GitHub repository into{" "}
                <span className={styles.code}>/srv/apps/&lt;name&gt;</span>,
                then starting it as a systemd unit attached to a port-mode
                site from <Link href="/domains">/domains</Link>.
              </>
            ) : (
              <>
                Pick a port-mode site from <Link href="/domains">/domains</Link>{" "}
                and the panel will deploy a systemd unit that fills it.
              </>
            )}
          </div>
        </div>
      </div>
      <div className={styles.section}>
        <DeployForm
          action={deployAppAction}
          checkSourceState={checkAppSourceStateAction}
          portSites={portSites}
          sourceMethod={sourceMethod}
          rootKeypairs={rootKeypairs}
        />
      </div>
    </div>
  );
}
