import { agent } from "@/lib/agent";
import { requireSession } from "@/lib/auth";
import styles from "@/components/page.module.css";
import LogTailer from "./LogTailer";

export const dynamic = "force-dynamic";

export default async function LogsPage({
  searchParams,
}: {
  searchParams: Promise<{ unit?: string }>;
}) {
  const s = await requireSession();
  if (s.role !== "admin") return <AdminsOnly />;
  const services = await agent.services.list().catch(() => []);
  const sp = await searchParams;
  const initial = sp?.unit;
  const unitOptions = services
    .map((s) => s.unit)
    .filter((u) => u.endsWith(".service"))
    .sort();
  return (
    <div>
      <div className={styles.head}>
        <div>
          <h1 className={styles.title}>Logs</h1>
          <div className={styles.subtitle}>Live tail from journald.</div>
        </div>
      </div>
      <LogTailer units={unitOptions} initial={initial} />
    </div>
  );
}

function AdminsOnly() {
  return (
    <div>
      <div className={styles.head}>
        <div>
          <h1 className={styles.title}>Logs</h1>
          <div className={styles.subtitle}>Only admins can read server logs.</div>
        </div>
      </div>
    </div>
  );
}
