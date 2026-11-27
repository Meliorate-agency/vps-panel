"use client";

import { useEffect, useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import Table from "@/components/Table/Table";
import TaskProgress from "@/components/TaskProgress/TaskProgress";
import ConfirmDeleteButton from "@/components/ConfirmDeleteButton/ConfirmDeleteButton";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";
import styles from "@/components/page.module.css";
import type { ImportPreflight, ServerSnapshot, ServerSnapshotEstimate } from "@/lib/agent";
import type { LinkView } from "@/lib/serverSnapshots/links";
import { generatePassphrase, passphraseProblem } from "@/lib/serverSnapshots/passphrase";
import {
  createLinkAction,
  createSnapshotAction,
  deleteSnapshotAction,
  discardImportAction,
  preflightImportAction,
  startImportAction,
  estimateSnapshotAction,
  pullSnapshotAction,
  revokeLinkAction,
} from "@/server-actions/serverSnapshots";

function bytes(n: number): string {
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return i === 0 ? `${n} B` : `${v.toFixed(1)} ${units[i]}`;
}

function when(iso: string): string {
  if (!iso) return "";
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleString();
}

/**
 * The Server snapshots page: makes one encrypted file of the whole server,
 * lists the files, links, downloads and deletes them, and on a freshly
 * installed panel imports one.
 */
export default function ServerSnapshots({
  snapshots,
  links,
  fresh,
}: {
  snapshots: ServerSnapshot[];
  links: LinkView[];
  /** This panel hosts nothing yet: Import is offered (only then can it run). */
  fresh: boolean;
}) {
  const [panel, setPanel] = useState<null | "create" | "pull" | { link: string } | { importing: string }>(null);
  return (
    <div>
      <div style={{ display: "flex", gap: 12, justifyContent: "flex-end", marginBottom: 12, flexWrap: "wrap" }}>
        {panel === null && (
          <div style={{ display: "flex", gap: 8 }}>
            <button className={styles.btn} onClick={() => setPanel("create")}>
              Make a snapshot
            </button>
            <button className={styles.btnGhost} onClick={() => setPanel("pull")}>
              Pull from another server
            </button>
          </div>
        )}
      </div>
      {panel === "create" && <CreateSnapshot onClose={() => setPanel(null)} />}
      {panel === "pull" && <PullSnapshot onClose={() => setPanel(null)} />}
      {panel !== null && typeof panel === "object" && "importing" in panel && (
        <ImportPanel
          snapshot={snapshots.find((s) => s.name === panel.importing)}
          onClose={() => setPanel(null)}
        />
      )}
      {panel !== null && typeof panel === "object" && "link" in panel && (
        <LinkPanel
          snapshot={snapshots.find((s) => s.name === panel.link)}
          links={links.filter((l) => l.snapshot === panel.link)}
          onClose={() => setPanel(null)}
        />
      )}
      <Table
        columns={[
          { key: "name", label: "Snapshot" },
          { key: "made", label: "Made" },
          { key: "size", label: "Size", width: "100px" },
          { key: "drive", label: "Drive", width: "70px" },
          { key: "actions", label: "" },
        ]}
        rowKey={(_, i) => snapshots[i]?.name ?? i}
        rows={snapshots.map((s) => ({
          name: (
            <div>
              <span className={styles.code}>{s.name}</span>
              <div className={styles.muted} style={{ fontSize: 11 }}>
                {s.location === "incoming" ? "pulled from " : "from "}
                {s.source_host || "this server"}
                {s.panel_commit ? ` · panel ${s.panel_commit.slice(0, 7)}${s.panel_dirty ? " (local changes)" : ""}` : ""}
              </div>
              {s.import_unfinished && (
                <div className={styles.muted} style={{ fontSize: 11 }}>
                  An import of it stopped part way: continue it or discard what it did.
                </div>
              )}
              {s.location === "incoming" && !fresh && !s.import_unfinished && (
                <div className={styles.muted} style={{ fontSize: 11 }}>
                  Import it on a freshly installed panel: this one already hosts apps or sites.
                </div>
              )}
            </div>
          ),
          made: <span className={styles.muted}>{when(s.created_at)}</span>,
          size: bytes(s.size),
          drive: s.include_drive ? "yes" : "no",
          actions: (
            <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
              {s.location === "local" && (
                <button
                  className={styles.btnGhost}
                  onClick={() => setPanel({ link: s.name })}
                  title="A one-time link another server pulls this snapshot with"
                >
                  Link
                  {links.some((l) => l.snapshot === s.name && (l.state === "open" || l.state === "in use"))
                    ? " (active)"
                    : ""}
                </button>
              )}
              {/* An unfinished import is offered even though the panel no
                  longer looks fresh: its first steps put apps and sites in. */}
              {(fresh || s.import_unfinished) && (
                <button
                  className={styles.btnGhost}
                  onClick={() => setPanel({ importing: s.name })}
                  title={
                    s.import_unfinished
                      ? "Continue the import that stopped, or discard what it did"
                      : "Make THIS server what the snapshot describes"
                  }
                >
                  {s.import_unfinished ? "Continue import" : "Import"}
                </button>
              )}
              <a className={styles.btnGhost} href={`/api/server-snapshots/${encodeURIComponent(s.name)}/download`}>
                Download
              </a>
              <ConfirmDeleteButton
                action={deleteSnapshotAction.bind(null, s.name)}
                title="Delete this snapshot?"
                body={
                  <>
                    <span className={styles.code}>{s.name}</span> ({bytes(s.size)}) is deleted from this
                    server. Copies you downloaded are not affected.
                  </>
                }
                buttonLabel="Delete"
              />
            </div>
          ),
        }))}
        empty="No server snapshots yet."
      />
    </div>
  );
}

function CreateSnapshot({ onClose }: { onClose: () => void }) {
  const router = useRouter();
  const [pass, setPass] = useState("");
  const [confirm, setConfirm] = useState("");
  const [show, setShow] = useState(false);
  const [drive, setDrive] = useState(true);
  const [pause, setPause] = useState(true);
  const [saved, setSaved] = useState(false);
  const [estimate, setEstimate] = useState<ServerSnapshotEstimate | null>(null);
  const [estimateError, setEstimateError] = useState<string | null>(null);
  const [estimating, startEstimate] = useTransition();
  const [starting, startCreate] = useTransition();
  const [error, setError] = useState<string | null>(null);
  const [taskId, setTaskId] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    startEstimate(async () => {
      setEstimateError(null);
      const r = await estimateSnapshotAction(drive);
      if (r.ok) setEstimate(r.estimate);
      else setEstimateError(r.error);
    });
  }, [drive]);

  const problem = passphraseProblem(pass);
  const mismatch = confirm.length > 0 && confirm !== pass;
  const fits = estimate ? estimate.fits : true;
  const ready = !problem && confirm === pass && saved && fits && !estimating && !starting && !taskId;

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(pass);
      setCopied(true);
    } catch {
      // Plain http (a fresh install before its certificate) has no
      // clipboard API: show it so it can be copied by hand.
      setShow(true);
    }
  };

  if (taskId) {
    return (
      <div className={styles.section} style={{ marginBottom: 16 }}>
        <h3 className={styles.sectionTitle}>Making the snapshot</h3>
        <div style={{ fontSize: 13, marginBottom: 8 }} className={styles.muted}>
          This can take a while on a big server. You can leave this page; the task keeps running and
          is listed under Tasks.
        </div>
        <TaskProgress
          key={taskId}
          taskId={taskId}
          onComplete={() => {
            router.refresh();
          }}
        />
        <div style={{ marginTop: 10 }}>
          <button className={styles.btnGhost} onClick={onClose}>
            Close
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className={styles.section} style={{ marginBottom: 16 }}>
      <h3 className={styles.sectionTitle}>Make a server snapshot</h3>
      <div className={styles.formRow}>
        <label className={styles.field}>
          Passphrase
          <input
            className={styles.input}
            type={show ? "text" : "password"}
            autoComplete="new-password"
            value={pass}
            onChange={(e) => {
              setPass(e.target.value);
              setCopied(false);
            }}
          />
          <span style={{ fontSize: 11, color: problem && pass ? "var(--warn, #e0a800)" : "var(--text-dim)" }}>
            {problem ?? "Strong enough."}
          </span>
        </label>
        <label className={styles.field}>
          Passphrase again
          <input
            className={styles.input}
            type={show ? "text" : "password"}
            autoComplete="new-password"
            value={confirm}
            onChange={(e) => setConfirm(e.target.value)}
          />
          {mismatch && <span style={{ fontSize: 11, color: "var(--danger, #e5484d)" }}>Does not match.</span>}
        </label>
      </div>
      <div style={{ display: "flex", gap: 8, marginBottom: 12, flexWrap: "wrap" }}>
        <button
          type="button"
          className={styles.btnGhost}
          onClick={() => {
            const p = generatePassphrase();
            setPass(p);
            setConfirm(p);
            setShow(true);
            setCopied(false);
          }}
        >
          Generate one
        </button>
        <button type="button" className={styles.btnGhost} onClick={() => setShow((v) => !v)}>
          {show ? "Hide" : "Show"}
        </button>
        {pass && (
          <button type="button" className={styles.btnGhost} onClick={copy}>
            {copied ? "Copied" : "Copy"}
          </button>
        )}
      </div>

      <div style={{ display: "flex", flexDirection: "column", gap: 8, marginBottom: 12 }}>
        <label className={styles.checkbox}>
          <input type="checkbox" checked={drive} onChange={(e) => setDrive(e.target.checked)} />
          Include Drive{estimate ? ` (${bytes(estimate.drive_bytes)})` : ""}
        </label>
        <label className={styles.checkbox}>
          <input type="checkbox" checked={pause} onChange={(e) => setPause(e.target.checked)} />
          Pause Docker containers for the seconds their volumes are copied (a database inside one is
          then copied whole)
        </label>
      </div>

      <Estimate estimate={estimate} loading={estimating} error={estimateError} />

      <label className={styles.checkbox} style={{ margin: "12px 0" }}>
        <input type="checkbox" checked={saved} onChange={(e) => setSaved(e.target.checked)} />
        I saved this passphrase somewhere off this server (a password manager). It is not stored
        anywhere, and the snapshot cannot be opened without it.
      </label>

      {error && <div className={styles.error}>{error}</div>}
      <div style={{ display: "flex", gap: 8 }}>
        <button
          className={styles.btn}
          disabled={!ready}
          onClick={() =>
            startCreate(async () => {
              setError(null);
              const r = await createSnapshotAction(pass, drive, pause);
              if (!r.ok) {
                setError(r.error);
                return;
              }
              setPass("");
              setConfirm("");
              setTaskId(r.taskId);
            })
          }
        >
          {starting ? "Starting..." : "Make the snapshot"}
        </button>
        <button className={styles.btnGhost} onClick={onClose} disabled={starting}>
          Cancel
        </button>
      </div>
    </div>
  );
}

