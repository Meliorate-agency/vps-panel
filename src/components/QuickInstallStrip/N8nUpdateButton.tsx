"use client";

import { startTransition, useActionState, useState } from "react";
import { useRouter } from "next/navigation";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";
import TaskProgress from "@/components/TaskProgress/TaskProgress";
import shared from "@/components/page.module.css";
import styles from "./QuickInstallStrip.module.css";
import type { UpdateN8nResult } from "@/server-actions/quickInstall";

type Props = {
  updateAction: (
    prev: UpdateN8nResult | null,
    fd: FormData,
  ) => Promise<UpdateN8nResult>;
};

/**
 * "Update available" affordance for the n8n quick-install card. The agent's
 * /quick-install list flags n8n when the registry serves a newer n8nio/n8n
 * image than the running container; this pill renders top-right of the card
 * only then. Click → confirm → pull the latest image + recreate the container
 * (workflows / credentials in the n8n_data volume preserved, public-URL env +
 * bind kept), with the docker log streamed live. On success the page refreshes
 * so the badge disappears.
 */
export default function N8nUpdateButton({ updateAction }: Props) {
  const router = useRouter();
  const [open, setOpen] = useState(false);
  const [state, formAction, pending] = useActionState<
    UpdateN8nResult | null,
    FormData
  >(updateAction, null);

  // useActionState keeps the last result. Reopening after a failed update
  // showed that task's log and no Confirm, so no retry until a reload: the
  // dialog now opens past a finished task (and past an error), and a failed
  // task offers Try again. A task still running stays on show, so a second
  // click cannot start a second update.
  const [openedWith, setOpenedWith] = useState<UpdateN8nResult | null>(null);
  const [finished, setFinished] = useState<{
    taskId: string;
    status: "succeeded" | "failed";
  } | null>(null);
  const result = state !== openedWith ? state : null;
  const taskId = result?.ok ? result.data.taskId : null;

  // Once the task is accepted we hand the dialog over to the streaming log +
  // our own Close button, so hide ConfirmDialog's built-in action row.
  const started = !!taskId;

  return (
    <>
      <button
        type="button"
        className={styles.updateBadge}
        onClick={() => {
          const running = state?.ok === true && state.data.taskId !== finished?.taskId;
          if (!running) setOpenedWith(state);
          setOpen(true);
        }}
        title="Update available — pull the latest n8n and restart"
        aria-label="n8n update available"
      >
        <svg width="11" height="11" viewBox="0 0 24 24" fill="none" aria-hidden="true">
          <path
            d="M12 16V4M12 4 7 9M12 4l5 5"
            stroke="currentColor"
            strokeWidth="2.2"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
          <path
            d="M5 20h14"
            stroke="currentColor"
            strokeWidth="2.2"
            strokeLinecap="round"
          />
        </svg>
        Update
      </button>

      <ConfirmDialog
        open={open}
        title="Update n8n?"
        confirmLabel="Update & restart"
        cancelLabel="Cancel"
        pending={pending}
        hideActions={started}
        lockDismiss={pending}
        // Inside a transition, or pending never turns true and a second
        // click starts a second update.
        onConfirm={() => startTransition(() => formAction(new FormData()))}
        onCancel={() => {
          if (pending) return;
          setOpen(false);
        }}
      >
        <p style={{ marginTop: 0 }}>
          A newer <span className={shared.code}>n8nio/n8n</span> image is
          available. This pulls the latest image and recreates the container.
          Your workflows, credentials, and database in the{" "}
          <span className={shared.code}>n8n_data</span> volume are preserved,
          and the current public-URL settings + port binding are kept.
        </p>

        {result?.ok === false && (
          <div className={shared.error} style={{ marginBottom: 10 }}>
            {result.error}
          </div>
        )}

        {taskId && (
          <>
            <div style={{ marginTop: 14 }}>
              <TaskProgress
                key={taskId}
                taskId={taskId}
                onComplete={(s) => {
                  setFinished({ taskId, status: s });
                  if (s === "succeeded") router.refresh();
                }}
              />
            </div>
            <div
              style={{
                display: "flex",
                justifyContent: "flex-end",
                gap: 8,
                marginTop: 12,
              }}
            >
              {finished?.taskId === taskId && finished.status === "failed" && (
                <button
                  type="button"
                  className={shared.btnGhost}
                  onClick={() => setOpenedWith(state)}
                >
                  Try again
                </button>
              )}
              <button
                type="button"
                className={shared.btnGhost}
                onClick={() => setOpen(false)}
              >
                Close
              </button>
            </div>
          </>
        )}
      </ConfirmDialog>
    </>
  );
}
