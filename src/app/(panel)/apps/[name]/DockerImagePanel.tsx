"use client";

import { useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import StatusBadge from "@/components/StatusBadge/StatusBadge";
import type { DockerImageStatus } from "@/lib/agent";
import styles from "@/components/page.module.css";

type StatusResult = { ok: true; data?: DockerImageStatus } | { ok: false; error: string };
type UpdateResult = { ok: true; taskId: string } | { ok: false; error: string };

function shortId(id: string | null): string {
  return id ? id.replace(/^sha256:/, "").slice(0, 12) : "-";
}

/** A Docker app built from Git: what it builds and which commits run. */
type BuildInfo = {
  context: string;
  dockerfile: string;
  currentCommit: string | null;
  previousCommit: string | null;
};

/**
 * "Image" section of a Docker app's page. Check asks the registry whether
 * the image's tag points at a newer build (one network call, on demand so
 * the page never waits on a registry). Update pulls the tag again and moves
 * the app onto it; the agent rolls back to the previous image when the new
 * one does not come up. Either way the operator watches the task log.
 *
 * An app built from Git (`build`) shows its running build and commit
 * instead; its updates are new commits, handled by the Updates section.
 */
export default function DockerImagePanel({
  appName,
  image,
  currentId,
  previousId,
  build,
  canManage,
  checkAction,
  updateAction,
}: {
  appName: string;
  image: string;
  currentId: string | null;
  previousId: string | null;
  build: BuildInfo | null;
  canManage: boolean;
  checkAction: (name: string) => Promise<StatusResult>;
  updateAction: (name: string) => Promise<UpdateResult>;
}) {
  const router = useRouter();
  const [checking, startCheck] = useTransition();
  const [updating, startUpdate] = useTransition();
  const [status, setStatus] = useState<DockerImageStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const pinned = image.includes("@");

  const check = () => {
    setError(null);
    startCheck(async () => {
      const r = await checkAction(appName);
      if (r.ok && r.data) setStatus(r.data);
      else if (!r.ok) setError(r.error);
    });
  };

  const update = () => {
    setError(null);
    startUpdate(async () => {
      const r = await updateAction(appName);
      if (r.ok) router.push(`/tasks/${encodeURIComponent(r.taskId)}`);
      else setError(r.error);
    });
  };

  if (build) {
    const commit = (c: string | null) => (c ? <> from commit <span className={styles.code}>{c.slice(0, 7)}</span></> : null);
    return (
      <div style={{ display: "grid", gridTemplateColumns: "max-content 1fr", gap: "6px 14px", fontSize: 13 }}>
        <span className={styles.muted}>Builds</span>
        <span>
          <span className={styles.code}>{build.dockerfile}</span> in{" "}
          <span className={styles.code}>{build.context === "." ? "the repository root" : build.context}</span>
        </span>
        <span className={styles.muted}>Running</span>
        <span>
          {currentId ? (
            <>
              <span className={styles.code}>{shortId(currentId)}</span>
              {commit(build.currentCommit)}
            </>
          ) : (
            <span className={styles.muted}>no build yet: Save and redeploy below builds one</span>
          )}
        </span>
        <span className={styles.muted}>Rollback</span>
        <span>
          {previousId ? (
            <>
              <span className={styles.code}>{shortId(previousId)}</span>
              {commit(build.previousCommit)}
              <span className={styles.muted}> - a rebuild that does not come up returns to it</span>
            </>
          ) : (
            <span className={styles.muted}>none yet (kept from the first rebuild on)</span>
          )}
        </span>
      </div>
    );
  }

  return (
    <div>
      <div style={{ display: "grid", gridTemplateColumns: "max-content 1fr", gap: "6px 14px", fontSize: 13 }}>
        <span className={styles.muted}>Image</span>
        <span className={styles.code} style={{ wordBreak: "break-all" }}>{image}</span>
        <span className={styles.muted}>Running</span>
        <span>
          <span className={styles.code}>{shortId(currentId)}</span>
          {!currentId && (
            <span className={styles.muted}> (not on this server: Save and redeploy below pulls it)</span>
          )}
        </span>
        <span className={styles.muted}>Rollback</span>
        <span>
          {previousId ? (
            <>
              <span className={styles.code}>{shortId(previousId)}</span>
              <span className={styles.muted}> - an update that does not come up returns to it</span>
            </>
          ) : (
            <span className={styles.muted}>none yet (kept from the first update on)</span>
          )}
        </span>
      </div>

      {pinned ? (
        <p className={styles.muted} style={{ marginBottom: 0 }}>
          The image is pinned to a digest, so it never changes. Change the image in Settings to
          move to another build.
        </p>
      ) : (
        canManage && (
          <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap", marginTop: 12 }}>
            <button type="button" className={styles.btnGhost} onClick={check} disabled={checking || updating}>
              {checking ? "Checking…" : "Check for updates"}
            </button>
            <button
              type="button"
              className={status?.update_available ? styles.btn : styles.btnGhost}
              onClick={update}
              disabled={checking || updating}
              title={`docker pull ${image}, then restart on it if it changed`}
            >
              {updating ? "Starting…" : "Update now"}
            </button>
            {status && status.update_available === true && <StatusBadge text="newer image available" tone="warn" />}
            {status && status.update_available === false && <StatusBadge text="up to date" tone="ok" />}
            {status && status.update_available === null && (
              <span className={styles.muted} style={{ fontSize: 12 }}>
                Could not check: {status.error}
              </span>
            )}
          </div>
        )
      )}
      {error && (
        <div className={styles.error} style={{ marginTop: 10 }}>
          {error}
        </div>
      )}
    </div>
  );
}
