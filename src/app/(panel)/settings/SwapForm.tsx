"use client";

import { startTransition, useActionState, useState } from "react";
import { createPortal } from "react-dom";
import { useRouter } from "next/navigation";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";
import TaskProgress from "@/components/TaskProgress/TaskProgress";
import styles from "@/components/page.module.css";
import local from "./server.module.css";
import type { SwapStatus } from "@/lib/agent";
import type { SwapResult } from "@/server-actions/swap";

const MIB = 1024 * 1024;
const GIB = 1024 * MIB;
/** Sizes offered, in MiB. The ones that do not fit on the disk are left out. */
const SIZES_MB = [512, 1024, 2048, 3072, 4096, 6144, 8192, 12288, 16384, 24576, 32768, 49152, 65536];
const SWAPPINESS = [
  { value: 1, label: "Only as a last resort (1)" },
  { value: 10, label: "When RAM is nearly full (10), suggested" },
  { value: 60, label: "Sooner, the Linux default (60)" },
];

type Action = (prev: SwapResult | null, fd: FormData) => Promise<SwapResult>;

/** Same rounding as the agent's log: the kernel counts a swap area one page short. */
function bytes(b: number): string {
  return b >= GIB - MIB ? `${Number((b / GIB).toFixed(1))} GB` : `${Math.round(b / MIB)} MB`;
}

function mbText(mb: number): string {
  return mb >= 1024 ? `${Number((mb / 1024).toFixed(1))} GB` : `${mb} MB`;
}

/**
 * Settings > Server > Swap: what is in place, then a size and a swappiness
 * picker. Saving starts an agent task whose log streams under the form; when
 * it ends the page re-renders with the new live values.
 */
export default function SwapForm({ status, action }: { status: SwapStatus; action: Action }) {
  const router = useRouter();
  const [state, formAction, pending] = useActionState<SwapResult | null, FormData>(action, null);
  const [finished, setFinished] = useState<string | null>(null);
  const taskId = state?.ok ? state.taskId : null;
  const running = taskId !== null && finished !== taskId;

  const save = (sizeMb: number, swappiness: number) => {
    const fd = new FormData();
    fd.set("size_mb", String(sizeMb));
    fd.set("swappiness", String(swappiness));
    // Inside a transition, or pending never turns true and a second click
    // starts a second change.
    startTransition(() => formAction(fd));
  };

  return (
    <>
      <Facts status={status} />
      {status.unsupported ? (
        <div className={local.warn}>{status.unsupported}</div>
      ) : (
        <Fields
          // A finished change re-renders the page with the new live values;
          // a new key resets the pickers to them.
          key={[
            status.file_bytes,
            status.active,
            status.in_fstab,
            status.swappiness,
            status.leftover_bridge,
          ].join("|")}
          status={status}
          busy={pending || running || status.busy}
          onSave={save}
        />
      )}
      {status.busy && !running && (
        <p className={`${styles.muted} ${local.hint}`} style={{ marginTop: 8 }}>
          A swap change is running; its log is under <a href="/tasks">Tasks</a>.
        </p>
      )}
      {state?.ok === false && (
        <div className={styles.error} style={{ marginTop: 10 }}>
          {state.error}
        </div>
      )}
      {taskId && (
        <div style={{ marginTop: 12 }}>
          <TaskProgress
            key={taskId}
            taskId={taskId}
            onComplete={() => {
              setFinished(taskId);
              router.refresh();
            }}
          />
        </div>
      )}
    </>
  );
}

function Facts({ status }: { status: SwapStatus }) {
  return (
    <>
      <ul className={local.facts}>
        <li>
          <span className={styles.muted}>Swap file:</span>{" "}
          {status.file_bytes === null ? (
            "none"
          ) : (
            <>
              <span className={styles.code}>{status.path}</span> {bytes(status.file_bytes)},{" "}
              {status.active ? `${bytes(status.used_bytes)} in use` : "not in use"}
            </>
          )}
        </li>
        {status.other.map((a) => (
          <li key={a.name}>
            <span className={styles.muted}>Other swap:</span>{" "}
            <span className={styles.code}>{a.name}</span> ({a.kind}) {bytes(a.size_bytes)},{" "}
            {bytes(a.used_bytes)} in use
          </li>
        ))}
        <li>
          <span className={styles.muted}>RAM:</span> {bytes(status.mem_total_bytes)},{" "}
          {bytes(status.mem_available_bytes)} available
        </li>
        <li>
          <span className={styles.muted}>Disk:</span> {bytes(status.disk_free_bytes)} free of{" "}
          {bytes(status.disk_total_bytes)}
        </li>
      </ul>
      {status.active && !status.in_fstab && (
        <div className={local.warn}>
          <span className={styles.code}>{status.path}</span> is not in{" "}
          <span className={styles.code}>/etc/fstab</span>, so it would be gone after a reboot.
          Saving adds it.
        </div>
      )}
      {status.file_bytes !== null && !status.active && (
        <div className={local.warn}>
          Linux is not using <span className={styles.code}>{status.path}</span>. Saving turns it
          on at the size picked below; Off deletes it.
        </div>
      )}
    </>
  );
}

