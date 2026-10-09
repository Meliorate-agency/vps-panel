import Link from "next/link";
import { agent, type AppRow, type GitStatusResp } from "@/lib/agent";
import StatusBadge from "@/components/StatusBadge/StatusBadge";
import QuickInstallStrip from "@/components/QuickInstallStrip/QuickInstallStrip";
import {
  quickInstallAction,
  setBindAction,
  getRoundcubeSettingsAction,
  updateRoundcubeSettingsAction,
  getN8nSettingsAction,
  updateN8nSettingsAction,
  updateN8nAction,
  getN8nAutoCheck,
  setN8nAutoCheckAction,
  getPhpMyAdminSettingsAction,
  updatePhpMyAdminSettingsAction,
} from "@/server-actions/quickInstall";
import {
  restartExternalAppAction,
  updateAppAction,
  getAutoCheckUpdates,
  setAutoCheckUpdatesAction,
  checkAppUpdateAction,
  refreshGitStatusAction,
} from "@/server-actions/apps";
import {
  createAppFolderAction,
  deleteAppFolderAction,
  listAppFolders,
  listAppFolderAssignments,
  renameAppFolderAction,
  setAppFolderAction,
  toggleAppFolderCollapsedAction,
} from "@/server-actions/appFolders";
import { getServerAddress } from "@/lib/serverAddress";
import { getSession } from "@/lib/auth";
import { tailscaleCard } from "@/lib/tailscale";
import styles from "@/components/page.module.css";
import DeployMethodButton from "./DeployMethodButton";
import GitStatusBadge from "./GitStatusBadge";
import AutoUpdateCheckToggle from "./AutoUpdateCheckToggle";
import CheckUpdateButton from "./CheckUpdateButton";
import AppFolderSection from "./AppFolderSection";
import MoveToFolderButton from "./MoveToFolderButton";
import NewFolderButton from "./NewFolderButton";

export const dynamic = "force-dynamic";

/** Actions column needs more room for the extra Check button when the
 *  page-load auto-check is off. */
const buildColumns = (autoCheck: boolean) => [
  { key: "name", label: "Name" },
  { key: "source", label: "Source", width: "260px" },
  { key: "subdomain", label: "Subdomain" },
  { key: "port", label: "Port", width: "70px" },
  { key: "git", label: "Git", width: "180px" },
  { key: "status", label: "Status", width: "160px" },
  { key: "actions", label: "", width: autoCheck ? "150px" : "250px" },
];

/**
 * Build the row data for one app. Extracted so every bucket (named
 * folders, ungrouped, external) renders rows with the same cell layout.
 * `folderMeta` is null for external apps (they can't be foldered) and
 * carries the folder picker context for panel-managed apps.
 */
function buildAppRow(
  a: AppRow,
  gitStatuses: Map<string, GitStatusResp>,
  folderMeta: {
    currentFolderId: number | null;
    folders: { id: number; name: string }[];
  } | null,
  autoCheck: boolean,
) {
  const isExternal = a.kind === "external";
  const hasUnit = !!a.active;
  const statusText = a.sub || a.active || (isExternal ? "no unit" : "unknown");
  return {
    name: (
      <Link href={`/apps/${encodeURIComponent(a.name)}`}>
        <strong>{a.name}</strong>
      </Link>
    ),
    source: (
      <div
        style={{
          display: "flex",
          gap: 6,
          alignItems: "center",
          flexWrap: "wrap",
        }}
      >
        {isExternal ? (
          <StatusBadge text="external" tone="warn" />
        ) : (
          <StatusBadge text="panel" tone="ok" />
        )}
        <span className={styles.code}>{a.source_dir || "-"}</span>
        {a.source_dir_state === "missing" && (
          <span title="source_dir doesn't exist on disk">
            <StatusBadge text="folder missing" tone="danger" />
          </span>
        )}
        {a.source_dir_state === "empty" && (
          <span title="source_dir exists but is empty">
            <StatusBadge text="folder empty" tone="warn" />
          </span>
        )}
      </div>
    ),
    subdomain: a.subdomain ? (
      <a
        href={`https://${a.subdomain}`}
        target="_blank"
        rel="noopener noreferrer"
        className={styles.code}
        title={`Open https://${a.subdomain} in a new tab`}
      >
        {a.subdomain}
      </a>
    ) : isExternal ? (
      <span className={styles.code}>-</span>
    ) : (
      <span title="Unassigned - stopped until a routing slot is picked on the app page">
        <StatusBadge text="no domain" tone="warn" />
      </span>
    ),
    port: a.port || "-",
    git: autoCheck ? (
      <GitStatusBadge
        appName={a.name}
        status={gitStatuses.get(a.name)}
        isExternal={isExternal}
        updateAction={updateAppAction}
        refreshAction={refreshGitStatusAction}
      />
    ) : isExternal ? (
      <span className={styles.muted}>—</span>
    ) : (
      <span
        className={styles.muted}
        title="Auto-check on page load is off — use the Check button on this row"
      >
        not checked
      </span>
    ),
    status: <StatusBadge text={statusText} />,
    actions: isExternal ? (
      hasUnit ? (
        <div style={{ display: "flex", gap: 6 }}>
          <form action={restartExternalAppAction.bind(null, a.name)}>
            <button type="submit" className={styles.btnGhost}>
              Restart
            </button>
          </form>
          <Link
            href={`/apps/${encodeURIComponent(a.name)}`}
            className={styles.btnGhost}
          >
            Open
          </Link>
        </div>
      ) : (
        <Link
          href={`/apps/${encodeURIComponent(a.name)}`}
          className={styles.btnGhost}
          title="Set up panel management for this folder"
        >
          Open
        </Link>
      )
    ) : (
      <div style={{ display: "flex", gap: 6, justifyContent: "flex-end" }}>
        {/* Only when the page-load auto-check is off — otherwise the Git
            column's badge already offers the update popover. */}
        {!autoCheck && (
          <CheckUpdateButton
            appName={a.name}
            checkAction={checkAppUpdateAction}
          />
        )}
        <Link
          href={`/apps/${encodeURIComponent(a.name)}`}
          className={styles.btnGhost}
        >
          Open
        </Link>
        {folderMeta && (
          <MoveToFolderButton
            appName={a.name}
            currentFolderId={folderMeta.currentFolderId}
            folders={folderMeta.folders}
            moveAction={setAppFolderAction.bind(null, a.name)}
          />
        )}
      </div>
    ),
  };
}

