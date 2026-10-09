"use client";

import { startTransition, useActionState, useEffect, useRef, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import ServiceIcon from "@/components/ServiceIcon/ServiceIcon";
import TaskProgress from "@/components/TaskProgress/TaskProgress";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";
import type { QuickInstallRecipe } from "@/lib/agent";
import type {
  GetN8nSettingsResult,
  GetPhpMyAdminSettingsResult,
  GetRoundcubeSettingsResult,
  QuickInstallResult,
  SetBindResult,
  UpdateN8nResult,
  UpdateN8nSettingsResult,
  UpdatePhpMyAdminSettingsResult,
  UpdateRoundcubeSettingsResult,
} from "@/server-actions/quickInstall";
import shared from "@/components/page.module.css";
import styles from "./QuickInstallStrip.module.css";
import RoundcubeSettings from "./RoundcubeSettings";
import N8nSettings from "./N8nSettings";
import N8nUpdateButton from "./N8nUpdateButton";
import PhpMyAdminSettings from "./PhpMyAdminSettings";
import TailscaleManager from "./TailscaleManager";
import { PHASE_LABEL, phaseTone, type TailscaleCard } from "@/lib/tailscale";

type Props = {
  recipes: QuickInstallRecipe[];
  serverAddress: string | null;
  /** Override the strip's heading (defaults to "Quick install"). */
  heading?: string;
  /** Override the small hint shown next to the heading. */
  hint?: string;
  installAction: (
    prev: QuickInstallResult | null,
    fd: FormData,
  ) => Promise<QuickInstallResult>;
  bindAction: (prev: SetBindResult | null, fd: FormData) => Promise<SetBindResult>;
  /** Roundcube-specific: read current IMAP/SMTP settings from the running
   *  container so the panel UI can show them as editable. */
  getRoundcubeSettings: () => Promise<GetRoundcubeSettingsResult>;
  /** Roundcube-specific: apply edited settings (recreates container,
   *  wipes the cached config volume). */
  updateRoundcubeSettings: (
    prev: UpdateRoundcubeSettingsResult | null,
    fd: FormData,
  ) => Promise<UpdateRoundcubeSettingsResult>;
  /** n8n-specific: read current public-URL env + routing-slot suggestions. */
  getN8nSettings: () => Promise<GetN8nSettingsResult>;
  /** n8n-specific: apply edited public URL (recreates container). */
  updateN8nSettings: (
    prev: UpdateN8nSettingsResult | null,
    fd: FormData,
  ) => Promise<UpdateN8nSettingsResult>;
  /** n8n-specific: pull the latest image + recreate the container. Powers the
   *  "Update" pill that shows top-right of the n8n card when a newer
   *  n8nio/n8n image is available. Optional — only the /apps strip surfaces
   *  n8n, so the /databases strip can omit it. */
  updateN8n?: (
    prev: UpdateN8nResult | null,
    fd: FormData,
  ) => Promise<UpdateN8nResult>;
  /** n8n-specific: whether the page-load Docker Hub probe for a newer
   *  n8nio/n8n image is enabled. Rendered as a small on/off note on the
   *  card; toggled from the settings popup. Deliberately per-app - the
   *  apps-wide auto-check toggle only covers panel-managed git apps.
   *  Optional: strips that never render n8n (/databases) omit both. */
  n8nAutoCheck?: boolean;
  /** n8n-specific: persist the auto-check preference (admin-gated server
   *  action; revalidates /apps). */
  setN8nAutoCheck?: (enabled: boolean) => Promise<void>;
  /** phpMyAdmin-specific: read PMA_ABSOLUTE_URI + port-8081 routing slots. */
  getPhpMyAdminSettings: () => Promise<GetPhpMyAdminSettingsResult>;
  /** phpMyAdmin-specific: apply edited public URL (recreates container).
   *  Fixes the session-cookie-secure mismatch when phpMyAdmin sits behind
   *  the nginx HTTPS proxy. */
  updatePhpMyAdminSettings: (
    prev: UpdatePhpMyAdminSettingsResult | null,
    fd: FormData,
  ) => Promise<UpdatePhpMyAdminSettingsResult>;
  /**
   * Show the "Route →" link on LOCAL-bound web_ui recipes. Default true.
   *
   * On /databases the operator is actively configuring DB tools and
   * /domains routing is part of that flow, so the link is useful. On
   * /apps the strip is just a secondary install surface — adding domain
   * routing belongs on the dedicated /domains page, not as a CTA buried
   * at the bottom of the apps list. Pass `false` from /apps to declutter.
   */
  showRouteButton?: boolean;
  /** Tailscale card state (built on the server from the agent status) and
   *  whether this session may open its dialog (admins only: the dialog shows
   *  the login link). Only /apps passes it. */
  tailscale?: { card: TailscaleCard; canManage: boolean } | null;
};

export default function QuickInstallStrip({
  recipes,
  serverAddress,
  heading = "Quick install",
  hint = "One-click apt + systemd install on the host",
  installAction,
  bindAction,
  getRoundcubeSettings,
  updateRoundcubeSettings,
  getN8nSettings,
  updateN8nSettings,
  updateN8n,
  n8nAutoCheck,
  setN8nAutoCheck,
  getPhpMyAdminSettings,
  updatePhpMyAdminSettings,
  showRouteButton = true,
  tailscale = null,
}: Props) {
  const router = useRouter();
  const [installState, installFormAction, installPending] = useActionState<
    QuickInstallResult | null,
    FormData
  >(installAction, null);
  const [bindState, bindFormAction, bindPending] = useActionState<
    SetBindResult | null,
    FormData
  >(bindAction, null);

  // The actions return as soon as the agent accepts a task, so pending alone
  // re-enabled Install while apt was still running and a second click
  // started a second install. A task counts as running until its
  // TaskProgress reports completion.
  const [finishedTasks, setFinishedTasks] = useState<Set<string>>(new Set());
  const [submittedSlug, setSubmittedSlug] = useState<string | null>(null);
  const installTask = installState?.ok ? installState.data ?? null : null;
  const bindTask = bindState?.ok ? bindState.data ?? null : null;
  const installTaskRunning = !!installTask?.taskId && !finishedTasks.has(installTask.taskId);
  const bindTaskRunning = !!bindTask?.taskId && !finishedTasks.has(bindTask.taskId);
  const installBusy = installPending || installTaskRunning;
  const bindBusy = bindPending || bindTaskRunning;
  const installingSlug = installPending
    ? submittedSlug
    : installTaskRunning
      ? installTask?.slug ?? null
      : null;
  const rebindingSlug = bindTaskRunning ? bindTask?.slug ?? null : null;
  const taskDone = (taskId: string) => {
    setFinishedTasks((prev) => new Set(prev).add(taskId));
    router.refresh();
  };

  if (recipes.length === 0) return null;

  return (
    <div className={styles.wrap}>
      <div className={styles.head}>
        <div className={styles.heading}>{heading}</div>
        <div className={styles.hint}>{hint}</div>
      </div>

      <div className={styles.scroll}>
        {recipes.map((r) => {
          const installingThis = installingSlug === r.slug;
          const reBindingThis = rebindingSlug === r.slug;
          const isPublic = r.bind === "0.0.0.0";
          const url =
            r.installed && r.web_ui && isPublic && serverAddress
              ? `http://${serverAddress}:${r.default_port}`
              : null;

          return (
            <div key={r.slug} className={styles.card}>
              {r.slug === "n8n" &&
                r.installed &&
                r.update_available === true &&
                updateN8n && <N8nUpdateButton updateAction={updateN8n} />}
              <div className={styles.icon}>
                <ServiceIcon slug={r.slug} size={36} />
              </div>
              <div className={styles.body}>
                <div className={styles.name}>{r.name}</div>
                {r.default_port > 0 && <div className={styles.port}>:{r.default_port}</div>}
                {/* Per-app update-check state (currently n8n only). Shown so
                    the operator can tell at a glance why the Update pill
                    is/isn't appearing; flipped from the settings cog. */}
                {r.slug === "n8n" &&
                  r.installed &&
                  n8nAutoCheck !== undefined && (
                    <div
                      className={styles.autoCheckNote}
                      title={
                        n8nAutoCheck
                          ? "Update check on — each page load asks Docker Hub whether a newer n8n image exists. Toggle it in n8n settings (cog)."
                          : "Update check off — no registry call on page load; the Update pill won't appear. Toggle it in n8n settings (cog)."
                      }
                    >
                      <span
                        className={`${styles.autoCheckDot} ${
                          n8nAutoCheck ? "" : styles.autoCheckDotOff
                        }`}
                      />
                      update check {n8nAutoCheck ? "on" : "off"}
                    </div>
                  )}
                {/* Tailscale: this server's tailnet address where other
                    cards show their port, and the connection state. */}
                {r.slug === "tailscale" && r.installed && tailscale && (
                  <>
                    {tailscale.card.ip && <div className={styles.port}>{tailscale.card.ip}</div>}
                    <div className={styles.autoCheckNote}>
                      <span
                        className={`${styles.autoCheckDot} ${
                          phaseTone(tailscale.card.phase) === "warn"
                            ? styles.dotWarn
                            : phaseTone(tailscale.card.phase) === "muted"
                              ? styles.autoCheckDotOff
                              : ""
                        }`}
                      />
                      {PHASE_LABEL[tailscale.card.phase]}
                    </div>
                  </>
                )}
              </div>

              <div className={styles.action}>
                {!r.installed ? (
                  <form
                    action={installFormAction}
                    onSubmit={() => setSubmittedSlug(r.slug)}
                    className={styles.fullRow}
                  >
                    <input type="hidden" name="slug" value={r.slug} />
                    <button
                      type="submit"
                      className={styles.installBtn}
                      disabled={installBusy}
                    >
                      {installingThis ? "Installing..." : "Install"}
                    </button>
                  </form>
                ) : r.slug === "tailscale" && tailscale?.canManage ? (
                  <TailscaleManager card={tailscale.card} />
                ) : r.web_ui ? (
                  <>
                    <form action={bindFormAction} className={styles.bindForm}>
                      <input type="hidden" name="slug" value={r.slug} />
                      <select
                        name="bind"
                        defaultValue={r.bind ?? "127.0.0.1"}
                        className={styles.bindSelect}
                        disabled={bindBusy}
                        onChange={(e) => e.currentTarget.form?.requestSubmit()}
                        title={
                          "Container listen interface — NOT a subdomain.\n" +
                          "LOCAL: only reachable from this host (use Route to add a subdomain via nginx).\n" +
                          "PUBLIC: reachable on the public IP at the raw port (UFW must allow it)."
                        }
                      >
                        <option value="127.0.0.1">Local 127.0.0.1</option>
                        <option value="0.0.0.0">Public 0.0.0.0</option>
                      </select>
                    </form>
                    {url ? (
                      <a
                        className={styles.openBtn}
                        href={url}
                        target="_blank"
                        rel="noreferrer noopener"
                      >
                        Open
                      </a>
                    ) : showRouteButton ? (
                      // LOCAL bind means the container only listens on
                      // 127.0.0.1, so the only way to reach it from the
                      // outside is via an nginx vhost on this host that
                      // proxies a subdomain to 127.0.0.1:<port>. The "Route"
                      // link deep-links to /domains?port=<port> which
                      // pre-fills the Add domain form with Mode = "Reverse
                      // proxy to port" and the upstream port already set —
                      // the operator just types the subdomain and submits.
                      // Solves the "I bound to LOCAL and now nothing
                      // works" confusion the previous UI baked in.
                      <Link
                        href={`/domains?port=${r.default_port}`}
                        className={styles.localBadge}
                        title={`Bound to 127.0.0.1 — not directly reachable. Click to add an nginx subdomain proxying to 127.0.0.1:${r.default_port}.`}
                        style={{ textDecoration: "none" }}
                      >
                        {reBindingThis ? "..." : "Route →"}
                      </Link>
                    ) : null}
                    {r.slug === "n8n" && (
                      <N8nSettings
                        getAction={getN8nSettings}
                        updateAction={updateN8nSettings}
                        autoCheck={n8nAutoCheck}
                        setAutoCheck={setN8nAutoCheck}
                      />
                    )}
                    {r.slug === "phpmyadmin" && (
                      <PhpMyAdminSettings
                        getAction={getPhpMyAdminSettings}
                        updateAction={updatePhpMyAdminSettings}
                      />
                    )}
                  </>
                ) : r.slug === "roundcube" ? (
                  <>
                    <span className={styles.installedBadge}>Installed</span>
                    <RoundcubeSettings
                      getAction={getRoundcubeSettings}
                      updateAction={updateRoundcubeSettings}
                    />
                  </>
                ) : (r.slug === "mariadb" || r.slug === "postgresql") &&
                  r.bind !== null ? (
                  // Systemd-managed DB engines (not docker). Same
                  // LOCAL/PUBLIC dropdown as the web UIs but the
                  // confirmation flow is harder: a database has no
                  // built-in login wall, so flipping to PUBLIC instantly
                  // exposes the engine to the internet (UFW is opened
                  // server-side). One misclick = bad day. Hence the
                  // window.confirm gate + reverting the select if the
                  // operator backs out.
                  <DbBindForm
                    engine={r.slug}
                    currentBind={r.bind ?? "127.0.0.1"}
                    port={r.default_port}
                    pending={bindBusy}
                    isReBindingThis={reBindingThis}
                    bindFormAction={bindFormAction}
                  />
                ) : (
                  <span className={styles.installedBadge}>Installed</span>
                )}
              </div>
            </div>
          );
        })}
      </div>

      {installState?.ok === false && (
        <div className={shared.error} style={{ marginTop: 12 }}>
          {installState.error}
        </div>
      )}

      {installState?.ok && installState.data?.taskId && (
        <div className={shared.section}>
          <h2 className={shared.sectionTitle}>Installing {installState.data.slug}</h2>
          <TaskProgress
            key={installState.data.taskId}
            taskId={installState.data.taskId}
            onComplete={() => taskDone(installState.data!.taskId!)}
          />
        </div>
      )}

      {bindState?.ok === false && (
        <div className={shared.error} style={{ marginTop: 12 }}>
          {bindState.error}
        </div>
      )}

      {bindState?.ok && bindState.data?.taskId && (
        <div className={shared.section}>
          <h2 className={shared.sectionTitle}>
            Rebinding {bindState.data.slug} to {bindState.data.bind}
          </h2>
          <TaskProgress
            key={bindState.data.taskId}
            taskId={bindState.data.taskId}
            onComplete={() => taskDone(bindState.data!.taskId!)}
          />
        </div>
      )}
    </div>
  );
}

/**
 * Bind dropdown for the systemd-managed DB engines (MariaDB, PostgreSQL).
 *
 * Why a dedicated component instead of reusing the inline `<form>` block
 * the docker web-UIs use: a database engine has no built-in login wall,
 * so a misclick to PUBLIC instantly exposes auth attempts from the
 * entire internet. We need a confirmation step the docker GUIs don't.
 *
 * Implementation: a controlled `<select>` whose `LOCAL → PUBLIC` change
 * opens the app's styled `ConfirmDialog` (centered, danger tone) instead
 * of a native `window.confirm`. Submission is dispatched imperatively via
 * the bound server action so the dropdown can stay on its current value
 * until the operator actually confirms, and snap back if they cancel. The
 * select is re-synced to the server's truth whenever the live bind changes
 * AND after any in-flight toggle settles, so a failed toggle can never
 * leave the dropdown showing a state that wasn't applied (critical for the
 * LOCAL direction: showing "local" while the port is still open would be
 * dangerously misleading).
 */
function DbBindForm({
  engine,
  currentBind,
  port,
  pending,
  isReBindingThis,
  bindFormAction,
}: {
  engine: "mariadb" | "postgresql";
  currentBind: string;
  port: number;
  pending: boolean;
  isReBindingThis: boolean;
  bindFormAction: (fd: FormData) => void;
}) {
  const engineName = engine === "mariadb" ? "MariaDB" : "PostgreSQL";
  const [value, setValue] = useState(currentBind);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const wasPending = useRef(pending);

  // Sync to the server's truth when the live bind changes (successful
  // toggle revalidates /apps + /databases).
  useEffect(() => {
    setValue(currentBind);
  }, [currentBind]);

  // After any toggle settles (pending true → false), re-assert the server
  // value. Catches the failure path where the action errored and
  // currentBind never moved, so an optimistic value would otherwise stick.
  useEffect(() => {
    if (wasPending.current && !pending) setValue(currentBind);
    wasPending.current = pending;
  }, [pending, currentBind]);

  const submit = (bind: string) => {
    const fd = new FormData();
    fd.set("slug", engine);
    fd.set("bind", bind);
    setValue(bind); // optimistic; reconciled by the effects above
    // Inside a transition, or pending never turns true: the select stayed
    // enabled mid-toggle and the settle effect above never ran.
    startTransition(() => bindFormAction(fd));
  };

  const onSelect = (e: React.ChangeEvent<HTMLSelectElement>) => {
    const next = e.target.value;
    if (next === "0.0.0.0" && currentBind !== "0.0.0.0") {
      // Defer the dangerous transition to the styled confirm. The select is
      // controlled, so not calling setValue keeps it visually on the
      // current bind until the operator confirms.
      setConfirmOpen(true);
      return;
    }
    submit(next);
  };

  return (
    <>
      <div className={styles.bindForm} style={{ width: "100%" }}>
        <select
          value={value}
          className={styles.bindSelect}
          disabled={pending}
          onChange={onSelect}
          title={
            `${engineName} listen interface.\n` +
            `LOCAL: bound to 127.0.0.1 only (default). Reach from this host or via the docker bridge for phpMyAdmin.\n` +
            `PUBLIC: bound to 0.0.0.0 + UFW opens ${port}/tcp. Reachable from any machine - make sure users have strong passwords.`
          }
        >
          <option value="127.0.0.1">Local 127.0.0.1</option>
          <option value="0.0.0.0">Public 0.0.0.0</option>
        </select>
        {isReBindingThis && (
          <span className={styles.localBadge} style={{ marginLeft: 6 }}>
            …
          </span>
        )}
      </div>

      <ConfirmDialog
        open={confirmOpen}
        title={`Expose ${engineName} to the public internet?`}
        confirmLabel={`Expose ${engineName}`}
        cancelLabel="Keep local"
        tone="danger"
        pending={pending}
        onConfirm={() => {
          submit("0.0.0.0");
          setConfirmOpen(false);
        }}
        onCancel={() => setConfirmOpen(false)}
      >
        <p style={{ marginTop: 0 }}>
          Port <strong>{port}</strong> will be opened in UFW. Anyone with TCP
          reach to this host will be able to attempt to authenticate against{" "}
          {engineName}.
        </p>
        <p style={{ marginBottom: 6 }}>Before continuing, make sure:</p>
        <ul style={{ margin: "0 0 4px", paddingLeft: 18 }}>
          <li>Every database user has a strong password</li>
          <li>No users have weak default credentials</li>
          <li>
            {engine === "postgresql"
              ? "The panel will append a host-all-0.0.0.0/0 line to pg_hba.conf"
              : "No '%'-host accounts have weak passwords"}
          </li>
        </ul>
      </ConfirmDialog>
    </>
  );
}