function Estimate({
  estimate,
  loading,
  error,
}: {
  estimate: ServerSnapshotEstimate | null;
  loading: boolean;
  error: string | null;
}) {
  if (error) return <div className={styles.error}>Could not measure the server: {error}</div>;
  if (!estimate) return <div className={styles.muted}>Measuring what the snapshot would hold...</div>;
  return (
    <div style={{ fontSize: 13, opacity: loading ? 0.6 : 1 }}>
      <Table
        columns={[
          { key: "what", label: "Holds" },
          { key: "size", label: "Size", width: "110px" },
        ]}
        rows={[
          ...estimate.sections.map((s) => ({ what: s.title, size: bytes(s.bytes) })),
          { what: "Databases", size: bytes(estimate.database_bytes) },
        ]}
      />
      <div style={{ marginTop: 8 }}>
        About <strong>{bytes(estimate.total_bytes)}</strong> before compression. Needs up to{" "}
        {bytes(estimate.need_bytes)} free on this disk while it is made;{" "}
        {estimate.free_bytes === null ? "free space unknown" : `${bytes(estimate.free_bytes)} free`}.
      </div>
      {!estimate.fits && (
        <div className={styles.error}>
          Not enough free disk space for the snapshot
          {estimate.include_drive ? ". Leaving Drive out makes it smaller." : "."}
        </div>
      )}
    </div>
  );
}

