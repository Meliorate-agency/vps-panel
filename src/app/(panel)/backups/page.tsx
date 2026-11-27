import Link from "next/link";
import { agent } from "@/lib/agent";
import Table from "@/components/Table/Table";
import styles from "@/components/page.module.css";
import { runBackupAction } from "@/server-actions/backups";
import BackupRunner from "./BackupRunner";

export const dynamic = "force-dynamic";

/**
 * Backups: restic copies of this server's files and databases, kept on
 * this server for getting things back. Restic calls each copy a
 * "snapshot"; the page says "backup" so it is never mistaken for a server
 * snapshot (the move to another server, /server-snapshots).
 */
export default async function BackupsPage() {
  const backups = await agent.backups.list().catch(() => []);
  return (
    <div>
      <div className={styles.head}>
        <div>
          <h1 className={styles.title}>Backups</h1>
          <div className={styles.subtitle}>
            Encrypted copies of this server&apos;s files and databases, for getting things back.
          </div>
        </div>
        <div className={styles.actions}>
          <BackupRunner action={runBackupAction} />
        </div>
      </div>

      <div className={styles.muted} style={{ fontSize: 13, marginBottom: 12 }}>
        Each backup holds the apps, Drive, mailboxes, the panel&apos;s settings and a dump of every database.
        Older ones are thinned out on their own (7 daily, 4 weekly, 6 monthly are kept). They are stored
        on this server, so they do not help if the server itself is lost. To move the whole server to
        another one, use <Link href="/server-snapshots">Server snapshots</Link>.
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
        <strong>Keep a copy of the backup password off this server.</strong> Every backup is
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
          { key: "id", label: "Backup" },
          { key: "time", label: "When" },
          { key: "paths", label: "Paths" },
          { key: "host", label: "Host", width: "150px" },
        ]}
        rows={backups.map((s) => ({
          id: <span className={styles.code}>{s.id}</span>,
          time: <span className={styles.muted}>{s.time}</span>,
          paths: <span className={styles.code}>{s.paths.join(", ")}</span>,
          host: s.hostname,
        }))}
        empty="No backups yet. Click 'Run backup now' to make one."
      />
    </div>
  );
}
