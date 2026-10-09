import { notFound } from "next/navigation";
import { agent, type GitStatusResp } from "@/lib/agent";
import StatusBadge from "@/components/StatusBadge/StatusBadge";
import styles from "@/components/page.module.css";
import {
  startAppAction,
  stopAppAction,
  restartAppAction,
  deleteAppAction,
  getAppRepoUrl,
  updateAppRepoUrlAction,
  getAppEntrypoint,
  updateAppEntrypointAction,
  deployAppAction,
  updateAppAction,
  checkAppSourceStateAction,
  setAppDomainAction,
} from "@/server-actions/apps";
import { freeSlotsFor, listRoutingSlots, slotUnavailableReason } from "@/lib/appSlots";
import AppServiceControls from "./AppServiceControls";
import EnvFilesEditor from "./EnvFilesEditor";
import DeleteAppButton from "./DeleteAppButton";
import RepoUrlForm from "./RepoUrlForm";
import EntrypointForm from "./EntrypointForm";
import DomainForm from "./DomainForm";
import DeployForm from "../new/DeployForm";
import UpdatesPanel from "./UpdatesPanel";

export const dynamic = "force-dynamic";

type PortSiteRow = { domain: string; upstream_port: number };

export default async function AppDetailPage({
  params,
}: {
  params: Promise<{ name: string }>;
}) {
  const { name } = await params;
  let app;
  try {
    app = await agent.apps.get(name);
  } catch {
    notFound();
  }
  if (!app) notFound();

  const start = startAppAction.bind(null, name);
  const stop = stopAppAction.bind(null, name);
  const restart = restartAppAction.bind(null, name);
  const updateRepoUrl = updateAppRepoUrlAction.bind(null, name);
  const initialRepoUrl = (await getAppRepoUrl(name)) ?? "";
  const updateEntrypoint = updateAppEntrypointAction.bind(null, name);
  const initialEntrypoint = await getAppEntrypoint(name);
  const sourceMissing =
    app.source_dir_state === "missing" || app.source_dir_state === "empty";
  const isExternal = app.kind === "external";
  const setDomain = setAppDomainAction.bind(null, name);

  // Routing slots for the Domain picker (panel apps) and the recovery form
  // (external apps). Slots bound to another app or sitting on a quick-install
  // service port are left out entirely, not shown greyed.
  const allSlots = listRoutingSlots();
  const freeSlots = freeSlotsFor(name, allSlots);
  const unavailableSlots = allSlots.flatMap((s) => {
    const reason = slotUnavailableReason(s, name);
    return reason ? [{ domain: s.domain, reason }] : [];
  });
  const currentSlot = app.subdomain
    ? { domain: app.subdomain, port: app.port }
    : null;
  // subdomain '' + port 0 is the marker applyAppRouting writes on unassign.
  const unassigned = !isExternal && !app.subdomain;

  // Project `.env*` files live inside the source dir. Skip the fetch when the
  // folder doesn't exist (the agent would just 404); the component falls back
  // to a client-side load + inline error if this comes back null.
  const initialEnvFiles =
    app.source_dir_state === "missing"
      ? null
      : await agent.apps.listEnvFiles(name).catch(() => null);

  // Snapshot the git state for the Updates section. Skip for external apps
  // (no panel-managed repo URL stored) and skip when there's no code on
  // disk — git would just error.
  let gitStatus: GitStatusResp | undefined;
  if (!isExternal && !sourceMissing) {
    gitStatus = await agent.apps.gitStatus(name).catch(() => undefined);
  }

  // Recovery form needs port-mode sites (routing slots) + root deploy keys —
  // same data shape /apps/new uses. Only fetch when actually rendering
  // the form to keep panel-managed page loads cheap.
  let portSites: PortSiteRow[] = [];
  let rootKeypairs: string[] = [];
  if (isExternal) {
    portSites = freeSlots.map((s) => ({ domain: s.domain, upstream_port: s.port }));
    rootKeypairs = await agent.ssh
      .list("root")
      .then((rows) =>
        rows
          .filter((r) => r.authorized)
          .map((r) => r.keypair_name)
          .filter((n): n is string => !!n),
      )
      .catch(() => []);
  }

  if (isExternal) {
    return (
      <div>
        <div className={styles.head}>
          <div>
            <h1 className={styles.title}>{app.name}</h1>
            <div className={styles.subtitle}>
              <StatusBadge text="external" tone="warn" />{" "}
              <span className={styles.code}>{app.source_dir}</span>{" "}
              <StatusBadge text={app.sub || app.active || "no systemd unit"} />
            </div>
          </div>
        </div>

        <div
          style={{
            marginBottom: 16,
            padding: "12px 14px",
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
            <strong>This app was detected externally.</strong> The folder{" "}
            <span className={styles.code}>{app.source_dir}</span> exists but
            the panel didn&apos;t create it and doesn&apos;t manage its
            systemd unit. Use the form below to bind it to panel management:
            paste a repo URL, pick a routing slot, then run{" "}
            <strong>Pull</strong> → <strong>Install</strong> →{" "}
            <strong>Build</strong> → <strong>Deploy</strong>. Deploy writes a{" "}
            <span className={styles.code}>
              vps-app-{app.name}.service
            </span>{" "}
            unit and the app becomes panel-managed.
            {app.source_dir_state === "empty" && (
              <>
                {" "}
                The folder is currently <strong>empty</strong> — start with{" "}
                <strong>Pull</strong>.
              </>
            )}
          </div>
        </div>

        <div className={styles.section}>
          <h2 className={styles.sectionTitle}>Set up panel management</h2>
          <DeployForm
            action={deployAppAction}
            checkSourceState={checkAppSourceStateAction}
            portSites={portSites}
            sourceMethod="github"
            rootKeypairs={rootKeypairs}
            lockedName={app.name}
            initialRepoUrl={initialRepoUrl}
          />
        </div>

        <div className={styles.section}>
          <h2 className={styles.sectionTitle}>Project env files</h2>
          <EnvFilesEditor name={name} initialData={initialEnvFiles} />
        </div>

        <div className={styles.section}>
          <h2 className={styles.sectionTitle}>Danger zone</h2>
          <DeleteAppButton
            appName={name}
            sourceDir={app.source_dir}
            deleteAction={deleteAppAction}
          />
        </div>
      </div>
    );
  }

  return (
    <div>
      <div className={styles.head}>
        <div>
          <h1 className={styles.title}>{app.name}</h1>
          <div className={styles.subtitle}>
            {unassigned ? (
              <>
                <StatusBadge text="no domain" tone="warn" />{" "}
                <span className={styles.muted}>
                  unloaded - assign a routing slot below
                </span>{" "}
              </>
            ) : (
              <>
                <span className={styles.code}>{app.subdomain}</span>{" "}
                -&gt; <span className={styles.code}>127.0.0.1:{app.port}</span>{" "}
              </>
            )}
            <StatusBadge text={app.sub || app.active || "unknown"} />
          </div>
        </div>
        <div className={styles.actions}>
          <AppServiceControls
            startAction={start}
            stopAction={stop}
            restartAction={restart}
            active={app.active}
            unassigned={unassigned}
          />
        </div>
      </div>

      {sourceMissing && (
        <div
          style={{
            marginBottom: 16,
            padding: "12px 14px",
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
              Source directory is{" "}
              {app.source_dir_state === "missing" ? "missing" : "empty"}.
            </strong>{" "}
            <span className={styles.code}>{app.source_dir}</span>{" "}
            {app.source_dir_state === "missing"
              ? "doesn't exist on disk."
              : "exists but has no files inside."}{" "}
            The systemd unit will fail to start until code is back in place:
            re-deploy it under the same name via{" "}
            <a href={`/apps/new?method=github`}>+ Deploy app</a> (the pull stage
            clones into the empty folder), or copy the code back into the folder.
          </div>
        </div>
      )}

      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>Domain</h2>
        <p className={styles.muted} style={{ marginBottom: 10 }}>
          Which routing slot serves this app. Moving it rewrites{" "}
          <span className={styles.code}>PORT</span> in the systemd unit and
          restarts the app; the old domain answers 502 until another app takes
          it. &quot;No domain&quot; unloads the app entirely.
        </p>
        <DomainForm
          current={currentSlot}
          slots={freeSlots.map((s) => ({ domain: s.domain, port: s.port }))}
          unavailable={unavailableSlots}
          action={setDomain}
        />
      </div>

      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>Updates</h2>
        <UpdatesPanel
          appName={name}
          status={gitStatus}
          sourceMissing={sourceMissing}
          updateAction={updateAppAction}
        />
      </div>

      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>Repository</h2>
        <p className={styles.muted} style={{ marginBottom: 10 }}>
          Where Rebuild (under Updates) and a re-deploy pull this app&apos;s code
          from.
        </p>
        <RepoUrlForm initialUrl={initialRepoUrl} action={updateRepoUrl} />
      </div>

      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>Entrypoint</h2>
        <p className={styles.muted} style={{ marginBottom: 10 }}>
          The command the systemd unit runs. Fix it here if the app was deployed
          with the wrong start command (e.g. it&apos;s running{" "}
          <span className={styles.code}>npm start</span> instead of{" "}
          <span className={styles.code}>node dist/main</span>).
        </p>
        <EntrypointForm
          initialValue={initialEntrypoint}
          action={updateEntrypoint}
        />
      </div>

      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>Project env files</h2>
        {app.source_dir_state === "missing" ? (
          <p className={styles.muted}>
            Source folder is missing - env files appear here once code is in
            place.
          </p>
        ) : (
          <EnvFilesEditor name={name} initialData={initialEnvFiles} />
        )}
      </div>

      <div
        style={{
          display: "grid",
          gridTemplateColumns: "1fr 1fr",
          gap: 16,
          alignItems: "stretch",
        }}
      >
        <div className={styles.section} style={{ marginBottom: 0 }}>
          <h2 className={styles.sectionTitle}>Danger zone</h2>
          <DeleteAppButton
            appName={name}
            sourceDir={app.source_dir}
            deleteAction={deleteAppAction}
          />
        </div>

        <div className={styles.section} style={{ marginBottom: 0 }}>
          <h2 className={styles.sectionTitle}>Logs</h2>
          <p className={styles.muted}>
            Live tail at{" "}
            <a href={`/logs?unit=vps-app-${encodeURIComponent(name)}.service`}>
              /logs?unit=vps-app-{name}.service
            </a>
          </p>
        </div>
      </div>
    </div>
  );
}