const TTL_CHOICES: { hours: number; label: string }[] = [
  { hours: 1, label: "1 hour" },
  { hours: 24, label: "24 hours" },
  { hours: 72, label: "3 days" },
  { hours: 168, label: "7 days" },
];

/** One-time links another server pulls this snapshot with. */
function LinkPanel({
  snapshot,
  links,
  onClose,
}: {
  snapshot: ServerSnapshot | undefined;
  links: LinkView[];
  onClose: () => void;
}) {
  const [ttl, setTtl] = useState(24);
  const [made, setMade] = useState<{ url: string; expiresAt: number; warning: string | null } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pending, start] = useTransition();
  const [copied, setCopied] = useState(false);
  if (!snapshot) return null;

  const copy = async () => {
    if (!made) return;
    try {
      await navigator.clipboard.writeText(made.url);
      setCopied(true);
    } catch {
      // Plain http has no clipboard API: the field is selected instead.
      (document.getElementById("snapshot-link") as HTMLInputElement | null)?.select();
    }
  };

  return (
    <div className={styles.section} style={{ marginBottom: 16 }}>
      <h3 className={styles.sectionTitle}>
        Pull link for <span className={styles.code}>{snapshot.name}</span>
      </h3>
      <div style={{ fontSize: 13, marginBottom: 12 }} className={styles.muted}>
        On the new server (a freshly installed panel), open Backups, choose Pull from another server and
        paste the link: the file goes straight from this server to that one. The first server that uses
        the link gets the file and can resume a broken transfer; nobody else can use it after that.
        Anyone holding the link can fetch the encrypted file, so pass it on like a password.
      </div>

      {made ? (
        <div style={{ marginBottom: 12 }}>
          <div style={{ display: "flex", gap: 8 }}>
            <input
              id="snapshot-link"
              className={styles.input}
              readOnly
              value={made.url}
              onFocus={(e) => e.currentTarget.select()}
              style={{ flex: 1, fontFamily: "var(--font-mono, monospace)", fontSize: 12 }}
            />
            <button className={styles.btnGhost} onClick={copy}>
              {copied ? "Copied" : "Copy"}
            </button>
          </div>
          <div style={{ fontSize: 12, marginTop: 6 }} className={styles.muted}>
            Shown once: only a fingerprint of its secret is kept. Works until{" "}
            {new Date(made.expiresAt).toLocaleString()}.
          </div>
          {made.warning && <div className={styles.error}>{made.warning}</div>}
        </div>
      ) : (
        <div style={{ display: "flex", gap: 8, alignItems: "center", marginBottom: 12, flexWrap: "wrap" }}>
          <label className={styles.field} style={{ flexDirection: "row", alignItems: "center", gap: 8 }}>
            Works for
            <select className={styles.select} value={ttl} onChange={(e) => setTtl(Number(e.target.value))}>
              {TTL_CHOICES.map((c) => (
                <option key={c.hours} value={c.hours}>
                  {c.label}
                </option>
              ))}
            </select>
          </label>
          <button
            className={styles.btn}
            disabled={pending}
            onClick={() =>
              start(async () => {
                setError(null);
                const r = await createLinkAction(snapshot.name, ttl);
                if (!r.ok) setError(r.error);
                else setMade({ url: r.url, expiresAt: r.expiresAt, warning: r.warning });
              })
            }
          >
            {pending ? "Making..." : "Make a link"}
          </button>
        </div>
      )}
      {error && <div className={styles.error}>{error}</div>}

      {links.length > 0 && (
        <Table
          columns={[
            { key: "state", label: "Link" },
            { key: "until", label: "Works until" },
            { key: "who", label: "Taken by" },
            { key: "served", label: "Sent", width: "100px" },
            { key: "act", label: "", width: "90px" },
          ]}
          rowKey={(_, i) => links[i]?.id ?? i}
          rows={links.map((l) => ({
            state: l.state,
            until: <span className={styles.muted}>{new Date(l.expires_at).toLocaleString()}</span>,
            who: l.claimed_ip ? <span className={styles.code}>{l.claimed_ip}</span> : <span className={styles.muted}>nobody yet</span>,
            served: bytes(l.bytes_served),
            act:
              l.state === "open" || l.state === "in use" ? (
                <ConfirmDeleteButton
                  action={revokeLinkAction.bind(null, l.id)}
                  title="Revoke this link?"
                  body="The link stops working at once, also for a server that is pulling with it right now."
                  confirmLabel="Revoke"
                  buttonLabel="Revoke"
                />
              ) : null,
          }))}
        />
      )}
      <div style={{ marginTop: 10 }}>
        <button className={styles.btnGhost} onClick={onClose}>
          Close
        </button>
      </div>
    </div>
  );
}