function Fields({
  status,
  busy,
  onSave,
}: {
  status: SwapStatus;
  busy: boolean;
  onSave: (sizeMb: number, swappiness: number) => void;
}) {
  const fileMb = status.file_bytes === null ? 0 : Math.round(status.file_bytes / MIB);
  const liveMb = status.active ? fileMb : 0;
  const [sizeMb, setSizeMb] = useState(fileMb);
  const [swappiness, setSwappiness] = useState(status.swappiness);
  const [confirming, setConfirming] = useState(false);

  // Rule of thumb: as much swap as RAM, at least 2 GB and at most 8 GB.
  const ramMb = status.mem_total_bytes / MIB;
  const suggestedMb = SIZES_MB.find((mb) => mb >= Math.min(8192, Math.max(2048, ramMb))) ?? 8192;
  const fits = (mb: number) => mb >= status.min_mb && mb <= status.max_mb;
  const sizes = Array.from(
    new Set([0, ...SIZES_MB.filter(fits), ...(fileMb ? [fileMb] : [])]),
  ).sort((a, b) => a - b);
  const swappinessOptions = SWAPPINESS.some((o) => o.value === status.swappiness)
    ? SWAPPINESS
    : [...SWAPPINESS, { value: status.swappiness, label: `Current setting (${status.swappiness})` }];

  const sizeLabel = (mb: number) => {
    if (mb === 0) return "Off, no swap file";
    const tags: string[] = [];
    if (mb === fileMb && status.file_bytes !== null) {
      tags.push(status.active ? "current" : "existing file");
    }
    if (mb === suggestedMb) tags.push("suggested");
    return tags.length ? `${mbText(mb)} (${tags.join(", ")})` : mbText(mb);
  };

  const changes = describeChanges(status, liveMb, sizeMb, swappiness);
  const turningOff = sizeMb === 0 && status.file_bytes !== null;

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        if (turningOff) setConfirming(true);
        else onSave(sizeMb, swappiness);
      }}
    >
      <div className={styles.formRow}>
        <label className={styles.field}>
          Swap file size
          <select
            className={styles.input}
            value={sizeMb}
            disabled={busy}
            onChange={(e) => setSizeMb(Number(e.target.value))}
          >
            {sizes.map((mb) => (
              <option key={mb} value={mb}>
                {sizeLabel(mb)}
              </option>
            ))}
          </select>
        </label>
        <label className={styles.field}>
          When to use it (swappiness)
          <select
            className={styles.input}
            value={swappiness}
            disabled={busy}
            onChange={(e) => setSwappiness(Number(e.target.value))}
          >
            {swappinessOptions.map((o) => (
              <option key={o.value} value={o.value}>
                {o.label}
              </option>
            ))}
          </select>
        </label>
      </div>
      <p className={`${styles.muted} ${local.hint}`}>
        {sizes.length === 1
          ? `No swap file fits: the disk has ${bytes(status.disk_free_bytes)} free and ${bytes(status.disk_reserve_bytes)} of that has to stay free.`
          : `Suggested for ${bytes(status.mem_total_bytes)} of RAM: ${mbText(suggestedMb)}. Sizes that would leave less than ${bytes(status.disk_reserve_bytes)} of the disk free are not listed.`}{" "}
        Swappiness is how early Linux moves idle memory to the disk: at 10, apps stay in RAM
        until it is nearly full.
      </p>
      {changes.length > 0 && (
        <ul className={local.changes}>
          {changes.map((c) => (
            <li key={c}>{c}</li>
          ))}
        </ul>
      )}
      <button type="submit" className={styles.btn} disabled={busy || changes.length === 0}>
        {busy ? "Working..." : "Save swap settings"}
      </button>
      {confirming &&
        typeof document !== "undefined" &&
        // Portaled: the settings card's backdrop-filter would trap the
        // dialog's fixed positioning inside the card.
        createPortal(
          <ConfirmDialog
            open
            title="Turn swap off?"
            tone="danger"
            confirmLabel="Turn swap off"
            onConfirm={() => {
              setConfirming(false);
              onSave(sizeMb, swappiness);
            }}
            onCancel={() => setConfirming(false)}
          >
            <p style={{ marginTop: 0 }}>{changes.join(" ")}</p>
            <p>Without swap, Linux kills a process when the apps need more memory than is free.</p>
          </ConfirmDialog>,
          document.body,
        )}
    </form>
  );
}

/** What saving would do, one sentence per change; empty = nothing to save. */
function describeChanges(
  status: SwapStatus,
  liveMb: number,
  sizeMb: number,
  swappiness: number,
): string[] {
  const path = status.path;
  const inUse = status.active && status.used_bytes > 0 ? bytes(status.used_bytes) : null;
  const out: string[] = [];
  if (sizeMb !== liveMb) {
    if (sizeMb === 0) {
      out.push(
        `Turn swap off and delete ${path}${inUse ? `; the ${inUse} in it moves back into RAM` : ""}.`,
      );
    } else if (liveMb > 0) {
      out.push(
        `Resize ${path} from ${mbText(liveMb)} to ${mbText(sizeMb)}${inUse ? `; the ${inUse} in it moves out first` : ""}.`,
      );
    } else if (status.file_bytes !== null) {
      out.push(`Rebuild ${path} as a ${mbText(sizeMb)} swap file, on now and after every reboot.`);
    } else {
      out.push(`Create a ${mbText(sizeMb)} swap file at ${path}, on now and after every reboot.`);
    }
  } else if (sizeMb === 0 && status.file_bytes !== null) {
    out.push(`Delete the unused ${path}.`);
  } else if (sizeMb > 0 && !status.in_fstab) {
    out.push(`Add ${path} to /etc/fstab so it stays on after a reboot.`);
  } else if (sizeMb === 0 && status.in_fstab) {
    out.push(`Remove the ${path} line from /etc/fstab: the file does not exist.`);
  }
  if (status.leftover_bridge) {
    out.push(`Remove ${status.leftover_bridge}, left by a change that did not finish.`);
  }
  if (swappiness !== status.swappiness) {
    out.push(`Change swappiness from ${status.swappiness} to ${swappiness}.`);
  }
  return out;
}
