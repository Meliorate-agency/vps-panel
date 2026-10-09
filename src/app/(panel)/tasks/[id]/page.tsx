import Link from "next/link";
import { notFound } from "next/navigation";
import { agent, AgentError, type TaskState } from "@/lib/agent";
import { requireSession } from "@/lib/auth";
import StatusBadge from "@/components/StatusBadge/StatusBadge";
import LiveTaskLog from "./LiveTaskLog";
import styles from "@/components/page.module.css";

export const dynamic = "force-dynamic";

export default async function TaskDetailPage({
  params,
}: {
  params: Promise<{ id: string }>;
}) {
  const { id } = await params;
  // Task logs can include env values and build output with secrets.
  const s = await requireSession();
  if (s.role !== "admin") {
    return (
      <div className={styles.head}>
        <div>
          <h1 className={styles.title}>Task</h1>
          <div className={styles.subtitle}>Only admins can read task logs.</div>
        </div>
      </div>
    );
  }

  let task: TaskState;
  try {
    task = await agent.tasks.get(id);
  } catch (e) {
    if (e instanceof AgentError && e.status === 404) notFound();
    throw e;
  }

  const isLive = task.status === "pending" || task.status === "running";

  return (
    <div>
      <Link href="/tasks" className={styles.backArrow} aria-label="Back to tasks">
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
          <h1 className={styles.title}>
            <span className={styles.code}>{task.kind}</span>
          </h1>
          <div className={styles.subtitle}>
            <StatusBadge text={task.status} />{" "}
            <span>started {task.started_at}</span>
            {task.finished_at && <span> &middot; finished {task.finished_at}</span>}
          </div>
        </div>
      </div>

      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>Log</h2>
        {isLive ? (
          <LiveTaskLog taskId={task.id} />
        ) : (
          <pre className={styles.log}>
            {task.log.length > 0 ? task.log.join("\n") : "(no output)"}
          </pre>
        )}
      </div>
    </div>
  );
}
