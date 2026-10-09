"use client";

import { useActionState, useEffect, useMemo, useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";
import TaskProgress from "@/components/TaskProgress/TaskProgress";
import StatusBadge from "@/components/StatusBadge/StatusBadge";
import shared from "@/components/page.module.css";
import styles from "./QuickInstallStrip.module.css";
import type {
  GetN8nSettingsResult,
  UpdateN8nSettingsResult,
} from "@/server-actions/quickInstall";

type Props = {
  getAction: () => Promise<GetN8nSettingsResult>;
  updateAction: (
    prev: UpdateN8nSettingsResult | null,
    fd: FormData,
  ) => Promise<UpdateN8nSettingsResult>;
  /** Whether the page-load Docker Hub probe for a newer n8n image is on.
   *  Panel-side preference (not container env), so it renders immediately
   *  without waiting for getAction. Optional - strips without the /apps
   *  context omit it and the toggle row simply doesn't render. */
  autoCheck?: boolean;
  /** Persist the auto-check preference (admin-gated server action). */
  setAutoCheck?: (enabled: boolean) => Promise<void>;
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
 * Optimistic on/off switch for n8n's page-load update probe: flip
 * immediately, persist via the server action, revert on error. No
 * router.refresh() - the action's revalidatePath("/apps") already
 * re-renders the page (card note included) in the action response, and a
 * second refresh would run the Docker Hub probe + git fetches twice.
 * Prop-synced so an external refresh (another tab, failed persist) wins.
 */
function AutoCheckRow({
  autoCheck,
  setAutoCheck,
}: {
  autoCheck: boolean;
  setAutoCheck: (enabled: boolean) => Promise<void>;
}) {
  const [on, setOn] = useState(autoCheck);
  const [pending, startTransition] = useTransition();

  useEffect(() => {
    setOn(autoCheck);
  }, [autoCheck]);

  const toggle = () => {
    if (pending) return;
    const next = !on;
    setOn(next);
    startTransition(async () => {
      try {
        await setAutoCheck(next);
      } catch {
        setOn(!next); // revert on failure
      }
    });
  };

  return (
    <div
      style={{
        display: "flex",
        alignItems: "flex-start",
        gap: 10,
        padding: "10px 12px",
        background: "var(--bg-elev-2)",
        border: "1px solid var(--border)",
        borderRadius: 6,
        marginBottom: 14,
        fontSize: 12.5,
      }}
    >
      <button
        type="button"
        role="switch"
        aria-checked={on}
        aria-label="Automatically check for n8n updates on page load"
        onClick={toggle}
        disabled={pending}
        className={`${styles.switch} ${on ? styles.switchOn : ""}`}
        style={{ marginTop: 1 }}
      >
        <span className={styles.knob} />
      </button>
      <div>
        <div
          style={{ cursor: "pointer", userSelect: "none", fontWeight: 600 }}
          onClick={toggle}
        >
          Automatically check for updates on page load
        </div>
        <div className={shared.muted} style={{ fontSize: 11.5, marginTop: 2 }}>
          {on
            ? "Each load of the Apps page asks Docker Hub whether a newer n8n image exists (6s cap). The card shows an Update pill when one does."
            : "No registry call on page load — the card won't show the Update pill until this is back on."}
        </div>
      </div>
    </div>
  );
}

/**
 * Settings button + modal for the n8n quick-install card. Reads the
 * running container's current public-URL env (N8N_HOST / N8N_PROTOCOL /
 * WEBHOOK_URL) and any /domains routing slots pointing at port 5678,
 * then lets the operator pick or paste a public URL. Apply recreates the
 * container with the new env — workflows in n8n_data are preserved.
 * Also hosts the per-app "auto-check for updates" switch (a panel-side
 * preference, rendered above the container settings so it works even when
 * the container env can't be read).
 *
 * UX detail: editing the Public URL field auto-derives Host + Protocol,
 * so the operator only has to fill in one place. The derived values stay
 * visible (read-only) underneath as a sanity check.
 */
export default function N8nSettings({
  getAction,
  updateAction,
  autoCheck,
  setAutoCheck,
}: Props) {
  const router = useRouter();
  const [open, setOpen] = useState(false);
  const [loading, startLoad] = useTransition();
  const [loaded, setLoaded] = useState<GetN8nSettingsResult | null>(null);
  const [updateState, updateFormAction, updatePending] = useActionState<
    UpdateN8nSettingsResult | null,
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
        // Prefer the explicit WEBHOOK_URL — that's what n8n actually
        // hands to webhook URLs. Fall back to deriving from host+protocol.
        const initial =
          r.data.webhook_url ||
          (r.data.host && r.data.protocol
            ? `${r.data.protocol}://${r.data.host}/`
            : "");
        setPublicUrl(initial);
      }
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  // Derive host + protocol from the public URL the operator typed/pasted.
  // We surface these so the operator sees exactly what's about to land in
  // the docker run. URL constructor handles "https://x.example.com/foo"
  // vs "https://x.example.com" vs "x.example.com" (with fallback).
  const derived = useMemo(() => {
    const raw = publicUrl.trim();
    if (!raw) return { host: "", protocol: "", normalizedUrl: "" };
    try {
      const u = new URL(
        raw.includes("://") ? raw : `https://${raw}`,
      );
      const proto = u.protocol.replace(":", "");
      const host = u.host;
      // Always submit a URL with a trailing slash. n8n's webhook generator
      // appends "/webhook/<id>" without leading slash, so a missing
      // trailing slash here ends up as "https://hostwebhook/<id>".
      let normalizedUrl = `${proto}://${host}${u.pathname || "/"}`;
      if (!normalizedUrl.endsWith("/")) normalizedUrl += "/";
      return { host, protocol: proto, normalizedUrl };
    } catch {
      return { host: "", protocol: "", normalizedUrl: "" };
    }
  }, [publicUrl]);

  const onTaskComplete = (s: "succeeded" | "failed") => {
    if (s === "succeeded") router.refresh();
  };

  const submitting = updatePending;
  const validDerivation =
    !!derived.host &&
    (derived.protocol === "http" || derived.protocol === "https") &&
    derived.host.includes(".");

  return (
    <>
      <button
        type="button"
        className={styles.settingsBtn}
        onClick={() => setOpen(true)}
        title="n8n settings — public URL / webhook host, update check"
        aria-label="n8n settings"
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
        title="n8n settings"
        hideActions
        pending={submitting}
        onConfirm={() => {}}
        onCancel={() => {
          if (submitting) return;
          setOpen(false);
        }}
      >
        <p style={{ marginTop: 0 }}>
          Set the public URL n8n uses when generating webhook URLs. Without
          this, n8n hands out{" "}
          <span className={shared.code}>http://localhost:5678/webhook/…</span>
          {" "}even when you have a nginx vhost in front. Applying tears
          down + recreates the container with the new env. The{" "}
          <span className={shared.code}>n8n_data</span> volume (workflows,
          credentials, sqlite DB) is preserved.
        </p>

        {autoCheck !== undefined && setAutoCheck && (
          <AutoCheckRow autoCheck={autoCheck} setAutoCheck={setAutoCheck} />
        )}

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
              <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
                <span className={shared.muted}>Current:</span>
                {loaded.data.webhook_url ? (
                  <StatusBadge text="public URL set" tone="ok" />
                ) : (
                  <StatusBadge text="defaulting to localhost" tone="warn" />
                )}
                <span className={shared.muted}>· bind:</span>{" "}
                <span className={shared.code}>{loaded.data.bind}</span>
              </div>
              <div>
                <span className={shared.muted}>WEBHOOK_URL:</span>{" "}
                <span className={shared.code}>
                  {loaded.data.webhook_url || "(unset)"}
                </span>
              </div>
              <div>
                <span className={shared.muted}>N8N_HOST:</span>{" "}
                <span className={shared.code}>
                  {loaded.data.host || "(unset, defaults to localhost)"}
                </span>
              </div>
              <div>
                <span className={shared.muted}>N8N_PROTOCOL:</span>{" "}
                <span className={shared.code}>
                  {loaded.data.protocol || "(unset, defaults to http)"}
                </span>
              </div>
              {loaded.data.suggested_domains.length > 0 && (
                <div style={{ marginTop: 4 }}>
                  <span className={shared.muted}>
                    Routing slots on /domains → port 5678:
                  </span>
                  <div style={{ marginTop: 4, display: "flex", gap: 6, flexWrap: "wrap" }}>
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
                  No /domains routing slots point at port 5678 yet. Create
                  one at <span className={shared.code}>/domains</span> with{" "}
                  <strong>Mode = Reverse proxy to port</strong>, port 5678,
                  then come back here.
                </div>
              )}
            </div>

            <form action={updateFormAction}>
              <input type="hidden" name="host" value={derived.host} />
              <input type="hidden" name="protocol" value={derived.protocol} />
              <input
                type="hidden"
                name="webhook_url"
                value={derived.normalizedUrl}
              />

              <label className={shared.field} style={{ marginBottom: 10 }}>
                Public URL
                <input
                  className={shared.input}
                  value={publicUrl}
                  onChange={(e) => setPublicUrl(e.target.value)}
                  placeholder="https://n8n.example.com/"
                  autoComplete="off"
                />
                <span className={shared.muted} style={{ fontSize: 12, marginTop: 4 }}>
                  The full URL n8n is reachable at from the public internet —
                  typically the matching <span className={shared.code}>/domains</span>{" "}
                  entry above. n8n splits this into{" "}
                  <span className={shared.code}>N8N_HOST</span> +{" "}
                  <span className={shared.code}>N8N_PROTOCOL</span> internally;
                  the trailing slash matters (it gets joined with{" "}
                  <span className={shared.code}>webhook/&lt;id&gt;</span>).
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
                    <span className={shared.muted}>WEBHOOK_URL =</span>{" "}
                    <span className={shared.code}>
                      {derived.normalizedUrl || "(invalid URL)"}
                    </span>
                  </div>
                  <div>
                    <span className={shared.muted}>N8N_HOST =</span>{" "}
                    <span className={shared.code}>
                      {derived.host || "(invalid)"}
                    </span>
                  </div>
                  <div>
                    <span className={shared.muted}>N8N_PROTOCOL =</span>{" "}
                    <span className={shared.code}>
                      {derived.protocol || "(invalid)"}
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
                  disabled={submitting || !validDerivation}
                  title={
                    !validDerivation
                      ? "Enter a full URL with scheme (https://n8n.example.com/)"
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
