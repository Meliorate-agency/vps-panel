"use client";

import Link from "next/link";
import { useActionState, useEffect, useRef, useState } from "react";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";
import styles from "@/components/page.module.css";
import type { ActionResult, RoutingOutcome } from "@/server-actions/apps";

type Result = ActionResult<RoutingOutcome>;

type Slot = { domain: string; port: number };

type Props = {
  /** Slot the app is bound to now; null when unassigned. */
  current: Slot | null;
  /** Slots this app may pick: unowned ones plus its own. */
  slots: Slot[];
  /** Slots kept out of the picker, with the reason, for the hint line. */
  unavailable: { domain: string; reason: string }[];
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
};

const NONE = "";

function slotKey(s: Slot | null): string {
  return s ? `${s.domain}|${s.port}` : NONE;
}

/**
 * Domain picker on /apps/[name]. Picking another slot moves the app to that
 * slot's port and restarts it; "No domain" stops + disables the app, so that
 * one goes through a confirm first. Save stays greyed out until the value
 * differs from what the server has - same UX as the entrypoint editor.
 */
export default function DomainForm({ current, slots, unavailable, action }: Props) {
  const [state, formAction, pending] = useActionState<Result | null, FormData>(
    action,
    null,
  );
  const currentKey = slotKey(current);
  const [selected, setSelected] = useState(currentKey);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const formRef = useRef<HTMLFormElement>(null);
  const confirmedRef = useRef(false);

  // revalidatePath after a save makes the server's binding the clean baseline.
  useEffect(() => {
    setSelected(currentKey);
  }, [currentKey]);

  // React 19 resets the form after a successful action; the <select> DOM
  // snaps to its default option while React's value (unchanged between
  // renders) is never re-written. Re-apply it once the reset has landed -
  // same workaround as DeployForm.
  const selectRef = useRef<HTMLSelectElement>(null);
  useEffect(() => {
    const form = formRef.current;
    if (!form) return;
    const onReset = () => {
      queueMicrotask(() => {
        if (selectRef.current) selectRef.current.value = selected;
      });
    };
    form.addEventListener("reset", onReset);
    return () => form.removeEventListener("reset", onReset);
  }, [selected]);

  const dirty = selected !== currentKey;
  const canSave = !pending && dirty;
  const unassigning = selected === NONE;
  // The current binding may not be a site (app deployed with its own vhost);
  // keep it selectable so the form can always show where the app is.
  const showCurrentExtra =
    current !== null && !slots.some((s) => s.domain === current.domain);

  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    if (unassigning && current && !confirmedRef.current) {
      e.preventDefault();
      setConfirmOpen(true);
    }
    confirmedRef.current = false;
  }

  const outcome = state?.ok ? state.data : undefined;

  return (
    <form action={formAction} onSubmit={onSubmit} ref={formRef}>
      <label className={styles.field} style={{ marginBottom: 8 }}>
        Routing slot
        <select
          ref={selectRef}
          name="slot"
          className={styles.select}
          value={selected}
          onChange={(e) => setSelected(e.target.value)}
          disabled={pending}
        >
          <option value={NONE}>No domain - unload the app (stop + disable)</option>
          {showCurrentExtra && current && (
            <option value={slotKey(current)}>
              {current.domain} → 127.0.0.1:{current.port} (current)
            </option>
          )}
          {slots.map((s) => (
            <option key={s.domain} value={slotKey(s)}>
              {s.domain} → 127.0.0.1:{s.port}
              {current?.domain === s.domain ? " (current)" : ""}
            </option>
          ))}
        </select>
        <span className={styles.muted} style={{ fontSize: 12, marginTop: 4 }}>
          Slots are port-mode sites from{" "}
          <Link href="/domains" className={styles.code}>
            /domains
          </Link>
          . The app adopts the slot&apos;s port through{" "}
          <span className={styles.code}>PORT</span> and restarts. Only slots not
          bound to another app are listed.
          {slots.length === 0 && !current && (
            <> No free slot right now - add a port-mode site first.</>
          )}
          {unavailable.length > 0 && (
            <>
              {" "}
              Not available:{" "}
              {unavailable.map((u, i) => (
                <span key={u.domain}>
                  {i > 0 ? ", " : ""}
                  <span className={styles.code}>{u.domain}</span> ({u.reason})
                </span>
              ))}
              .
            </>
          )}
        </span>
      </label>

      <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
        {state?.ok === false && (
          <span className={styles.error} style={{ marginRight: "auto" }}>
            {state.error}
          </span>
        )}
        {outcome && !dirty && (
          <span
            style={{
              color: outcome.state === "assigned" && !outcome.bound ? "var(--warn)" : "var(--ok, #4ade80)",
              fontSize: 12,
              fontWeight: 600,
              marginRight: "auto",
            }}
          >
            {outcome.state === "unassigned"
              ? `Domain removed - app stopped and disabled. ${outcome.domain} answers 502 until another app takes it.`
              : outcome.bound
                ? `Bound to ${outcome.domain} - app restarted on 127.0.0.1:${outcome.port}.`
                : `Bound to ${outcome.domain}, but the app did not come up on :${outcome.port} within 45s and was stopped. A start script with its own -p <port> ignores PORT - fix the entrypoint, then press Start.`}
          </span>
        )}
        <button
          type="submit"
          disabled={!canSave}
          className={canSave ? styles.btn : undefined}
          style={
            canSave
              ? { fontSize: 13, padding: "6px 14px" }
              : {
                  fontSize: 13,
                  padding: "6px 14px",
                  background: "var(--bg-elev-2)",
                  border: "1px solid var(--border)",
                  color: "var(--text-dim)",
                  borderRadius: 6,
                  cursor: "not-allowed",
                  fontWeight: 600,
                }
          }
          title={!dirty ? "No changes to save" : unassigning ? "Stop + disable the app" : "Rewrite PORT + restart"}
        >
          {pending ? "Applying…" : unassigning ? "Remove domain" : "Assign & restart"}
        </button>
      </div>

      {outcome && !outcome.bound && outcome.state === "assigned" && outcome.log.length > 0 && !dirty && (
        <details style={{ marginTop: 10 }}>
          <summary className={styles.muted} style={{ cursor: "pointer", fontSize: 12 }}>
            journal (last {outcome.log.length} lines)
          </summary>
          <pre className={styles.log}>{outcome.log.join("\n")}</pre>
        </details>
      )}

      <ConfirmDialog
        open={confirmOpen}
        title="Remove domain?"
        tone="danger"
        confirmLabel="Stop & remove"
        onCancel={() => setConfirmOpen(false)}
        onConfirm={() => {
          setConfirmOpen(false);
          confirmedRef.current = true;
          formRef.current?.requestSubmit();
        }}
      >
        <p style={{ margin: 0 }}>
          The app is <strong>stopped and disabled</strong> (it will not come
          back on reboot) and{" "}
          <span className={styles.code}>{current?.domain}</span> answers 502
          until another app is assigned to it. Code, env files and the cert
          stay in place; pick a slot here to bring the app back.
        </p>
      </ConfirmDialog>
    </form>
  );
}
