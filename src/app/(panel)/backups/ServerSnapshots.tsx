"use client";

import { useEffect, useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import Table from "@/components/Table/Table";
import TaskProgress from "@/components/TaskProgress/TaskProgress";
import ConfirmDeleteButton from "@/components/ConfirmDeleteButton/ConfirmDeleteButton";
import styles from "@/components/page.module.css";
import type { ServerSnapshot, ServerSnapshotEstimate } from "@/lib/agent";
import type { LinkView } from "@/lib/serverSnapshots/links";
import { generatePassphrase, passphraseProblem } from "@/lib/serverSnapshots/passphrase";
import {
  createLinkAction,
  createSnapshotAction,
  deleteSnapshotAction,
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
 * Backups > Server snapshot: makes one encrypted file of the whole server,
 * lists the files, downloads and deletes them. A freshly installed panel on
 * another server imports such a file.
 */
export default function ServerSnapshots({
  snapshots,
  links,
}: {
  snapshots: ServerSnapshot[];
  links: LinkView[];
}) {
  const [panel, setPanel] = useState<null | "create" | "pull" | { link: string }>(null);
  return (
    <div>
      <div style={{ display: "flex", gap: 12, alignItems: "flex-start", marginBottom: 12, flexWrap: "wrap" }}>
        <div style={{ flex: "1 1 420px", fontSize: 13, color: "var(--text-dim)" }}>
          One encrypted file with everything this server hosts: apps, sites, certificates, every
          database, DNS zones, mailboxes, cron jobs, SSH keys, Docker volumes, accounts, settings and,
          if you choose, Drive. A freshly installed panel on another server imports it and takes over.
          It is encrypted with a passphrase you choose here; without it nobody can open the file,
          you included.
        </div>
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
      {panel !== null && typeof panel === "object" && (
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
          { key: "actions", label: "", width: "280px" },
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
