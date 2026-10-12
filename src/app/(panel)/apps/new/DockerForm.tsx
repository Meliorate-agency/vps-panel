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
  image: string;
  container_port: string;
  volumes: string;
  memory_mb: string;
  cpus: string;
  env: string;
};

type Props = {
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
  imageInfo: (image: string) => Promise<InfoResult>;
  docker: DockerStatus;
} & (
  | {
      mode: "new";
      portSites: PortSite[];
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
  image: "",
  container_port: "",
  volumes: "",
  memory_mb: "1024",
  cpus: "1",
  env: "",
};

/**
 * Deploy a Docker image as an app (mode "new", on /apps/new?method=docker),
 * or change a Docker app's settings and redeploy it (mode "edit", on its
 * page). Submits to deployAppAction with runtime=docker: Pull fetches the
 * image and reads what it declares (port, folders) into the empty fields,
 * Deploy writes the unit and starts the container.
 *
 * The form is submitted by hand (FormData from the form + the clicked
 * button's step), not through `<form action>`, so React never resets the
 * fields after a step.
 */
export default function DockerForm(props: Props) {
  const router = useRouter();
  const editing = props.mode === "edit";
  const [state, dispatch, pending] = useActionState<Result | null, FormData>(props.action, null);
  const [, startTransition] = useTransition();
  const [values, setValues] = useState<DockerFormValues>(editing ? props.initial : EMPTY);
  const [name, setName] = useState(editing ? props.name : "");
  const portSites = props.mode === "new" ? props.portSites : [];
  const [slotKey, setSlotKey] = useState(portSites[0] ? `${portSites[0].domain}|${portSites[0].upstream_port}` : "");
  const [info, setInfo] = useState<DockerImageInfo | null>(null);
  const lastStep = useRef<string>("deploy");

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

  // An image already on the server describes itself without a Pull.
  const { imageInfo } = props;
  useEffect(() => {
    const image = values.image.trim();
    setInfo(null);
    if (!image) return;
    let cancelled = false;
    const t = setTimeout(async () => {
      const r = await imageInfo(image);
      if (!cancelled && r.ok && r.data) applyInfo(r.data);
    }, 600);
    return () => {
      cancelled = true;
      clearTimeout(t);
    };
    // applyInfo only calls state setters, so it is not a dependency.
  }, [values.image, imageInfo]);

  const onSubmit = (e: React.FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    const submitter = (e.nativeEvent as SubmitEvent).submitter as HTMLButtonElement | null;
    const fd = new FormData(e.currentTarget);
    const step = submitter?.value || "deploy";
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

  const canDeploy =
    !pending && !!slot && !!name && !!values.image.trim() && Number(values.container_port) > 0;

  return (
    <form onSubmit={onSubmit}>
      <input type="hidden" name="runtime" value="docker" />
      <input type="hidden" name="subdomain" value={slot?.domain ?? ""} />
      <input type="hidden" name="port" value={slot ? String(slot.port) : ""} />
      {editing && <input type="hidden" name="name" value={name} />}

      {props.mode === "new" && (
        <div className={styles.formRow}>
          <label className={styles.field}>
            App name
            <input
              name="name"
              className={styles.input}
              required
              pattern="[a-z0-9][a-z0-9-]{1,30}[a-z0-9]"
              placeholder="uptime"
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
      )}

      {editing && !slot && (
        <div className={styles.notice} style={{ marginBottom: 12 }}>
          Assign a domain in the Domain section first: the container is published on its port.
        </div>
      )}

      <div className={styles.formRow}>
        <label className={styles.field} style={{ flex: 2 }}>
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
        <label className={styles.field}>
          Container port
          <input
            name="container_port"
            className={styles.input}
            type="number"
            min={1}
            max={65535}
            placeholder="3001"
            value={values.container_port}
            onChange={set("container_port")}
          />
          <span className={styles.muted} style={{ fontSize: 12, marginTop: 4 }}>
            The port the app listens on inside the container.
          </span>
        </label>
      </div>

      <div className={styles.formRow}>
        <label className={styles.field} style={{ flex: 2 }}>
          Folders to keep
          <textarea
            name="volumes"
            className={styles.input}
            rows={3}
            placeholder="/app/data"
            value={values.volumes}
            onChange={set("volumes")}
            spellCheck={false}
            style={{ fontFamily: "var(--mono)", fontSize: 13 }}
          />
          <span className={styles.muted} style={{ fontSize: 12, marginTop: 4 }}>
            One folder inside the container per line. Each is kept in a Docker volume across
            restarts and updates and included in backups. Everything else in the container
            starts fresh on every update.
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
          <span className={styles.code}>KEY=value</span>, one per line. This server is{" "}
          <span className={styles.code}>host.docker.internal</span> from inside the container
          (MariaDB and PostgreSQL answer there once phpMyAdmin or pgAdmin is installed).
        </span>
      </label>

      {state?.ok === false && <div className={styles.error}>{state.error}</div>}

      <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
        <button
          type="submit"
          name="step"
          value="pull"
          className={styles.btnGhost}
          disabled={pending || !values.image.trim() || (!editing && !name) || !slot}
          title="Download the image and read its port and folders"
        >
          {pending && lastStep.current === "pull" ? "Pulling…" : "Pull"}
        </button>
        <button
          type="submit"
          name="step"
          value="deploy"
          className={styles.btn}
          disabled={!canDeploy}
          title={canDeploy ? "Write the unit and start the container" : "Fill in the name, image and container port"}
        >
          {pending && lastStep.current === "deploy" ? "Deploying…" : editing ? "Save and redeploy" : "Deploy"}
        </button>
      </div>

      {state?.ok && state.data?.taskId && (
        <TaskProgress
          key={state.data.taskId}
          taskId={state.data.taskId}
          onComplete={async (s) => {
            if (s !== "succeeded") return;
            if (lastStep.current === "pull") {
              const r = await props.imageInfo(values.image.trim());
              if (r.ok && r.data) applyInfo(r.data);
              return;
            }
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