/** On the new server: fetch a snapshot from the old one's link. */
function PullSnapshot({ onClose }: { onClose: () => void }) {
  const router = useRouter();
  const [link, setLink] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [pending, start] = useTransition();
  const [taskId, setTaskId] = useState<string | null>(null);

  return (
    <div className={styles.section} style={{ marginBottom: 16 }}>
      <h3 className={styles.sectionTitle}>Pull a snapshot from another server</h3>
      <div style={{ fontSize: 13, marginBottom: 12 }} className={styles.muted}>
        Paste the link made on the old server (Backups, its snapshot, Link). This server fetches the
        file straight from there; a broken transfer picks up where it stopped. It then shows up in the
        list below, ready to import.
      </div>
      {taskId ? (
        <TaskProgress key={taskId} taskId={taskId} onComplete={() => router.refresh()} />
      ) : (
        <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
          <input
            className={styles.input}
            style={{ flex: "1 1 420px" }}
            placeholder="https://panel.example.com/api/snapshot-pull/...#..."
            value={link}
            onChange={(e) => setLink(e.target.value)}
            autoComplete="off"
            spellCheck={false}
          />
          <button
            className={styles.btn}
            disabled={pending || !link.trim()}
            onClick={() =>
              start(async () => {
                setError(null);
                const r = await pullSnapshotAction(link);
                if (!r.ok) {
                  setError(r.error);
                  return;
                }
                setLink("");
                setTaskId(r.taskId);
              })
            }
          >
            {pending ? "Starting..." : "Pull"}
          </button>
        </div>
      )}
      {error && <div className={styles.error}>{error}</div>}
      <div style={{ marginTop: 10 }}>
        <button className={styles.btnGhost} onClick={onClose}>
          Close
        </button>
      </div>
    </div>
  );
}

