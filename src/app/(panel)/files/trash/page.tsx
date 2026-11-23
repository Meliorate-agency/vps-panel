import Link from "next/link";
import { agent, type TrashRow } from "@/lib/agent";
import { requireSession } from "@/lib/auth";
import styles from "@/components/page.module.css";
import TrashTable from "./TrashTable";

export const dynamic = "force-dynamic";

/**
 * Everything deleted from /files lands here rather than being `rm`'d. The
 * agent's purge loop collects entries older than 30 days; until then a delete
 * is a mistake you can walk back.
 */
export default async function TrashPage() {
  // The names and paths of deleted files: admins only, like the listings.
  const s = await requireSession();
  if (s.role !== "admin") {
    return (
      <div className={styles.head}>
        <div>
          <h1 className={styles.title}>Trash</h1>
          <div className={styles.subtitle}>Only admins can browse the server&apos;s files.</div>
        </div>
      </div>
    );
  }
  let rows: TrashRow[];
  let error: string | null = null;
  try {
    rows = await agent.files.listTrash();
  } catch (e: unknown) {
    rows = [];
    error = e instanceof Error ? e.message : String(e);
  }

  return (
    <div>
      <Link href="/files" className={styles.backArrow} aria-label="Back to Files">
        ←
      </Link>

      <div className={styles.head}>
        <div>
          <h1 className={styles.title}>Trash</h1>
          <div className={styles.subtitle}>
            Items deleted from Files. Purged automatically 30 days after deletion.
          </div>
        </div>
      </div>

      {error && <div className={styles.error}>{error}</div>}

      <TrashTable rows={rows} />
    </div>
  );
}
