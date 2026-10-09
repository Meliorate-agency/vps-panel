"use client";

import { useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import TaskProgress from "@/components/TaskProgress/TaskProgress";
import styles from "@/components/page.module.css";
import type { RunBackupResult } from "@/server-actions/backups";

export default function BackupRunner({
  action,
}: {
  action: () => Promise<RunBackupResult>;
}) {
  const router = useRouter();
  const [pending, start] = useTransition();
  const [taskId, setTaskId] = useState<string | null>(null);
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  return (
    <div>
      <button
        className={styles.btn}
        // Stays disabled until the backup task ends, not just until the agent
        // accepted it; the snapshot list refreshes when it does.
        disabled={pending || running}
        onClick={() =>
          start(async () => {
            setError(null);
            const r = await action();
            if (!r.ok) {
              setError(r.error);
              return;
            }
            setTaskId(r.taskId);
            setRunning(true);
          })
        }
      >
        {pending ? "Starting..." : running ? "Backing up..." : "Run backup now"}
      </button>
      {error && <div className={styles.error}>{error}</div>}
      {taskId && (
        <TaskProgress
          key={taskId}
          taskId={taskId}
          onComplete={() => {
            setRunning(false);
            router.refresh();
          }}
        />
      )}
    </div>
  );
}
