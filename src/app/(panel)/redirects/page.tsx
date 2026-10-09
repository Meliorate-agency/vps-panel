import { agent } from "@/lib/agent";
import { listRedirects } from "@/lib/redirects";
import Table from "@/components/Table/Table";
import StatusBadge from "@/components/StatusBadge/StatusBadge";
import ConfirmDeleteButton from "@/components/ConfirmDeleteButton/ConfirmDeleteButton";
import styles from "@/components/page.module.css";
import {
  createRedirectAction,
  deleteRedirectAction,
  updateRedirectAction,
} from "@/server-actions/redirects";
import RedirectForm from "./RedirectForm";
import EditRedirectRow from "./EditRedirectRow";
import local from "./redirects.module.css";

export const dynamic = "force-dynamic";

export default async function RedirectsPage() {
  // Only vhosts the panel wrote can carry redirect rules: the Sites tab's
  // `site-<domain>` files and app vhosts with the `# vps-app:` marker. The
  // panel host and hand-written vhosts are left alone (the agent enforces
  // this too). If the agent is down we still render the stored rules, just
  // without the "vhost missing" hint since we can't tell.
  let agentReachable = true;
  const vhosts = await agent.sites.list().catch(() => {
    agentReachable = false;
    return [];
  });
  const domains = vhosts
    .filter((v) => v.kind === "site" || v.kind === "app")
    .map((v) => v.domain)
    .sort();
  const servable = new Set(domains);
  const redirects = listRedirects();

  return (
    <div>
      <div className={styles.head}>
        <div>
          <h1 className={styles.title}>Redirects</h1>
          <div className={styles.subtitle}>
            301 and 302 rules for the domains this server hosts. Written straight into nginx and live on save.
          </div>
        </div>
      </div>

      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>Add redirect</h2>
        {domains.length === 0 ? (
          <p className={styles.muted}>
            {agentReachable
              ? "No panel-managed sites or apps yet. Add a domain on the Domains page first."
              : "The agent is not reachable, so the list of domains could not be loaded."}
          </p>
        ) : (
          <RedirectForm action={createRedirectAction} domains={domains} />
        )}
      </div>

      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>Active redirects</h2>
        <Table
          columns={[
            { key: "domain", label: "Domain" },
            { key: "from", label: "From" },
            { key: "to", label: "To" },
            { key: "type", label: "Type", width: "150px" },
            { key: "actions", label: "", width: "170px" },
          ]}
          rowKey={(_row, i) => redirects[i].id}
          rows={redirects.map((r) => ({
            domain: (
              <span className={local.stack}>
                <span className={styles.code}>{r.domain}</span>
                {agentReachable && !servable.has(r.domain) && (
                  <StatusBadge text="vhost missing" tone="warn" />
                )}
              </span>
            ),
            from: (
              <span className={local.stack}>
                <span className={styles.code}>{r.source_path}</span>
                {r.match_mode === "prefix" && (
                  <span className={local.hint}>and everything under it</span>
                )}
              </span>
            ),
            to: <span className={styles.code}>{r.target_url}</span>,
            type: (
              <StatusBadge
                text={r.code === 301 ? "301 permanent" : "302 temporary"}
                tone={r.code === 301 ? "ok" : "info"}
              />
            ),
            actions: (
              <div className={local.rowActions}>
                <EditRedirectRow
                  redirect={{
                    id: r.id,
                    domain: r.domain,
                    source_path: r.source_path,
                    target_url: r.target_url,
                    code: r.code,
                    match_mode: r.match_mode,
                  }}
                  action={updateRedirectAction.bind(null, r.id)}
                />
                <ConfirmDeleteButton
                  action={deleteRedirectAction.bind(null, r.id)}
                  title="Delete redirect?"
                  buttonLabel="Delete"
                  body={
                    <p>
                      Requests to{" "}
                      <code className={styles.code}>
                        {r.domain}
                        {r.source_path}
                      </code>{" "}
                      will stop redirecting to{" "}
                      <code className={styles.code}>{r.target_url}</code> as soon as nginx reloads.
                    </p>
                  }
                />
              </div>
            ),
          }))}
          empty="No redirects yet."
        />
      </div>
    </div>
  );
}
