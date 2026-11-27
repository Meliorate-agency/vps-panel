"use client";

import { useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";
import styles from "@/components/page.module.css";
import { updateAllAppsAction } from "@/server-actions/apps";

/**
 * "Update all" in the /apps header: the agent checks the apps one at a time
 * and updates each one with something new, never two at once. The queue's
 * progress is UpdateQueueStrip, below the header; the page refresh after
 * starting hands it the running queue.
 */
export default function UpdateAllButton({ running, appCount }: { running: boolean; appCount: number }) {
  const router = useRouter();
  const [open, setOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [pending, startTransition] = useTransition();

  const start = () => {
    setError(null);
    startTransition(async () => {
      const res = await updateAllAppsAction();
      if (!res.ok) {
        setError(res.error);
        return;
      }
      setOpen(false);
      router.refresh();
    });
  };

  return (
    <>
      <button
        type="button"
        className={styles.btnGhost}
        onClick={() => {
          setError(null);
          setOpen(true);
        }}
        disabled={running || appCount === 0}
        title={
          running
            ? "Update all is running - its progress is below"
            : appCount === 0
              ? "There are no apps to update"
              : "Check every app and update the ones with something new, one at a time"
        }
      >
        {running ? "Updating apps..." : "Update all"}
      </button>
      <ConfirmDialog
        open={open}
        title="Update all apps?"
        confirmLabel="Update all"
        pending={pending}
        onConfirm={start}
        onCancel={() => setOpen(false)}
      >
        <p style={{ margin: "0 0 8px" }}>
          The panel checks each of the {appCount} {appCount === 1 ? "app" : "apps"} for something new (new commits, or
          a newer image on its registry) and updates the ones that have it, one at a time, never two at once.
        </p>
        <p className={styles.muted} style={{ margin: 0 }}>
          Each app restarts when it is updated. One whose new version fails to build or does not come up is put back
          on its previous build, and the queue goes on with the next app. You can stop the queue after the app in
          progress.
        </p>
        {error && (
          <div className={styles.error} style={{ marginTop: 10 }}>
            {error}
          </div>
        )}
      </ConfirmDialog>
    </>
  );
}
