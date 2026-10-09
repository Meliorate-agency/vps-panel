"use client";

import { useActionState, useEffect, useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";
import TaskProgress from "@/components/TaskProgress/TaskProgress";
import StatusBadge from "@/components/StatusBadge/StatusBadge";
import shared from "@/components/page.module.css";
import styles from "./QuickInstallStrip.module.css";
import type {
  GetRoundcubeSettingsResult,
  UpdateRoundcubeSettingsResult,
} from "@/server-actions/quickInstall";

type Props = {
  getAction: () => Promise<GetRoundcubeSettingsResult>;
  updateAction: (
    prev: UpdateRoundcubeSettingsResult | null,
    fd: FormData,
  ) => Promise<UpdateRoundcubeSettingsResult>;
};

/**
 * Settings button + modal for the Roundcube quick-install card. Opens a
 * dialog that reads the running container's current IMAP/SMTP target,
 * shows the operator both the current value AND postfix's myhostname
 * (so they can spot drift at a glance), and lets them edit + apply new
 * values. Apply spawns an agent task that stops + recreates the container
 * and wipes the cached config volume.
 */
export default function RoundcubeSettings({ getAction, updateAction }: Props) {
  const router = useRouter();
  const [open, setOpen] = useState(false);
  const [loading, startLoad] = useTransition();
  const [loaded, setLoaded] = useState<GetRoundcubeSettingsResult | null>(null);
  const [updateState, updateFormAction, updatePending] = useActionState<
    UpdateRoundcubeSettingsResult | null,
    FormData
  >(updateAction, null);

  // Form-controlled fields prefilled from the loaded settings.
  const [mailHost, setMailHost] = useState("");
  const [imapPort, setImapPort] = useState<number>(993);
  const [smtpPort, setSmtpPort] = useState<number>(587);

  // Fetch the current settings every time the dialog opens so the operator
  // always sees fresh values, not whatever was cached from the last visit.
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
        setMailHost(r.data.mail_host);
        setImapPort(r.data.imap_port);
        setSmtpPort(r.data.smtp_port);
      }
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  // When the apply task finishes, refresh the parent route so the strip
  // re-renders with up-to-date container state.
  const onTaskComplete = (s: "succeeded" | "failed") => {
    if (s === "succeeded") {
      router.refresh();
      // Keep the dialog open so the operator can see the "ok" log; close
      // is manual.
    }
  };

  const detected = loaded?.ok ? loaded.data.detected_mail_host : null;
  const inSync = loaded?.ok ? loaded.data.in_sync : false;
  const driftDetected =
    loaded?.ok && detected && detected !== loaded.data.mail_host;

  return (
    <>
      <button
        type="button"
        className={styles.settingsBtn}
        onClick={() => setOpen(true)}
        title="Edit IMAP/SMTP mail host + ports"
        aria-label="Roundcube settings"
      >
        <svg
          width="14"
          height="14"
          viewBox="0 0 24 24"
          fill="none"
          aria-hidden="true"
        >
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
        title="Roundcube settings"
        hideActions
        pending={updatePending}
        onConfirm={() => {}}
        onCancel={() => {
          if (updatePending) return;
          setOpen(false);
        }}
      >
        <p style={{ marginTop: 0 }}>
          Edit the IMAP / SMTP target the Roundcube container connects to.
          Applying tears down the container and recreates it with the new
          values, wiping{" "}
          <span className={shared.code}>roundcube_config</span> so the new
          values take effect. User prefs in{" "}
          <span className={shared.code}>roundcube_db</span> are preserved.
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
                style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}
              >
                <span className={shared.muted}>Sync status:</span>
                {inSync ? (
                  <StatusBadge text="in sync with postfix" tone="ok" />
                ) : detected ? (
                  <StatusBadge text="container drifted" tone="warn" />
                ) : (
                  <StatusBadge text="postfix unreachable" tone="muted" />
                )}
              </div>
              <div>
                <span className={shared.muted}>Detected (postfix myhostname):</span>{" "}
                <span className={shared.code}>{detected ?? "(unknown)"}</span>
              </div>
              <div>
                <span className={shared.muted}>Container currently uses:</span>{" "}
                <span className={shared.code}>{loaded.data.mail_host}</span>
              </div>
              {driftDetected && (
                <div className={shared.muted} style={{ marginTop: 4 }}>
                  Click{" "}
                  <button
                    type="button"
                    className={shared.btnGhost}
                    style={{ fontSize: 11, padding: "2px 8px" }}
                    onClick={() => setMailHost(detected!)}
                  >
                    Use detected
                  </button>{" "}
                  to fill the field with the value above, then Apply.
                </div>
              )}
            </div>

            <form action={updateFormAction}>
              <label className={shared.field} style={{ marginBottom: 10 }}>
                Mail host (IMAP + SMTP target)
                <input
                  name="mail_host"
                  className={shared.input}
                  value={mailHost}
                  onChange={(e) => setMailHost(e.target.value)}
                  placeholder="vps.example.com"
                  required
                  autoComplete="off"
                />
                <span
                  className={shared.muted}
                  style={{ fontSize: 12, marginTop: 4 }}
                >
                  Must match a SAN on the Dovecot TLS cert. The agent uses{" "}
                  <span className={shared.code}>ssl://&lt;host&gt;</span> for
                  IMAP and <span className={shared.code}>tls://&lt;host&gt;</span>{" "}
                  for SMTP submission.
                </span>
              </label>
              <div className={shared.formRow}>
                <label className={shared.field}>
                  IMAP port
                  <input
                    name="imap_port"
                    type="number"
                    min={1}
                    max={65535}
                    className={shared.input}
                    value={imapPort}
                    onChange={(e) => setImapPort(Number(e.target.value))}
                    required
                  />
                </label>
                <label className={shared.field}>
                  SMTP port
                  <input
                    name="smtp_port"
                    type="number"
                    min={1}
                    max={65535}
                    className={shared.input}
                    value={smtpPort}
                    onChange={(e) => setSmtpPort(Number(e.target.value))}
                    required
                  />
                </label>
              </div>

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
                    if (updatePending) return;
                    setOpen(false);
                  }}
                  disabled={updatePending}
                >
                  Close
                </button>
                <button
                  type="submit"
                  className={shared.btn}
                  disabled={updatePending}
                >
                  {updatePending ? "Applying…" : "Apply"}
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
