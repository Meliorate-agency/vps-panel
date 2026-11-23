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

      {/* Where the repository password lives (install.sh writes it; the
          agent reads RESTIC_PASSWORD from .env). Never the key itself. */}
      <div
        style={{
          marginBottom: 16,
          padding: "10px 12px",
          background: "rgba(255, 196, 0, 0.08)",
          border: "1px solid rgba(255, 196, 0, 0.28)",
          borderRadius: 6,
          fontSize: 13,
        }}
      >
        <strong>Keep a copy of the backup password off this server.</strong> Every snapshot is
        encrypted with one password, and it is stored only here: as{" "}
        <span className={styles.code}>RESTIC_PASSWORD</span> in{" "}
        <span className={styles.code}>/etc/vps-panel/.env</span>, in{" "}
        <span className={styles.code}>/etc/vps-panel/backup.key</span>, and inside the
        encrypted backups themselves. If this server is lost, nobody can open the backups
        without a copy kept somewhere else, such as your password manager. As root on the
        server: <span className={styles.code}>grep ^RESTIC_PASSWORD= /etc/vps-panel/.env</span>
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