const STEP_NAMES: Record<string, string> = {
  unpack: "unpacked",
  packages: "packages installed",
  accounts: "accounts made",
  files: "files in place",
  "panel-a": "panel records",
  mariadb: "MariaDB",
  "dns-mail": "DNS and mail",
  postgres: "PostgreSQL",
  docker: "Docker",
  services: "services started",
  settings: "settings",
  "panel-b": "logins",
};

/**
 * Import a snapshot: THIS server takes over everything the snapshot's
 * server hosted, its panel logins included. Only on a freshly installed
 * panel (the agent's checks refuse anything else).
 */
function ImportPanel({ snapshot, onClose }: { snapshot: ServerSnapshot | undefined; onClose: () => void }) {
  const router = useRouter();
  const [pass, setPass] = useState("");
  const [pre, setPre] = useState<ImportPreflight | null>(null);
  const [replaces, setReplaces] = useState(false);
  const [sure, setSure] = useState(false);
  const [discarding, setDiscarding] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [pending, start] = useTransition();
  const [taskId, setTaskId] = useState<string | null>(null);
  const [finished, setFinished] = useState<null | "succeeded" | "failed">(null);
  if (!snapshot) return null;

  if (taskId) {
    return (
      <div className={styles.section} style={{ marginBottom: 16 }}>
        <h3 className={styles.sectionTitle}>Importing {snapshot.name}</h3>
        <TaskProgress
          key={taskId}
          taskId={taskId}
          onComplete={(s) => {
            setFinished(s);
            if (s === "succeeded") {
              // The agent restarts the panel a few seconds after; the old
              // server's logins are the ones that work from now on.
              window.setTimeout(() => window.location.assign("/login"), 15000);
            } else {
              router.refresh();
            }
          }}
        />
        {finished === "succeeded" && (
          <div className={styles.notice} style={{ marginTop: 10 }}>
            Done. The panel restarts now; sign in with the old server&apos;s login. The steps left to
            do by hand are at the end of the log above.
          </div>
        )}
        {finished === "failed" && (
          <>
            <div className={styles.error} style={{ marginTop: 10 }}>
              The import stopped. Continue it from where it stopped, or discard what it did: both
              start from the passphrase again.
            </div>
            <div style={{ display: "flex", gap: 8, marginTop: 10 }}>
              <button
                className={styles.btn}
                onClick={() => {
                  setTaskId(null);
                  setFinished(null);
                  setPre(null);
                  setSure(false);
                }}
              >
                Continue or discard
              </button>
              <button className={styles.btnGhost} onClick={onClose}>
                Close
              </button>
            </div>
          </>
        )}
      </div>
    );
  }

  const inv = pre?.manifest.inventory;
  const count = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;
  return (
    <div className={styles.section} style={{ marginBottom: 16 }}>
      <h3 className={styles.sectionTitle}>
        Import <span className={styles.code}>{snapshot.name}</span> onto this server
      </h3>
      <div style={{ fontSize: 13, marginBottom: 12 }} className={styles.muted}>
        This server becomes the one in the snapshot: its apps, sites, databases, DNS zones (moved to
        this server&apos;s address), mail, files, accounts and settings, and its panel logins. Only for
        a freshly installed panel.
      </div>
      {!pre ? (
        <div style={{ display: "flex", gap: 8, flexWrap: "wrap", marginBottom: 8 }}>
          <input
            className={styles.input}
            type="password"
            autoComplete="off"
            placeholder="The snapshot's passphrase"
            value={pass}
            onChange={(e) => setPass(e.target.value)}
            style={{ flex: "1 1 320px" }}
          />
          <button
            className={styles.btn}
            disabled={pending || !pass}
            onClick={() =>
              start(async () => {
                setError(null);
                const r = await preflightImportAction(snapshot.name, pass);
                if (r.ok) setPre(r.preflight);
                else setError(r.error);
              })
            }
          >
            {pending ? "Opening..." : "Open the snapshot"}
          </button>
        </div>
      ) : (
        <>
          <div style={{ fontSize: 13, marginBottom: 10 }}>
            Made {new Date(pre.manifest.created_at).toLocaleString()} on{" "}
            <span className={styles.code}>{pre.manifest.source.panel_host}</span>
            {pre.manifest.source.ipv4 ? ` (${pre.manifest.source.ipv4})` : ""}.
          </div>
          {inv && (
            <div style={{ fontSize: 13, marginBottom: 10 }} className={styles.muted}>
              {[
                count(inv.apps.length, "app", "apps"),
                count(inv.sites.length, "site", "sites"),
                count(inv.dns_zones.length, "DNS zone", "DNS zones"),
                count(inv.mail_domains.length, "mail domain", "mail domains"),
                count(inv.mailboxes, "mailbox", "mailboxes"),
                count(inv.mariadb_databases.length + inv.postgres_databases.length, "database", "databases"),
                count(inv.containers.length, "container", "containers"),
                count(inv.cron_jobs, "cron job", "cron jobs"),
                count(inv.system_users.length, "account", "accounts"),
                count(inv.panel_users, "panel login", "panel logins"),
              ].join(", ")}
              {pre.manifest.options.include_drive ? ", Drive" : ""}.
            </div>
          )}
          <ul style={{ listStyle: "none", padding: 0, margin: "0 0 12px 0", fontSize: 13 }}>
            {pre.checks.map((c) => (
              <li key={c.id} style={{ marginBottom: 4 }}>
                <span
                  style={{
                    color: c.level === "ok" ? "var(--ok)" : c.level === "warn" ? "var(--warn)" : "var(--danger)",
                    fontWeight: 600,
                    marginRight: 6,
                  }}
                >
                  {c.level === "ok" ? "OK" : c.level === "warn" ? "Note" : "Stops it"}
                </span>
                {c.message}
              </li>
            ))}
          </ul>
          {pre.resumable && (
            <div style={{ fontSize: 13, marginBottom: 10 }}>
              An earlier import of this snapshot stopped after:{" "}
              {pre.done_steps.map((s) => STEP_NAMES[s] ?? s).join(", ") || "nothing"}. Importing
              continues from there.{" "}
              <button className={styles.btnGhost} disabled={pending} onClick={() => setDiscarding(true)}>
                Discard what it did
              </button>
              <ConfirmDialog
                open={discarding}
                title="Discard what the import did?"
                confirmLabel="Discard"
                tone="danger"
                pending={pending}
                onCancel={() => setDiscarding(false)}
                onConfirm={() =>
                  start(async () => {
                    setError(null);
                    const r = await discardImportAction(snapshot.name);
                    setDiscarding(false);
                    if (!r.ok) setError(r.error);
                    else {
                      setPre(null);
                      router.refresh();
                    }
                  })
                }
              >
                <p style={{ margin: "0 0 8px" }}>
                  Everything the import added goes: its apps, sites, databases and their accounts, DNS
                  zones, mail domains with their mailboxes, accounts and files. What this server had before
                  is put back, its own DNS zone and panel records included, so the panel is freshly installed
                  again and a snapshot can be imported anew.
                </p>
                <p className={styles.muted} style={{ margin: 0 }}>
                  Packages the import installed (Docker, PostgreSQL and the like) stay, and so do firewall,
                  SSH and swap settings if the import got as far as applying them.
                </p>
              </ConfirmDialog>
            </div>
          )}
          {pre.can_import && (
            <>
              <label className={styles.checkbox} style={{ marginBottom: 6 }}>
                <input type="checkbox" checked={replaces} onChange={(e) => setReplaces(e.target.checked)} />
                This server replaces the old one for good (it also takes the old server&apos;s place in My
                Networks; shut the old one down)
              </label>
              <label className={styles.checkbox} style={{ marginBottom: 12, display: "flex" }}>
                <input type="checkbox" checked={sure} onChange={(e) => setSure(e.target.checked)} />
                I understand: this server&apos;s panel logins are replaced by the old server&apos;s, and
                everyone signs in again.
              </label>
              <div>
                <button
                  className={styles.btn}
                  disabled={pending || !sure}
                  onClick={() =>
                    start(async () => {
                      setError(null);
                      const r = await startImportAction(snapshot.name, pass, replaces);
                      if (!r.ok) {
                        setError(r.error);
                        return;
                      }
                      setPass("");
                      setTaskId(r.taskId);
                    })
                  }
                >
                  {pending ? "Starting..." : pre.resumable ? "Continue the import" : "Import"}
                </button>
              </div>
            </>
          )}
        </>
      )}
      {error && <div className={styles.error}>{error}</div>}
      <div style={{ marginTop: 10 }}>
        <button className={styles.btnGhost} onClick={onClose} disabled={pending}>
          Close
        </button>
      </div>
    </div>
  );
}
