import { agent } from "@/lib/agent";
import styles from "@/components/page.module.css";
import ServicesView from "./ServicesView";
import { requireSession } from "@/lib/auth";

export const dynamic = "force-dynamic";

export default async function ServicesPage() {
  // Checked here, not only in the (panel) layout: a client-side navigation
  // renders the page without re-running the layout.
  await requireSession();
  const services = await agent.services.list().catch(() => []);

  return (
    <div>
      <div className={styles.head}>
        <div>
          <h1 className={styles.title}>System services</h1>
          <div className={styles.subtitle}>Core services this panel depends on.</div>
        </div>
      </div>

      <ServicesView services={services} />
    </div>
  );
}
