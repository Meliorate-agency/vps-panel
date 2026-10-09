"use client";

import { useActionState, useEffect, useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";
import TaskProgress from "@/components/TaskProgress/TaskProgress";
import StatusBadge from "@/components/StatusBadge/StatusBadge";
import shared from "@/components/page.module.css";
import styles from "./QuickInstallStrip.module.css";
import type {
  GetPhpMyAdminSettingsResult,
  UpdatePhpMyAdminSettingsResult,
} from "@/server-actions/quickInstall";

type Props = {
  getAction: () => Promise<GetPhpMyAdminSettingsResult>;
  updateAction: (
    prev: UpdatePhpMyAdminSettingsResult | null,
    fd: FormData,
  ) => Promise<UpdatePhpMyAdminSettingsResult>;
};

function Spinner() {
  return (
    <svg
      width="12"
      height="12"
      viewBox="0 0 24 24"
      fill="none"
      aria-hidden="true"
      style={{ animation: "vps-panel-spin 0.8s linear infinite", marginRight: 4 }}
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
 * Settings button + modal for the phpMyAdmin quick-install card. Reads the
 * running container's current PMA_ABSOLUTE_URI and any /domains routing
 * slots pointing at port 8081, then lets the operator pick or paste a
 * public URL. Applying recreates the container with the new env var so
 * phpMyAdmin's session-cookie-secure detection works behind the nginx
 * HTTPS proxy. Bind is preserved.
 *
 * The "Failed to set session cookie. Maybe you are using HTTP instead of
 * HTTPS" error on the phpMyAdmin login page is exactly this — the image
 * doesn't trust X-Forwarded-Proto by default, so it thinks the request
 * came in over HTTP and refuses to set the Secure cookie the browser
 * needs. Setting PMA_ABSOLUTE_URI to the real HTTPS URL fixes it.
 */
export default function PhpMyAdminSettings({ getAction, updateAction }: Props) {
  const router = useRouter();
  const [open, setOpen] = useState(false);
  const [loading, startLoad] = useTransition();
  const [loaded, setLoaded] = useState<GetPhpMyAdminSettingsResult | null>(null);
  const [updateState, updateFormAction, updatePending] = useActionState<
    UpdatePhpMyAdminSettingsResult | null,
    FormData
  >(updateAction, null);

  const [publicUrl, setPublicUrl] = useState("");

  // Load once per open. The getAction prop gets a new identity whenever the
  // page refreshes (another action, a finished task, the n8n auto-check
  // toggle), and depending on it reloaded the settings and wiped what the
  // operator had typed.
  useEffect(() => {
    if (!open) return;
    setLoaded(null);
    startLoad(async () => {
      const r = await getAction();
      setLoaded(r);
      if (r.ok) {
        setPublicUrl(r.data.absolute_uri);
      }
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  // Normalize on submit: always include a scheme and trailing slash so
  // the operator can paste "pma.example.com" and we'll persist
  // "https://pma.example.com/". Empty stays empty (means "clear the var").
  const normalizedUrl = (() => {
    const raw = publicUrl.trim();
    if (!raw) return "";
    try {
      const u = new URL(raw.includes("://") ? raw : `https://${raw}`);
      const proto = u.protocol.replace(":", "");
      if (proto !== "http" && proto !== "https") return "";
      let normalized = `${proto}://${u.host}${u.pathname || "/"}`;
      if (!normalized.endsWith("/")) normalized += "/";
      return normalized;
    } catch {
      return "";
    }
  })();

  const validUrl =
    publicUrl.trim().length === 0 ||
    (normalizedUrl.length > 0 && normalizedUrl.includes("."));

  const onTaskComplete = (s: "succeeded" | "failed") => {
    if (s === "succeeded") router.refresh();
  };

  const submitting = updatePending;

  return (
    <>
      <button
        type="button"
        className={styles.settingsBtn}
        onClick={() => setOpen(true)}
        title="Edit public URL (fix HTTPS cookie error)"
        aria-label="phpMyAdmin settings"
      >
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true">
          <path
            d="M12 15.5a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7Z"
            stroke="currentColor"
            strokeWidth="1.8"
          />
          <path
            d="M19.4 13a7.5 7.5 0 0 0 0-2l2-1.6-2-3.4-2.4.8a7.5 7.5 0 0 0-1.7-1l-.4-2.5h-4l-.4 2.5a7.5 7.5 0 0 0-1.7 1L6.4 6l-2 3.4 2 1.6a7.5 7.5 0 0 0 0 2l-2 1.6 2 3.4 2.4-.8a7.5 7.5 0 0 0 1.7 1l.4 2.5h4l.4-2.5a7.5 7.5 0 0 0 1.7-1l2.4.8 2-3.4-2-1.6Z"
            stroke="currentColor"
            strokeWidth="1.6"
            strokeLinejoin="round"
          />
        </svg>
      </button>

      <ConfirmDialog
        open={open}
        title="phpMyAdmin settings"
        hideActions
        pending={submitting}
        onConfirm={() => {}}
        onCancel={() => {
          if (submitting) return;
          setOpen(false);
        }}
      >
        <p style={{ marginTop: 0 }}>
          Sets <span className={shared.code}>PMA_ABSOLUTE_URI</span> on the
          container so phpMyAdmin knows its real public URL. Without it,
          the login page shows <em>&ldquo;Failed to set session cookie.
          Maybe you are using HTTP instead of HTTPS&rdquo;</em> even when
          you hit the HTTPS subdomain — nginx talks to the container over
          plain HTTP internally, and phpMyAdmin doesn&apos;t trust the
          forwarded scheme by default. Applying recreates the container
          with the new env. Bind is preserved.
        </p>

        {loading && (
          <div className={shared.muted} style={{ marginBottom: 10 }}>
            Reading current settings from the container…
          </div>
        )}

        {loaded?.ok === false && (
          <div className={shared.error} style={{ marginBottom: 10 }}>
            Could not read current settings: {loaded.error}
          </div>
        )}

        {loaded?.ok && (
          <>
            <div
              style={{
                display: "grid",
                gap: 6,
                marginBottom: 14,
                padding: "10px 12px",
                background: "var(--bg-elev-2)",
                border: "1px solid var(--border)",
                borderRadius: 6,
                fontSize: 12,
              }}
            >
              <div
                style={{
                  display: "flex",
                  gap: 8,
                  alignItems: "center",
                  flexWrap: "wrap",
                }}
              >
                <span className={shared.muted}>Current:</span>
                {loaded.data.absolute_uri ? (
                  <StatusBadge text="public URL set" tone="ok" />
                ) : (
                  <StatusBadge text="defaulting (cookie error likely)" tone="warn" />
                )}
                <span className={shared.muted}>· bind:</span>{" "}
                <span className={shared.code}>{loaded.data.bind}</span>
              </div>
              <div>
                <span className={shared.muted}>PMA_ABSOLUTE_URI:</span>{" "}
                <span className={shared.code}>
                  {loaded.data.absolute_uri || "(unset)"}
                </span>
              </div>
              {loaded.data.suggested_domains.length > 0 && (
                <div style={{ marginTop: 4 }}>
                  <span className={shared.muted}>
                    Routing slots on /domains → port 8081:
                  </span>
                  <div
                    style={{
                      marginTop: 4,
                      display: "flex",
                      gap: 6,
                      flexWrap: "wrap",
                    }}
                  >
                    {loaded.data.suggested_domains.map((d) => (
                      <button
                        key={d}
                        type="button"
                        className={shared.btnGhost}
                        style={{ fontSize: 11, padding: "2px 8px" }}
                        onClick={() => setPublicUrl(`https://${d}/`)}
                        title={`Use https://${d}/ as the public URL`}
                      >
                        {d}
                      </button>
                    ))}
                  </div>
                </div>
              )}
              {loaded.data.suggested_domains.length === 0 && (
                <div className={shared.muted} style={{ marginTop: 4 }}>
                  No /domains routing slots point at port 8081 yet. Create
                  one at <span className={shared.code}>/domains</span> with{" "}
                  <strong>Mode = Reverse proxy to port</strong>, port 8081,
                  then come back here.
                </div>
              )}
            </div>

            <form action={updateFormAction}>
              <input
                type="hidden"
                name="absolute_uri"
                value={normalizedUrl}
              />

              <label className={shared.field} style={{ marginBottom: 10 }}>
                Public URL
                <input
                  className={shared.input}
                  value={publicUrl}
                  onChange={(e) => setPublicUrl(e.target.value)}
                  placeholder="https://pma.example.com/"
                  autoComplete="off"
                />
                <span
                  className={shared.muted}
                  style={{ fontSize: 12, marginTop: 4 }}
                >
                  The full URL phpMyAdmin is reachable at from the public
                  internet — usually the matching{" "}
                  <span className={shared.code}>/domains</span> entry above.
                  Empty clears the env (back to default phpMyAdmin
                  behaviour, which breaks HTTPS logins).
                </span>
              </label>

              {publicUrl.trim() && (
                <div
                  style={{
                    padding: "8px 10px",
                    background: "var(--bg-elev-2)",
                    border: "1px solid var(--border)",
                    borderRadius: 6,
                    fontSize: 12,
                    marginBottom: 12,
                  }}
                >
                  <div className={shared.muted}>About to apply:</div>
                  <div style={{ marginTop: 4 }}>
                    <span className={shared.muted}>PMA_ABSOLUTE_URI =</span>{" "}
                    <span className={shared.code}>
                      {normalizedUrl || "(invalid URL)"}
                    </span>
                  </div>
                </div>
              )}

              {updateState?.ok === false && (
                <div className={shared.error} style={{ marginBottom: 10 }}>
                  {updateState.error}
                </div>
              )}

              <div
                style={{
                  display: "flex",
                  justifyContent: "flex-end",
                  gap: 8,
                  marginTop: 12,
                }}
              >
                <button
                  type="button"
                  className={shared.btnGhost}
                  onClick={() => {
                    if (submitting) return;
                    setOpen(false);
                  }}
                  disabled={submitting}
                >
                  Close
                </button>
                <button
                  type="submit"
                  className={shared.btn}
                  disabled={submitting || !validUrl}
                  title={
                    !validUrl
                      ? "Enter a full URL with scheme (https://pma.example.com/) or leave empty to clear"
                      : "Recreate the container with the new env"
                  }
                >
                  {submitting ? (
                    <>
                      <Spinner /> Applying…
                    </>
                  ) : (
                    "Apply"
                  )}
                </button>
              </div>
            </form>
          </>
        )}

        {updateState?.ok && updateState.data?.taskId && (
          <div style={{ marginTop: 14 }}>
            <TaskProgress
              taskId={updateState.data.taskId}
              onComplete={onTaskComplete}
            />
          </div>
        )}
      </ConfirmDialog>
    </>
  );
}
