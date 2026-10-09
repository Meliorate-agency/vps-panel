import Link from "next/link";
import { agent } from "@/lib/agent";
import styles from "@/components/page.module.css";
import fileStyles from "./files.module.css";

export const dynamic = "force-dynamic";

/**
 * The roots screen — deliberately NOT a listing of `/`.
 *
 * The agent runs as root, so a file manager that opens at the filesystem root
 * is a root file manager one mis-drop away from an unbootable box. Instead we
 * land on the handful of directories the panel actually owns, each of which is
 * a jail the agent enforces. `/` is still reachable, as the last card, marked
 * for what it is.
 */
export default async function FilesPage() {
  const roots = await agent.files.roots().catch(() => []);
  const trashCount = await agent.files
    .listTrash()
    .then((r) => r.length)
    .catch(() => 0);

  // The escape hatch renders apart from the everyday roots so it reads as a
  // deliberate choice rather than the seventh item in a list.
  const normal = roots.filter((r) => r.id !== "fs");
  const whole = roots.find((r) => r.id === "fs");

  return (
    <div>
      <div className={styles.head}>
        <div>
          <h1 className={styles.title}>Files</h1>
          <div className={styles.subtitle}>
            Browse, organise and clean up the directories this server runs on.
          </div>
        </div>
        <div className={styles.actions}>
          <Link href="/files/trash" className={styles.btnGhost}>
            Trash{trashCount > 0 ? ` (${trashCount})` : ""}
          </Link>
        </div>
      </div>

      {roots.length === 0 && (
        <div className={styles.empty}>
          Couldn&apos;t reach the agent. Check that vps-panel-agent.service is running.
        </div>
      )}

      <div className={fileStyles.rootGrid}>
        {normal.map((r) => (
          <RootCard key={r.id} root={r} />
        ))}
      </div>

      {whole && (
        <div className={fileStyles.escapeHatch}>
          <div className={fileStyles.escapeText}>
            <div className={fileStyles.escapeTitle}>{whole.label}</div>
            <div className={fileStyles.escapeDesc}>
              Browse from <span className={styles.code}>/</span>. System paths that hold
              secrets or hold the box together — <span className={styles.code}>/usr</span>,{" "}
              <span className={styles.code}>/boot</span>,{" "}
              <span className={styles.code}>/etc/vps-panel</span>,{" "}
              <span className={styles.code}>/etc/letsencrypt</span> and friends — stay
              locked: no rename, no move, no delete.
            </div>
          </div>
          <Link href={`/files/${whole.id}`} className={styles.btnGhost}>
            Open /
          </Link>
        </div>
      )}
    </div>
  );
}

function RootCard({
  root,
}: {
  root: { id: string; label: string; path: string; description: string; writable: boolean; exists: boolean };
}) {
  const body = (
    <>
      <div className={fileStyles.rootIcon} aria-hidden="true">
        <svg width="20" height="20" viewBox="0 0 16 16" fill="none">
          <path
            d="M2 4.5A1.5 1.5 0 0 1 3.5 3h2.69a1.5 1.5 0 0 1 1.06.44l.78.78a1.5 1.5 0 0 0 1.06.44H12.5A1.5 1.5 0 0 1 14 6.16V11.5A1.5 1.5 0 0 1 12.5 13h-9A1.5 1.5 0 0 1 2 11.5v-7Z"
            stroke="currentColor"
            strokeWidth="1.4"
          />
        </svg>
      </div>
      <div className={fileStyles.rootLabel}>
        {root.label}
        {!root.writable && <span className={fileStyles.pill}>read-only</span>}
        {!root.exists && <span className={fileStyles.pillDim}>not present</span>}
      </div>
      <div className={fileStyles.rootPath}>{root.path}</div>
      <div className={fileStyles.rootDesc}>{root.description}</div>
    </>
  );

  if (!root.exists) {
    return (
      <div className={[fileStyles.rootCard, fileStyles.rootCardDisabled].join(" ")}>{body}</div>
    );
  }
  return (
    <Link href={`/files/${root.id}`} className={fileStyles.rootCard}>
      {body}
    </Link>
  );
}
