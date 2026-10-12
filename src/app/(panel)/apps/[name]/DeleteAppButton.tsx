"use client";

import { useState, useTransition } from "react";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";
import styles from "@/components/page.module.css";

/**
 * Styled replacement for the old window.confirm() delete prompt. Adds a
 * "wipe source directory" checkbox so the operator can pick whether
 * /srv/apps/<name> stays on disk (default — useful for re-deploys that
 * keep node_modules / .git) or gets `rm -rf`'d. For a Docker app the same
 * checkbox deletes its data volumes instead.
 */
export default function DeleteAppButton({
  appName,
  sourceDir,
  deleteAction,
  docker,
}: {
  appName: string;
  sourceDir: string;
  deleteAction: (name: string, opts?: { wipe?: boolean }) => Promise<void>;
  /** Set for a Docker app: its image and data volume names, and the Git
   *  checkout it is built from (null for an image from a registry). */
  docker?: { image: string; volumes: string[]; checkout?: string | null };
}) {
  const [open, setOpen] = useState(false);
  const [wipe, setWipe] = useState(false);
  const [pending, startTransition] = useTransition();
  const [error, setError] = useState<string | null>(null);

  const onConfirm = () => {
    setError(null);
    startTransition(async () => {
      try {
        await deleteAction(appName, { wipe });
        // deleteAppAction redirects to /apps — if we get here, the redirect
        // hasn't fired (likely an error before agent call).
        setOpen(false);
      } catch (e) {
        // redirect() throws a special "NEXT_REDIRECT" error to navigate.
        // Don't treat that as a UI error — it's expected on success.
        const msg = e instanceof Error ? e.message : String(e);
        if (msg.includes("NEXT_REDIRECT")) {
          return;
        }
        setError(msg);
      }
    });
  };

  return (
    <>
      <button
        type="button"
        className={styles.btnDanger}
        onClick={() => {
          setError(null);
          setWipe(false);
          setOpen(true);
        }}
      >
        Delete app
      </button>

      <ConfirmDialog
        open={open}
        title={`Delete ${appName}?`}
        confirmLabel={wipe ? "Delete + wipe" : "Delete"}
        cancelLabel="Cancel"
        tone="danger"
        pending={pending}
        onConfirm={onConfirm}
        onCancel={() => {
          if (pending) return;
          setOpen(false);
        }}
      >
        <p style={{ marginTop: 0 }}>
          {docker
            ? "Stops the container, removes its systemd unit, env file and settings, and the app's image tags."
            : "Stops the systemd unit, removes the unit file and env file, and (if the app owns its own nginx vhost) deletes that too."}
        </p>

        <div
          style={{
            background: "var(--bg-elev-2)",
            border: "1px solid var(--border)",
            borderRadius: 6,
            padding: "10px 12px",
            fontSize: 12,
            fontFamily: "var(--mono)",
            marginTop: 10,
            wordBreak: "break-all",
          }}
        >
          <div>
            <strong style={{ fontFamily: "inherit" }}>App:</strong> {appName}
          </div>
          {docker ? (
            <div>
              <strong style={{ fontFamily: "inherit" }}>Image:</strong> {docker.image}
            </div>
          ) : (
            <div>
              <strong style={{ fontFamily: "inherit" }}>Source:</strong>{" "}
              {sourceDir || "(unknown)"}
            </div>
          )}
        </div>

        <label
          className={styles.checkbox}
          style={{
            marginTop: 12,
            padding: "10px 12px",
            background: wipe ? "rgba(255, 77, 109, 0.10)" : "var(--bg-elev-2)",
            border: `1px solid ${wipe ? "rgba(255, 77, 109, 0.35)" : "var(--border)"}`,
            borderRadius: 6,
            display: "flex",
            gap: 10,
            cursor: "pointer",
            transition: "background 0.12s, border-color 0.12s",
          }}
        >
          <input
            type="checkbox"
            checked={wipe}
            onChange={(e) => setWipe(e.target.checked)}
          />
          {docker ? (
            <span style={{ display: "flex", flexDirection: "column", gap: 2 }}>
              <strong style={{ fontSize: 13 }}>
                Also delete the data volumes{docker.checkout ? " and the checkout" : ""}
              </strong>
              <span className={styles.muted} style={{ fontSize: 12 }}>
                {docker.volumes.length > 0 ? (
                  <span className={styles.code}>{docker.volumes.join(", ")}</span>
                ) : (
                  "The app keeps no folders."
                )}{" "}
                - everything the app saved
                {docker.checkout && (
                  <>
                    , and <span className={styles.code}>{docker.checkout}</span>
                  </>
                )}
                . Cannot be undone. Leave unchecked to keep the data: deploying an app with the
                same name and folders picks it up again.
              </span>
            </span>
          ) : (
          <span style={{ display: "flex", flexDirection: "column", gap: 2 }}>
            <strong style={{ fontSize: 13 }}>
              Also wipe the source directory
            </strong>
            <span className={styles.muted} style={{ fontSize: 12 }}>
              <span className={styles.code}>
                rm -rf {sourceDir || `/srv/apps/${appName}`}
              </span>{" "}
              — deletes the cloned repo, <span className={styles.code}>node_modules</span>,{" "}
              <span className={styles.code}>.git</span>, build artifacts. Cannot be undone.
              Leave unchecked if you want to redeploy on this folder later.
            </span>
          </span>
          )}
        </label>

        <div className={styles.muted} style={{ fontSize: 12, marginTop: 12 }}>
          <strong>Kept regardless:</strong> the /domains routing slot (subdomain
          + nginx vhost), DNS records, and the SSL cert. To remove those, use
          the Delete button on the matching /domains row.
        </div>

        {error && (
          <div className={styles.error} style={{ marginTop: 10 }}>
            {error}
          </div>
        )}
      </ConfirmDialog>
    </>
  );
}
