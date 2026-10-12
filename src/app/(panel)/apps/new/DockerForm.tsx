"use client";

import Link from "next/link";
import { useRouter } from "next/navigation";
import { useActionState, useEffect, useRef, useState, useTransition } from "react";
import TaskProgress from "@/components/TaskProgress/TaskProgress";
import type { DockerImageInfo, DockerStatus } from "@/lib/agent";
import styles from "@/components/page.module.css";

type Result = { ok: true; data?: { taskId: string } } | { ok: false; error: string };
type InfoResult = { ok: true; data?: DockerImageInfo } | { ok: false; error: string };
type InstallResult = { ok: true; data: { taskId: string; slug: string } } | { ok: false; error: string };
type PortSite = { domain: string; upstream_port: number };

/** The form's fields as strings, as the inputs hold them. */
export type DockerFormValues = {
  /** "image": pulled from a registry. "git": built from a Dockerfile. */
  source: "image" | "git";
  image: string;
  repo_url: string;
  context: string;
  dockerfile: string;
  build_args: string;
  container_port: string;
  volumes: string;
  memory_mb: string;
  cpus: string;
  env: string;
};

type Props = {
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
  /** What a pulled registry image declares. */
  imageInfo: (image: string) => Promise<InfoResult>;
  /** What an app's last build from Git declares. */
  buildInfo: (name: string) => Promise<InfoResult>;
  docker: DockerStatus;
} & (
  | {
      mode: "new";
      portSites: PortSite[];
      /** Host aliases with a deploy key on root, for private repositories. */
      rootKeypairs: string[];
      installDocker: (prev: InstallResult | null, fd: FormData) => Promise<InstallResult>;
    }
  | {
      mode: "edit";
      name: string;
      /** The app's routing slot; null while it has no domain. */
      slot: { domain: string; port: number } | null;
      initial: DockerFormValues;
    }
);

const EMPTY: DockerFormValues = {
  source: "image",
  image: "",
  repo_url: "",
  context: ".",
  dockerfile: "Dockerfile",
  build_args: "",
  container_port: "",
  volumes: "",
  memory_mb: "1024",
  cpus: "1",
  env: "",
};

/**
 * Deploy a Docker app (mode "new", on /apps/new?method=docker), or change a
 * Docker app's settings and redeploy it (mode "edit", on its page). Submits
 * to deployAppAction with runtime=docker.
 * - From a registry: Pull fetches the image and reads what it declares
 *   (port, folders) into the empty fields; Deploy starts it.
 * - From Git: Pull clones the repository into /srv/apps/<name>, Build runs
 *   its Dockerfile (and reads what the result declares), Deploy starts it.
 *   Saving changed build settings in edit mode pulls and builds again.
 *
 * The form is submitted by hand (FormData from the form + the clicked
 * button's step), not through `<form action>`, so React never resets the
 * fields after a step.
 */
