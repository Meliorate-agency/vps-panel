"use client";

import { useState, useTransition } from "react";
import styles from "@/components/page.module.css";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";
import StatusBadge from "@/components/StatusBadge/StatusBadge";
import TaskProgress from "@/components/TaskProgress/TaskProgress";

type CertStatus = {
  cert_exists: boolean;
  cert_not_after: string | null;
  days_remaining: number | null;
};

function formatCertDate(notAfter: string | null): string | null {
  if (!notAfter) return null;
  const d = new Date(notAfter);
  if (Number.isNaN(d.getTime())) return null;
  return d.toLocaleDateString("en-US", {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
}

type StartResult =
  | { ok: true; taskId: string }
  | { ok: false; error: string };
type CompleteResult = { ok: true } | { ok: false; error: string };

type Phase =
  | { kind: "idle" }
  | { kind: "confirming"; intent: "save" | "clear" }
  | { kind: "starting"; intent: "save" | "clear" }
  | { kind: "running"; intent: "save" | "clear"; taskId: string }
  | { kind: "done"; intent: "save" | "clear"; status: "succeeded" | "failed"; taskId: string }
  | { kind: "error"; error: string };

/**
 * Mirror of findParentManagedZone in src/server-actions/_dns_ensure.ts.
 * Duplicated here because "use server" modules cannot be imported into
 * client components. Keep in sync.
 */
function findParentManagedZone(
  domain: string,
  managedZones: string[],
): string | null {
  if (!domain.includes(".")) return null;
  const zoneSet = new Set(managedZones);
  const labels = domain.split(".");
  for (let i = 1; i < labels.length; i++) {
    const candidate = labels.slice(i).join(".");
    if (zoneSet.has(candidate)) return candidate;
  }
  return null;
}

function inferParentApex(host: string): string | null {
  const labels = host.split(".");
  if (labels.length < 3) return null;
  return labels.slice(1).join(".");
}

export default function PanelHostForm({
  value,
  saveAction,
  completeAction,
  managedZones = [],
  certStatus = null,
}: {
  value: string;
  saveAction: (
    host: string,
    opts: { createParentZone: boolean; issueSsl: boolean },
  ) => Promise<StartResult>;
  completeAction: (host: string) => Promise<CompleteResult>;
  /**
   * PowerDNS zones already managed by this panel. Used to decide whether the
   * "Also create parent zone" checkbox is relevant.
   */
  managedZones?: string[];
  /**
   * Cert status for the current panel host (null when in IP-only mode or
   * lookup failed). When a cert exists, the form shows a "Valid · Nd" badge
   * + expiry date and the action button reads "Renew" instead of "Issue
   * cert" (Save still wins when the operator typed a different hostname).
   */
  certStatus?: CertStatus | null;
}) {
  const [text, setText] = useState(value);
  const [pending, start] = useTransition();
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });
  const [createParentZone, setCreateParentZone] = useState(true);
  const [issueSsl, setIssueSsl] = useState(true);

  const trimmed = text.trim();
  const dirty = trimmed !== value;
  // The agent's /system/panel-host is idempotent — re-running with the same
  // hostname is the canonical "issue / re-issue the panel cert" path. So
  // Save is enabled whenever there's a hostname, not just when the value
  // changed. The button label adapts (Save / Issue cert / Renew) so the
  // user knows which mode they're in.
  const canSave = !pending && phase.kind === "idle" && trimmed.length > 0;
  const isIssueOnly = !dirty;
  const hasValidCert =
    !!certStatus && certStatus.cert_exists && (certStatus.days_remaining ?? -1) >= 0;
  const certDateLabel = hasValidCert
    ? formatCertDate(certStatus!.cert_not_after)
    : null;
  const idleButtonLabel = !isIssueOnly
    ? "Save"
    : hasValidCert
      ? "Renew cert"
      : "Issue cert";

  const parent = findParentManagedZone(trimmed, managedZones);
  const subdomainLabel = parent
    ? trimmed.slice(0, trimmed.length - parent.length - 1)
    : null;
  const apexThatWouldBeCreated =
    !parent && trimmed.length > 0 ? inferParentApex(trimmed) : null;

  const onSaveClick = () => {
    if (!canSave) return;
    setPhase({ kind: "confirming", intent: "save" });
  };

  const onClearClick = () => {
    if (pending || phase.kind !== "idle") return;
    setPhase({ kind: "confirming", intent: "clear" });
  };

  const onCancelDialog = () => {
    if (phase.kind === "running" || phase.kind === "starting") return;
    setPhase({ kind: "idle" });
  };

  const onConfirm = () => {
    if (phase.kind !== "confirming") return;
    const intent = phase.intent;
    const host = intent === "clear" ? "" : trimmed;
    setPhase({ kind: "starting", intent });
    start(async () => {
      const r = await saveAction(host, {
        createParentZone: intent === "save" ? createParentZone : false,
        issueSsl: intent === "save" ? issueSsl : false,
      });
      if (!r.ok) {
        setPhase({ kind: "error", error: r.error });
        return;
      }
      if (intent === "clear") setText("");
      setPhase({ kind: "running", intent, taskId: r.taskId });
    });
  };

  const onTaskComplete = (status: "succeeded" | "failed") => {
    if (phase.kind !== "running") return;
    const intent = phase.intent;
    if (status === "succeeded" && intent === "save" && trimmed.length > 0) {
      // Fire and forget — surface failures from the audit log if needed.
      void completeAction(trimmed);
    }
    setPhase({ kind: "done", intent, status, taskId: phase.taskId });
  };

  const closeDoneDialog = () => {
    setPhase({ kind: "idle" });
  };

  const targetUrl =
    phase.kind === "done" && phase.intent === "save" && trimmed.length > 0
      ? `https://${trimmed}/`
      : null;

  return (
    <div>
      {certStatus && (
        <div
          style={{
            display: "flex",
            gap: 8,
            alignItems: "center",
            flexWrap: "wrap",
            marginBottom: 10,
          }}
        >
          {hasValidCert ? (
            <>
              <StatusBadge
                text={`SSL · ${certStatus.days_remaining}d`}
                tone="ok"
              />
              {certDateLabel && (
                <span className={styles.muted}>expires {certDateLabel}</span>
              )}
            </>
          ) : (
            <StatusBadge text="No cert yet" tone="warn" />
          )}
        </div>
      )}
      <div style={{ display: "flex", gap: 8, alignItems: "flex-start", flexWrap: "wrap" }}>
        <input
          type="text"
          value={text}
          onChange={(e) => setText(e.target.value)}
          placeholder="panel.yourdomain.com"
          className={styles.input}
          style={{ flex: 1, minWidth: 280 }}
          disabled={phase.kind === "running" || phase.kind === "starting"}
        />
        <button
          className={styles.btn}
          onClick={onSaveClick}
          disabled={!canSave}
          title={
            trimmed.length === 0
              ? "Enter a hostname or use Clear to revert to IP-only mode."
              : !isIssueOnly
                ? "Save the new panel hostname and re-run setup."
                : hasValidCert
                  ? "Re-issue the Let's Encrypt cert for this hostname (idempotent)."
                  : "Run panel-host setup for this hostname and issue a Let's Encrypt cert."
          }
        >
          {idleButtonLabel}
        </button>
        {value && (
          <button
            className={styles.btnGhost}
            onClick={onClearClick}
            disabled={pending || phase.kind !== "idle"}
          >
            Clear (IP-only mode)
          </button>
        )}
      </div>

      {phase.kind === "error" && (
        <div className={styles.error} style={{ marginTop: 8 }}>
          {phase.error}
        </div>
      )}

      {/* Save confirmation */}
      <ConfirmDialog
        open={phase.kind === "confirming" && phase.intent === "save"}
        title={
          !isIssueOnly
            ? `Save panel hostname "${trimmed}"?`
            : hasValidCert
              ? `Renew Let's Encrypt cert for "${trimmed}"?`
              : `Issue Let's Encrypt cert for "${trimmed}"?`
        }
        confirmLabel={
          !isIssueOnly
            ? "Save & restart"
            : hasValidCert
              ? "Renew & restart"
              : "Issue cert & restart"
        }
        cancelLabel="Cancel"
        pending={pending}
        onConfirm={onConfirm}
        onCancel={onCancelDialog}
      >
        {isIssueOnly ? (
          <p>
            The hostname is unchanged. The agent will re-run panel-host setup,{" "}
            {hasValidCert ? "renew" : "issue"} a Let&apos;s Encrypt cert via
            HTTP-01, re-render the nginx vhost to serve{" "}
            <span className={styles.code}>443</span> with the{" "}
            {hasValidCert ? "fresh" : "new"} cert, and flip{" "}
            <span className={styles.code}>COOKIE_SECURE=1</span>.
          </p>
        ) : (
          <>
            <p>This will update the following on the server:</p>
            <ul>
              <li>
                <span className={styles.code}>/etc/vps-panel/.env</span> (
                <span className={styles.code}>PANEL_HOST</span> +{" "}
                <span className={styles.code}>COOKIE_SECURE</span>)
              </li>
              <li>
                Postfix <span className={styles.code}>myhostname</span> →{" "}
                <span className={styles.code}>{trimmed}</span>
              </li>
              <li>
                Dovecot <span className={styles.code}>postmaster_address</span> →{" "}
                <span className={styles.code}>postmaster@{trimmed}</span>
              </li>
              <li>
                <span className={styles.code}>
                  /etc/nginx/sites-available/vps-panel.conf
                </span>
              </li>
            </ul>
          </>
        )}
        {!isIssueOnly && (
          <>
            <p>DNS:</p>
            <ul>
              {parent ? (
                <li>
                  Add <span className={styles.code}>A</span> record{" "}
                  <span className={styles.code}>{subdomainLabel}</span> to managed
                  parent zone <span className={styles.code}>{parent}</span>
                </li>
              ) : apexThatWouldBeCreated && createParentZone ? (
                <>
                  <li>
                    Create new PowerDNS zone for the apex{" "}
                    <span className={styles.code}>{apexThatWouldBeCreated}</span> (SOA,
                    NS×2, A @/www/mail, AAAA, CAA)
                  </li>
                  <li>
                    Add <span className={styles.code}>A</span> record{" "}
                    <span className={styles.code}>
                      {trimmed.slice(
                        0,
                        trimmed.length - apexThatWouldBeCreated.length - 1,
                      )}
                    </span>{" "}
                    inside it
                  </li>
                </>
              ) : (
                <li>
                  Create a new PowerDNS zone{" "}
                  <span className={styles.code}>{trimmed}</span> (SOA, NS×2, A
                  @/www/mail, AAAA, CAA)
                </li>
              )}
              <li>
                Domain will appear under <span className={styles.code}>/domains</span>
              </li>
            </ul>
          </>
        )}
        {!isIssueOnly && apexThatWouldBeCreated && !parent && (
          <label className={styles.checkbox} style={{ marginTop: 4 }}>
            <input
              type="checkbox"
              checked={createParentZone}
              onChange={(e) => setCreateParentZone(e.target.checked)}
            />
            Also create parent zone{" "}
            <span className={styles.code}>{apexThatWouldBeCreated}</span> if
            missing
          </label>
        )}
        <label className={styles.checkbox} style={{ marginTop: 4 }}>
          <input
            type="checkbox"
            checked={issueSsl}
            onChange={(e) => setIssueSsl(e.target.checked)}
          />
          Issue Let&apos;s Encrypt cert automatically{" "}
          <span className={styles.muted} style={{ fontSize: 12 }}>
            (cookies stay non-Secure if it fails so you can still log in over HTTP)
          </span>
        </label>
        <div
          style={{
            marginTop: 12,
            padding: "8px 10px",
            background: "rgba(255, 196, 0, 0.08)",
            border: "1px solid rgba(255, 196, 0, 0.28)",
            borderRadius: 6,
            fontSize: 12,
          }}
        >
          The panel will restart — log in again at{" "}
          <span className={styles.code}>
            {issueSsl ? "https" : "http"}://{trimmed}/
          </span>
          .
        </div>
      </ConfirmDialog>

      {/* Clear confirmation */}
      <ConfirmDialog
        open={phase.kind === "confirming" && phase.intent === "clear"}
        title="Revert to IP-only mode?"
        confirmLabel="Clear & restart"
        cancelLabel="Cancel"
        tone="danger"
        pending={pending}
        onConfirm={onConfirm}
        onCancel={onCancelDialog}
      >
        <p>
          Clears <span className={styles.code}>PANEL_HOST</span> so the panel
          becomes reachable via the server&apos;s public IP only. The nginx
          vhost becomes a <span className={styles.code}>default_server</span>{" "}
          and cookies become non-Secure (plain HTTP).
        </p>
        <p>
          The PowerDNS zone or parent A record previously added is{" "}
          <strong>not</strong> removed.
        </p>
        <div
          style={{
            marginTop: 12,
            padding: "8px 10px",
            background: "rgba(255, 196, 0, 0.08)",
            border: "1px solid rgba(255, 196, 0, 0.28)",
            borderRadius: 6,
            fontSize: 12,
          }}
        >
          The panel will restart — log in again at the server&apos;s IP.
        </div>
      </ConfirmDialog>

      {/* Live progress */}
      <ConfirmDialog
        open={phase.kind === "running" || phase.kind === "starting"}
        title={
          phase.kind === "running" && phase.intent === "clear"
            ? "Reverting to IP-only mode…"
            : "Setting up panel hostname…"
        }
        hideActions
        lockDismiss
        onConfirm={() => {}}
        onCancel={() => {}}
      >
        <p className={styles.muted} style={{ marginBottom: 8 }}>
          Live progress from the agent. The panel will restart at the end —
          keep this dialog open until you see the success card.
        </p>
        {phase.kind === "running" ? (
          <TaskProgress taskId={phase.taskId} onComplete={onTaskComplete} />
        ) : (
          <div className={styles.muted}>Starting task…</div>
        )}
      </ConfirmDialog>

      {/* Final state */}
      <ConfirmDialog
        open={phase.kind === "done"}
        title={
          phase.kind === "done" && phase.status === "succeeded"
            ? phase.intent === "clear"
              ? "Reverted to IP-only mode"
              : "Panel hostname saved"
            : "Setup did not finish cleanly"
        }
        hideActions
        onConfirm={() => {}}
        onCancel={closeDoneDialog}
      >
        {phase.kind === "done" && phase.status === "succeeded" ? (
          <>
            <p>
              The agent finished its work. The panel is restarting now —
              give it a few seconds, then continue at the new URL.
            </p>
            {phase.intent === "save" && (
              <ul>
                <li>
                  New URL:{" "}
                  <span className={styles.code}>
                    {targetUrl ?? `https://${trimmed}/`}
                  </span>
                </li>
                <li>
                  If SSL succeeded, cookies are now Secure; otherwise the
                  panel stays on HTTP and you can issue the cert later with
                  Issue cert in Settings, Panel hostname.
                </li>
                <li>
                  The hostname is registered under{" "}
                  <span className={styles.code}>/domains</span>.
                </li>
                <li>
                  Until the name&apos;s DNS points to this server, open the panel by
                  this server&apos;s IP address as before; it keeps answering there.
                </li>
              </ul>
            )}
          </>
        ) : (
          <p>
            The task finished with status{" "}
            <span className={styles.code}>
              {phase.kind === "done" ? phase.status : "?"}
            </span>
            . The full log is under{" "}
            {phase.kind === "done" ? (
              <a href={`/tasks/${phase.taskId}`} className={styles.code}>
                Tasks
              </a>
            ) : (
              "Tasks"
            )}
            . Nothing destructive happened - you can adjust the hostname and try
            again.
          </p>
        )}
        <div
          className={styles.actions}
          style={{ marginTop: 12, justifyContent: "flex-end", display: "flex", gap: 8 }}
        >
          {targetUrl && (
            <a
              href={targetUrl}
              className={styles.btn}
              target="_blank"
              rel="noopener noreferrer"
            >
              Open new URL
            </a>
          )}
          <button className={styles.btnGhost} onClick={closeDoneDialog}>
            Close
          </button>
        </div>
      </ConfirmDialog>
    </div>
  );
}