export default async function AppsPage() {
  // Read both preferences first (local SQLite, sub-ms): the apps-wide
  // toggle gates the per-app git fetches below; the n8n-specific one gates
  // the quick-install registry probe. They're deliberately independent so
  // the operator can keep app checks on without paying for Docker Hub (or
  // vice versa).
  const [autoCheck, n8nAutoCheck] = await Promise.all([
    getAutoCheckUpdates(),
    getN8nAutoCheck(),
  ]);

  const [apps, recipes, addr, folders, folderAssignments, tsStatus, session] = await Promise.all([
    agent.apps.list().catch(() => []),
    // checkUpdates reaches out to Docker Hub for the n8n image tag (6s
    // timeout) — the single slowest thing on this page. Gated by n8n's own
    // auto-check toggle (in its settings popup), NOT the apps-wide one:
    // when off the card just doesn't show its update badge until the
    // operator turns it back on.
    agent.quickInstall.list({ checkUpdates: n8nAutoCheck }).catch(() => []),
    getServerAddress(),
    listAppFolders(),
    listAppFolderAssignments(),
    // Local CLI calls with an 8 s cap on the agent side; answers at once
    // when Tailscale is not installed.
    agent.tailscale.status().catch(() => null),
    getSession(),
  ]);
  const serverAddress = addr.domain ?? addr.ip ?? null;
  // Only the phase and the tailnet IP reach the client: the full status
  // carries the login link, which the dialog fetches for admins only.
  const tailscale = recipes.some((r) => r.slug === "tailscale" && r.installed)
    ? { card: tailscaleCard(tsStatus), canManage: session?.role === "admin" }
    : null;
  const columns = buildColumns(autoCheck);

  // For every panel-managed app, run `git fetch` in parallel and compare
  // local HEAD to upstream so we can show "up to date" vs "N behind" on
  // each row. External apps (no panel-managed unit) are skipped — we have
  // no clean way to know which directory they live in.
  //
  // Skipped entirely when the operator has turned auto-check off: that's
  // one network round trip per app on every single page load, and the
  // per-row Check button covers the same ground on demand.
  const gitStatuses = new Map<string, GitStatusResp>();
  if (autoCheck) {
    const fetches = apps
      .filter((a) => a.kind !== "external" && a.source_dir)
      .map(async (a) => {
        try {
          const status = await agent.apps.gitStatus(a.name);
          gitStatuses.set(a.name, status);
        } catch {
          // leave unset — row renders "—"
        }
      });
    await Promise.all(fetches);
  }

  const orphans = apps.filter(
    (a) =>
      a.kind !== "external" &&
      (a.source_dir_state === "missing" || a.source_dir_state === "empty"),
  );

  // Bucket panel-managed apps by folder_id.
  const panelApps = apps.filter((a) => a.kind !== "external");
  const externalApps = apps.filter((a) => a.kind === "external");
  const folderList = folders.map((f) => ({ id: f.id, name: f.name }));

  const inFolder = (folderId: number) =>
    panelApps.filter((a) => folderAssignments[a.name] === folderId);
  const ungrouped = panelApps.filter(
    (a) =>
      folderAssignments[a.name] === null ||
      folderAssignments[a.name] === undefined,
  );

  return (
    <div>
      <div className={styles.head}>
        <div>
          <h1 className={styles.title}>Apps</h1>
          <div className={styles.subtitle}>
            Node.js apps managed by systemd, served by nginx.
          </div>
          <AutoUpdateCheckToggle
            enabled={autoCheck}
            action={setAutoCheckUpdatesAction}
          />
        </div>
        <div className={styles.actions}>
          <NewFolderButton action={createAppFolderAction} />
          <DeployMethodButton />
        </div>
      </div>

      {orphans.length > 0 && (
        <div
          style={{
            marginBottom: 16,
            padding: "10px 14px",
            background: "rgba(255, 196, 0, 0.08)",
            border: "1px solid rgba(255, 196, 0, 0.32)",
            borderRadius: 8,
            fontSize: 13,
            display: "flex",
            alignItems: "flex-start",
            gap: 10,
          }}
        >
          <span style={{ fontSize: 18, lineHeight: 1 }}>⚠️</span>
          <div>
            <strong>
              {orphans.length === 1
                ? `1 app has no source code on disk`
                : `${orphans.length} apps have no source code on disk`}
            </strong>{" "}
            ({orphans.map((a) => a.name).join(", ")}). Their systemd unit
            won&apos;t start until code is back in place: re-deploy each one under
            the same name from <strong>+ Deploy app</strong> (the pull stage clones
            into the empty folder), or copy the code back into its folder.
          </div>
        </div>
      )}

      {panelApps.length === 0 && externalApps.length === 0 && folders.length === 0 ? (
        <div className={styles.empty}>
          No apps deployed yet. Click <strong>+ Deploy app</strong> to add one.
        </div>
      ) : (
        <>
          {/* Named folders, in their persisted sort order. */}
          {folders.map((f) => {
            const folderApps = inFolder(f.id);
            return (
              <AppFolderSection
                key={f.id}
                folder={{ id: f.id, name: f.name, collapsed: f.collapsed }}
                columns={columns}
                rows={folderApps.map((a) =>
                  buildAppRow(
                    a,
                    gitStatuses,
                    { currentFolderId: f.id, folders: folderList },
                    autoCheck,
                  ),
                )}
                empty="No apps in this folder. Use the folder picker on any panel-managed app to move it here."
                renameAction={renameAppFolderAction.bind(null, f.id)}
                deleteAction={deleteAppFolderAction.bind(null, f.id)}
                toggleCollapsedAction={toggleAppFolderCollapsedAction.bind(null, f.id)}
              />
            );
          })}

          {/* Ungrouped panel-managed apps. Always shown when there's at
              least one, OR when there are no folders yet (so the operator
              sees their apps even without creating folders first). */}
          {(ungrouped.length > 0 || folders.length === 0) && (
            <AppFolderSection
              folder={null}
              fallbackLabel="Ungrouped"
              columns={columns}
              rows={ungrouped.map((a) =>
                buildAppRow(
                  a,
                  gitStatuses,
                  { currentFolderId: null, folders: folderList },
                  autoCheck,
                ),
              )}
              empty={
                folders.length === 0
                  ? "No panel-managed apps yet. Click + Deploy app to add one."
                  : "Every panel-managed app is in a folder."
              }
            />
          )}

          {/* External apps (drift-detected from /srv/apps/). Can't be
              foldered because they have no panel.db row — folder
              assignment lives there. */}
          {externalApps.length > 0 && (
            <AppFolderSection
              folder={null}
              fallbackLabel="External"
              columns={columns}
              rows={externalApps.map((a) =>
                buildAppRow(a, gitStatuses, null, autoCheck),
              )}
              empty="No external apps."
            />
          )}
        </>
      )}

      <div style={{ marginTop: 24 }}>
        <QuickInstallStrip
          recipes={recipes}
          serverAddress={serverAddress}
          installAction={quickInstallAction}
          bindAction={setBindAction}
          getRoundcubeSettings={getRoundcubeSettingsAction}
          updateRoundcubeSettings={updateRoundcubeSettingsAction}
          getN8nSettings={getN8nSettingsAction}
          updateN8nSettings={updateN8nSettingsAction}
          updateN8n={updateN8nAction}
          n8nAutoCheck={n8nAutoCheck}
          setN8nAutoCheck={setN8nAutoCheckAction}
          getPhpMyAdminSettings={getPhpMyAdminSettingsAction}
          updatePhpMyAdminSettings={updatePhpMyAdminSettingsAction}
          showRouteButton={false}
          tailscale={tailscale}
        />
      </div>
    </div>
  );
}
