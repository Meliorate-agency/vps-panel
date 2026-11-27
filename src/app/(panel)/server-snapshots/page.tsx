import Link from "next/link";
import { agent } from "@/lib/agent";
import { db } from "@/lib/db";
import styles from "@/components/page.module.css";
import { listLinks } from "@/lib/serverSnapshots/links";
import ServerSnapshots from "./ServerSnapshots";
import { requireSession } from "@/lib/auth";

export const dynamic = "force-dynamic";

/**
 * True while this panel hosts nothing yet: the only time an import can run
 * (the agent checks it properly before importing; this only decides
 * whether the Import buttons are shown).
 */
function panelIsFresh(): boolean {
  const count = (table: string) => {
    try {
      return (db().prepare(`SELECT COUNT(*) AS c FROM ${table}`).get() as { c: number }).c;
    } catch {
      return 0;
    }
  };
  return count("apps") === 0 && count("sites") === 0 && count("redirects") === 0;
}

export default async function ServerSnapshotsPage() {
  // Checked here, not only in the (panel) layout: a client-side navigation
  // renders the page without re-running the layout.
  await requireSession();
  const list = await agent.serverSnapshots.list().catch(() => ({ snapshots: [], free_bytes: null }));
  const fresh = panelIsFresh();
  return (
    <div>
      <div className={styles.head}>
        <div>
          <h1 className={styles.title}>Server snapshots</h1>
          <div className={styles.subtitle}>Move this whole server to another one.</div>
        </div>
      </div>

      <div className={styles.section} style={{ marginBottom: 16, fontSize: 13 }}>
        <div style={{ marginBottom: 8 }}>
          A server snapshot is one encrypted file with everything this server hosts: apps, sites,
          certificates, every database, DNS zones, mailboxes, cron jobs, SSH keys, Docker volumes,
          accounts, settings and, if you choose, Drive. It is locked with a passphrase you choose; without
          it nobody can open the file, you included.
        </div>
        <ol style={{ margin: "0 0 8px 18px", padding: 0 }} className={styles.muted}>
          <li>On this server: make a snapshot, then make a link for it.</li>
          <li>
            On the new server (a freshly installed panel): Pull from another server, paste the link, then
            Import. The new server becomes this one, its DNS moved to its own address.
          </li>
        </ol>
        <div className={styles.muted}>
          For getting files or databases back on <strong>this</strong> server, use{" "}
          <Link href="/backups">Backups</Link> instead.
        </div>
      </div>

      <ServerSnapshots snapshots={list.snapshots} links={listLinks()} fresh={fresh} />
    </div>
  );
}
