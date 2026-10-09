import { agent } from "@/lib/agent";
import Table from "@/components/Table/Table";
import styles from "@/components/page.module.css";
import { runBackupAction } from "@/server-actions/backups";
import BackupRunner from "./BackupRunner";

export const dynamic = "force-dynamic";

export default async function BackupsPage() {
  const snapshots = await agent.backups.list().catch(() => []);
  return (
    <div>
      <div className={styles.head}>
        <div>
          <h1 className={styles.title}>Backups</h1>
          <div className={styles.subtitle}>restic-managed encrypted snapshots.</div>
        </div>
        <div className={styles.actions}>
          <BackupRunner action={runBackupAction} />
        </div>
      </div>

      <Table
        columns={[
          { key: "id", label: "Snapshot ID" },
          { key: "time", label: "When" },
          { key: "paths", label: "Paths" },
          { key: "host", label: "Host", width: "150px" },
        ]}
        rows={snapshots.map((s) => ({
          id: <span className={styles.code}>{s.id}</span>,
          time: <span className={styles.muted}>{s.time}</span>,
          paths: <span className={styles.code}>{s.paths.join(", ")}</span>,
          host: s.hostname,
        }))}
        empty="No snapshots yet. Click 'Run backup now' to create one."
      />
    </div>
  );
}
