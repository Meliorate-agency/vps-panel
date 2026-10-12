"use client";

import { useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import StatusBadge from "@/components/StatusBadge/StatusBadge";
import type { DockerImageStatus } from "@/lib/agent";
import styles from "@/components/page.module.css";

type UpdateResult = { ok: true; taskId: string } | { ok: false; error: string };

/**
 * The Updates cell of a Docker app on /apps: whether the registry has a newer
 * build of its image tag. "Update" pulls it and moves the app onto it (rolled
 * back if it does not come up), then opens the task log.
 */
export default function ImageStatusBadge({
  appName,
  status,
  updateAction,
}: {
  appName: string;
  /** undefined when the check failed outright. */
  status: DockerImageStatus | undefined;
  updateAction: (name: string) => Promise<UpdateResult>;
}) {
  const router = useRouter();
  const [pending, startTransition] = useTransition();
  const [error, setError] = useState<string | null>(null);

  if (!status || status.update_available === null) {
    return (
      <span className={styles.muted} title={status?.error ?? "could not check the registry"}>
        not checked
      </span>
    );
  }
  if (!status.update_available) {
    return (
      <span title={`${status.image} is the newest build of its tag`}>
        <StatusBadge text="up to date" tone="ok" />
      </span>
    );
  }
  return (
    <button
      type="button"
      className={styles.btnGhost}
      disabled={pending}
      title={error ?? `A newer build of ${status.image} is available: pull it and restart on it`}
      style={{ color: error ? "var(--danger, #ff4d6d)" : "var(--warn, #ffc400)", whiteSpace: "nowrap" }}
      onClick={() => {
        setError(null);
        startTransition(async () => {
          const r = await updateAction(appName);
          if (r.ok) router.push(`/tasks/${encodeURIComponent(r.taskId)}`);
          else setError(r.error);
        });
      }}
    >
      {pending ? "Starting…" : error ? "Update failed" : "Update available"}
    </button>
  );
}