export default function DockerForm(props: Props) {
  const router = useRouter();
  const editing = props.mode === "edit";
  const initial = editing ? props.initial : EMPTY;
  const [state, dispatch, pending] = useActionState<Result | null, FormData>(props.action, null);
  const [, startTransition] = useTransition();
  const [values, setValues] = useState<DockerFormValues>(initial);
  const [name, setName] = useState(editing ? props.name : "");
  const portSites = props.mode === "new" ? props.portSites : [];
  const [slotKey, setSlotKey] = useState(portSites[0] ? `${portSites[0].domain}|${portSites[0].upstream_port}` : "");
  const [info, setInfo] = useState<DockerImageInfo | null>(null);
  // A build of this app exists (from Git): Deploy can start it.
  const [built, setBuilt] = useState(editing);
  const lastStep = useRef<string>("deploy");
  const fromGit = values.source === "git";

  const slot =
    props.mode === "edit"
      ? props.slot
      : (() => {
          const s = portSites.find((p) => `${p.domain}|${p.upstream_port}` === slotKey);
          return s ? { domain: s.domain, port: s.upstream_port } : null;
        })();

  const set = (key: keyof DockerFormValues) => (e: { target: { value: string } }) =>
    setValues((v) => ({ ...v, [key]: e.target.value }));

  // Fill the port and folders from what the image declares, without
  // overwriting anything the operator typed.
  const applyInfo = (i: DockerImageInfo) => {
    setInfo(i);
    setValues((v) => ({
      ...v,
      container_port: v.container_port || (i.exposed_ports[0] ? String(i.exposed_ports[0]) : ""),
      volumes: v.volumes.trim() ? v.volumes : i.volumes.join("\n"),
    }));
  };

  // An image already on the server, or an earlier build of this app,
  // describes itself without a Pull or Build. applyInfo only calls state
  // setters, so it is not a dependency.
  const { imageInfo, buildInfo } = props;
  const probe = fromGit ? name : values.image.trim();
  useEffect(() => {
    setInfo(null);
    if (!probe || (fromGit && probe.length < 2)) return;
    let cancelled = false;
    const t = setTimeout(async () => {
      const r = fromGit ? await buildInfo(probe) : await imageInfo(probe);
      if (cancelled || !r.ok || !r.data) return;
      applyInfo(r.data);
      if (fromGit) setBuilt(true);
    }, 600);
    return () => {
      cancelled = true;
      clearTimeout(t);
    };
  }, [probe, fromGit, imageInfo, buildInfo]);

  const buildChanged =
    values.context !== initial.context ||
    values.dockerfile !== initial.dockerfile ||
    values.build_args !== initial.build_args;

  const onSubmit = (e: React.FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    const submitter = (e.nativeEvent as SubmitEvent).submitter as HTMLButtonElement | null;
    const fd = new FormData(e.currentTarget);
    let step = submitter?.value || "deploy";
    // New build settings only take effect in a new build.
    if (editing && fromGit && step === "deploy" && buildChanged) step = "all";
    fd.set("step", step);
    lastStep.current = step;
    startTransition(() => dispatch(fd));
  };

  if (props.mode === "new" && !props.docker.installed) {
    return <InstallDocker action={props.installDocker} />;
  }
  if (!props.docker.running) {
    return (
      <div className={styles.notice}>
        Docker is installed but its service is not answering. Start{" "}
        <span className={styles.code}>docker.service</span> on{" "}
        <Link href="/services">/services</Link>, then come back.
      </div>
    );
  }
  if (props.mode === "new" && portSites.length === 0) {
    return (
      <div className={styles.notice}>
        <p style={{ marginTop: 0 }}>
          No free routing slot. A Docker app is served on one of your domains through a
          port-mode site.
        </p>
        <p style={{ marginBottom: 0 }}>
          Go to <Link href="/domains" className={styles.code}>/domains</Link>, add a site with{" "}
          <strong>Mode = Reverse proxy to port</strong> and any free port (e.g. 7010), then come
          back here.
        </p>
      </div>
    );
  }

  const hasPort = Number(values.container_port) > 0;
  const ready = !pending && !!slot && !!name;
  const canPull = ready && (fromGit ? !!values.repo_url.trim() : !!values.image.trim());
  const canDeploy = ready && hasPort && (fromGit ? built : !!values.image.trim());
  const busy = (step: string) => pending && lastStep.current === step;
  const keypairs = props.mode === "new" ? props.rootKeypairs : [];

  return (
    <form onSubmit={onSubmit}>
      <input type="hidden" name="runtime" value="docker" />
      <input type="hidden" name="source" value={values.source} />
      <input type="hidden" name="subdomain" value={slot?.domain ?? ""} />
      <input type="hidden" name="port" value={slot ? String(slot.port) : ""} />
      {editing && <input type="hidden" name="name" value={name} />}
      {editing && fromGit && <input type="hidden" name="repo_url" value={values.repo_url} />}

      {props.mode === "new" && (
        <>
          <div role="radiogroup" aria-label="Where the image comes from" style={{ display: "flex", gap: 8, marginBottom: 14, flexWrap: "wrap" }}>
            {(
              [
                ["image", "Image from a registry", "Docker Hub, GHCR..."],
                ["git", "Build from Git", "your repo's Dockerfile"],
              ] as const
            ).map(([value, label, sub]) => (
              <button
                key={value}
                type="button"
                role="radio"
                aria-checked={values.source === value}
                className={values.source === value ? styles.btn : styles.btnGhost}
                onClick={() => {
                  setValues((v) => ({ ...v, source: value }));
                  setBuilt(false);
                }}
              >
                {label} <span style={{ opacity: 0.7, fontWeight: 400 }}>· {sub}</span>
              </button>
            ))}
          </div>

          <div className={styles.formRow}>
            <label className={styles.field}>
              App name
              <input
                name="name"
                className={styles.input}
                required
                pattern="[a-z0-9][a-z0-9-]{1,30}[a-z0-9]"
                placeholder={fromGit ? "myapp" : "uptime"}
                value={name}
                onChange={(e) =>
                  setName(e.target.value.toLowerCase().replace(/[^a-z0-9-]/g, "").slice(0, 32))
                }
                autoComplete="off"
              />
            </label>
            <label className={styles.field}>
              Domain (routing slot from /domains)
              <select
                className={styles.select}
                required
                value={slotKey}
                onChange={(e) => setSlotKey(e.target.value)}
              >
                {portSites.map((s) => (
                  <option key={`${s.domain}|${s.upstream_port}`} value={`${s.domain}|${s.upstream_port}`}>
                    {s.domain} → 127.0.0.1:{s.upstream_port}
                  </option>
                ))}
              </select>
              <span className={styles.muted} style={{ fontSize: 12, marginTop: 4 }}>
                The container is published on this port, on 127.0.0.1 only. nginx serves the
                domain and its certificate; nothing else is opened to the internet.
              </span>
            </label>
          </div>
        </>
      )}

      {editing && !slot && (
        <div className={styles.notice} style={{ marginBottom: 12 }}>
          Assign a domain in the Domain section first: the container is published on its port.
        </div>
      )}

      {fromGit ? (
        <>
          {!editing && (
            <label className={styles.field} style={{ marginBottom: 12 }}>
              Repository URL
              <input
                name="repo_url"
                className={styles.input}
                placeholder={keypairs[0] ? `git@${keypairs[0]}:owner/repo.git` : "https://github.com/owner/repo.git"}
                value={values.repo_url}
                onChange={set("repo_url")}
                autoComplete="off"
                spellCheck={false}
              />
              <span className={styles.muted} style={{ fontSize: 12, marginTop: 4, lineHeight: 1.55 }}>
                Public: <span className={styles.code}>https://github.com/owner/repo.git</span>.
                Private: <span className={styles.code}>git@&lt;alias&gt;:owner/repo.git</span> with a
                deploy key made on <Link href="/ssh-keys">/ssh-keys</Link> for the{" "}
                <strong>root</strong> user. Cloned into{" "}
                <span className={styles.code}>/srv/apps/{name || "<name>"}</span>.
                {keypairs.length > 0 && (
                  <span style={{ display: "block", marginTop: 6 }}>
                    Aliases on root:{" "}
                    {keypairs.map((k) => (
                      <button
                        key={k}
                        type="button"
                        className={styles.code}
                        style={{ marginRight: 6, cursor: "pointer", border: "1px solid var(--border)", background: "var(--bg-elev-2)", color: "var(--accent)" }}
                        onClick={() =>
                          setValues((v) => {
                            const m = v.repo_url.match(/^git@[^:]+:(.*)$/);
                            return { ...v, repo_url: `git@${k}:${m ? m[1] : "owner/repo.git"}` };
                          })
                        }
                      >
                        {k}
                      </button>
                    ))}
                  </span>
                )}
              </span>
            </label>
          )}
          {editing && (
            <p className={styles.muted} style={{ fontSize: 13, marginTop: 0 }}>
              Built from <span className={styles.code}>{values.repo_url || "(no repository URL)"}</span>{" "}
              in <span className={styles.code}>/srv/apps/{name}</span>. Change the URL under
              Repository.
            </p>
          )}
          <div className={styles.formRow}>
            <label className={styles.field}>
              Build folder
              <input
                name="context"
                className={styles.input}
                placeholder="."
                value={values.context}
                onChange={set("context")}
                spellCheck={false}
              />
              <span className={styles.muted} style={{ fontSize: 12, marginTop: 4 }}>
                The folder of the repository sent to the build. <span className={styles.code}>.</span>{" "}
                is the whole repository; a monorepo might use{" "}
                <span className={styles.code}>apps/web</span>.
              </span>
            </label>
            <label className={styles.field}>
              Dockerfile
              <input
                name="dockerfile"
                className={styles.input}
                placeholder="Dockerfile"
                value={values.dockerfile}
                onChange={set("dockerfile")}
                spellCheck={false}
              />
              <span className={styles.muted} style={{ fontSize: 12, marginTop: 4 }}>
                Inside the build folder, as in Docker Compose.
              </span>
            </label>
          </div>
          <label className={styles.field} style={{ marginBottom: 12 }}>
            Build variables
            <textarea
              name="build_args"
              className={styles.input}
              rows={2}
              placeholder="NEXT_PUBLIC_API_URL=https://api.example.com"
              value={values.build_args}
              onChange={set("build_args")}
              spellCheck={false}
              style={{ fontFamily: "var(--mono)", fontSize: 13 }}
            />
            <span className={styles.muted} style={{ fontSize: 12, marginTop: 4 }}>
              <span className={styles.code}>KEY=value</span> per line, for the Dockerfile&apos;s{" "}
              <span className={styles.code}>ARG</span> lines. They are stored in the image, so
              keep secrets in the environment variables below.
            </span>
          </label>
          {info && (
            <p style={{ fontSize: 12, marginTop: -4 }}>
              The build declares{" "}
              {info.exposed_ports.length ? (
                <>port <span className={styles.code}>{info.exposed_ports.join(", ")}</span></>
              ) : (
                "no port"
              )}
              {info.volumes.length > 0 && (
                <>
                  {" "}and folders <span className={styles.code}>{info.volumes.join(", ")}</span>
                </>
              )}
              , runs as <span className={styles.code}>{info.user || "root"}</span>.
            </p>
          )}
        </>
      ) : (
        <label className={styles.field} style={{ marginBottom: 12 }}>
          Image
          <input
            name="image"
            className={styles.input}
            required
            placeholder="louislam/uptime-kuma:1"
            value={values.image}
            onChange={set("image")}
            autoComplete="off"
            spellCheck={false}
          />
          <span className={styles.muted} style={{ fontSize: 12, marginTop: 4 }}>
            From Docker Hub (<span className={styles.code}>nginx:1.27</span>) or another public
            registry (<span className={styles.code}>ghcr.io/owner/app:tag</span>). Prefer a
            version tag over <span className={styles.code}>latest</span>. The image runs as it
            was built, so only use images whose publisher you trust.
          </span>
          {info && (
            <span style={{ fontSize: 12, marginTop: 6 }}>
              Image declares{" "}
              {info.exposed_ports.length ? (
                <>port <span className={styles.code}>{info.exposed_ports.join(", ")}</span></>
              ) : (
                "no port"
              )}
              {info.volumes.length > 0 && (
                <>
                  {" "}and folders <span className={styles.code}>{info.volumes.join(", ")}</span>
                </>
              )}
              , runs as <span className={styles.code}>{info.user || "root"}</span>.
            </span>
          )}
        </label>
      )}

      <div className={styles.formRow}>
        <label className={styles.field}>
          Container port
          <input
            name="container_port"
            className={styles.input}
            type="number"
            min={1}
            max={65535}
            placeholder="3000"
            value={values.container_port}
            onChange={set("container_port")}
          />
          <span className={styles.muted} style={{ fontSize: 12, marginTop: 4 }}>
            The port the app listens on inside the container.
            {fromGit && !hasPort && " Build fills it from the Dockerfile's EXPOSE."}
          </span>
        </label>
        <label className={styles.field}>
          Memory limit (MB)
          <input
            name="memory_mb"
            className={styles.input}
            type="number"
            min={0}
            value={values.memory_mb}
            onChange={set("memory_mb")}
          />
          <span className={styles.muted} style={{ fontSize: 12, marginTop: 4 }}>
            0 = no limit.
          </span>
        </label>
        <label className={styles.field}>
          CPU limit
          <input
            name="cpus"
            className={styles.input}
            type="number"
            min={0}
            step={0.1}
            value={values.cpus}
            onChange={set("cpus")}
          />
          <span className={styles.muted} style={{ fontSize: 12, marginTop: 4 }}>
            Cores, e.g. 0.5. 0 = no limit.
          </span>
        </label>
      </div>

      <label className={styles.field} style={{ marginBottom: 12 }}>
        Folders to keep
        <textarea
          name="volumes"
          className={styles.input}
          rows={2}
          placeholder="/app/data"
          value={values.volumes}
          onChange={set("volumes")}
          spellCheck={false}
          style={{ fontFamily: "var(--mono)", fontSize: 13 }}
        />
        <span className={styles.muted} style={{ fontSize: 12, marginTop: 4 }}>
          One folder inside the container per line. Each is kept in a Docker volume across
          restarts and updates and included in backups. Everything else in the container starts
          fresh on every update.
        </span>
      </label>

      <label className={styles.field} style={{ marginBottom: 12 }}>
        Environment variables
        <textarea
          name="env"
          className={styles.input}
          rows={4}
          placeholder={"TZ=Europe/Berlin\nDATABASE_URL=postgres://user:pass@host.docker.internal:5432/app"}
          value={values.env}
          onChange={set("env")}
          spellCheck={false}
          style={{ fontFamily: "var(--mono)", fontSize: 13 }}
        />
        <span className={styles.muted} style={{ fontSize: 12, marginTop: 4 }}>
          <span className={styles.code}>KEY=value</span>, one per line, given to the container
          when it starts. This server is <span className={styles.code}>host.docker.internal</span>{" "}
          from inside the container (MariaDB and PostgreSQL answer there once phpMyAdmin or
          pgAdmin is installed).
        </span>
      </label>

      {state?.ok === false && <div className={styles.error}>{state.error}</div>}

      <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
        {!editing && (
          <button
            type="submit"
            name="step"
            value="pull"
            className={styles.btnGhost}
            disabled={!canPull}
            title={fromGit ? "Clone or pull the repository" : "Download the image and read its port and folders"}
          >
            {busy("pull") ? "Pulling…" : "Pull"}
          </button>
        )}
        {!editing && fromGit && (
          <button
            type="submit"
            name="step"
            value="build"
            className={styles.btnGhost}
            disabled={!ready}
            title="Run docker build on the pulled code; the running app is not touched"
          >
            {busy("build") ? "Building…" : "Build"}
          </button>
        )}
        <button
          type="submit"
          name="step"
          value="deploy"
          className={styles.btn}
          disabled={!canDeploy}
          title={
            canDeploy
              ? "Write the unit and start the container"
              : fromGit && !built
                ? "Pull and Build first"
                : "Fill in the name and the container port"
          }
        >
          {busy("deploy") || busy("all")
            ? "Deploying…"
            : editing
              ? fromGit && buildChanged
                ? "Save, rebuild and redeploy"
                : "Save and redeploy"
              : "Deploy"}
        </button>
      </div>

      {state?.ok && state.data?.taskId && (
        <TaskProgress
          key={state.data.taskId}
          taskId={state.data.taskId}
          onComplete={async (s) => {
            if (s !== "succeeded") return;
            const step = lastStep.current;
            if (step === "pull" && !fromGit) {
              const r = await imageInfo(values.image.trim());
              if (r.ok && r.data) applyInfo(r.data);
              return;
            }
            if (step === "build") {
              setBuilt(true);
              const r = await buildInfo(name);
              if (r.ok && r.data) applyInfo(r.data);
              return;
            }
            if (step === "pull") return;
            if (editing) {
              router.refresh();
            } else {
              router.push(`/apps/${encodeURIComponent(name)}`);
              router.refresh();
            }
          }}
        />
      )}
    </form>
  );
}

/** Shown instead of the form until Docker is on the server. */
function InstallDocker({
  action,
}: {
  action: (prev: InstallResult | null, fd: FormData) => Promise<InstallResult>;
}) {
  const router = useRouter();
  const [state, formAction, pending] = useActionState<InstallResult | null, FormData>(action, null);
  return (
    <div className={styles.notice}>
      <p style={{ marginTop: 0 }}>
        Docker is not installed on this server yet. Installing it takes a minute and does not
        touch your other apps.
      </p>
      <form action={formAction}>
        <input type="hidden" name="slug" value="docker" />
        <button type="submit" className={styles.btn} disabled={pending || state?.ok === true}>
          {pending ? "Starting…" : "Install Docker"}
        </button>
      </form>
      {state?.ok === false && <div className={styles.error}>{state.error}</div>}
      {state?.ok && (
        <TaskProgress
          taskId={state.data.taskId}
          onComplete={(s) => {
            if (s === "succeeded") router.refresh();
          }}
        />
      )}
    </div>
  );
}
