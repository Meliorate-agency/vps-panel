"use client";

import { useState, useTransition } from "react";
import topbarStyles from "@/components/Topbar/Topbar.module.css";
import pageStyles from "@/components/page.module.css";
import type { DockerImageStatus, GitStatusResp } from "@/lib/agent";
import type { ActionResult } from "@/server-actions/apps";
import GitStatusBadge from "./GitStatusBadge";
import ImageStatusBadge from "./ImageStatusBadge";

type UpdateResult = { ok: true; taskId: string } | { ok: false; error: string };
type GitResult = { ok: true; status: GitStatusResp } | { ok: false; error: string };

function Spinner() {
  return (
    <svg
      className={topbarStyles.spinner}
      width="12"
      height="12"
      viewBox="0 0 24 24"
      fill="none"
      aria-hidden="true"
    >
      <circle
        cx="12"
        cy="12"
        r="9"
        stroke="currentColor"
        strokeWidth="2.5"
        strokeLinecap="round"
        strokeDasharray="42 60"
      />
    </svg>
  );
}

/**
 * The Updates cell when the page-load auto-check is off: a Check button that
 * asks about this one app (git fetch, or the registry for an image app) and
 * then shows the badge the auto-check would have, update popover included.
 * Checking changes nothing on the server; updating stays a separate click.
 */
export default function CheckStatusCell({
  appName,
  imageApp,
  docker,
  updateAction,
  refreshGitAction,
  refreshImageAction,
}: {
  appName: string;
  /** A Docker app pulled from a registry: no checkout, ask the registry. */
  imageApp: boolean;
  /** A Docker app built from Git (an update runs docker build). */
  docker: boolean;
  updateAction: (name: string) => Promise<UpdateResult>;
  refreshGitAction: (name: string) => Promise<GitResult>;
  refreshImageAction: (name: string) => Promise<ActionResult<DockerImageStatus>>;
}) {
  const [pending, start] = useTransition();
  const [git, setGit] = useState<GitStatusResp | null>(null);
  const [image, setImage] = useState<DockerImageStatus | null>(null);
  const [error, setError] = useState<string | null>(null);

  if (git) {
    return (
      <GitStatusBadge
        appName={appName}
        status={git}
        isExternal={false}
        updateAction={updateAction}
        refreshAction={refreshGitAction}
        docker={docker}
      />
    );
  }
  if (image) {
    return <ImageStatusBadge appName={appName} status={image} updateAction={updateAction} />;
  }

  const run = () => {
    setError(null);
    start(async () => {
      if (imageApp) {
        const r = await refreshImageAction(appName);
        if (!r.ok) setError(r.error);
        else if (!r.data || r.data.update_available === null) {
          setError(r.data?.error ?? "could not check the registry");
        } else setImage(r.data);
        return;
      }
      const r = await refreshGitAction(appName);
      if (r.ok) setGit(r.status);
      else setError(r.error);
    });
  };

  return (
    <button
      type="button"
      onClick={run}
      disabled={pending}
      className={pageStyles.btnGhost}
      title={error ?? `Check ${appName} for updates. Nothing is pulled until you click Update.`}
      style={{
        display: "inline-flex",
        alignItems: "center",
        gap: 5,
        whiteSpace: "nowrap",
        ...(error ? { color: "var(--danger, #ff4d6d)" } : null),
      }}
    >
      {pending ? (
        <>
          <Spinner /> Checking…
        </>
      ) : error ? (
        "Check failed, retry"
      ) : (
        "Check"
      )}
    </button>
  );
}
