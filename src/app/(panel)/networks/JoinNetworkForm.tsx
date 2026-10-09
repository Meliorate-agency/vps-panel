"use client";

import { useState, useTransition } from "react";
import styles from "@/components/page.module.css";
import local from "./networks.module.css";
import type { ActionResult, InvitePreview } from "@/server-actions/networks";

type Preview = { ok: true; invite: InvitePreview } | { ok: false; error: string };

/**
 * Paste a join code, see where it leads (the network, its owner's address
 * and key fingerprint), then join. Nothing leaves this server before Join.
 */
export default function JoinNetworkForm({
  preview,
  join,
  disabled,
  onDone,
}: {
  preview: (code: string) => Promise<Preview>;
  join: (code: string) => Promise<ActionResult>;
  disabled: boolean;
  /** Called after joining (the network shows up below). */
  onDone?: () => void;
}) {
  const [code, setCode] = useState("");
  const [checked, setChecked] = useState<InvitePreview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<string | null>(null);
  const [pending, start] = useTransition();

  const reset = () => {
    setChecked(null);
    setError(null);
  };

  return (
    <div>
      <label className={styles.field}>
        Join code from the network&apos;s owner
        <textarea
          className={`${styles.textarea} ${local.code}`}
          value={code}
          onChange={(e) => {
            setCode(e.target.value);
            setDone(null);
            reset();
          }}
          placeholder="vpsnet1...."
          spellCheck={false}
          autoComplete="off"
          autoFocus
          disabled={disabled || pending}
        />
      </label>
      {error && <div className={styles.error} style={{ marginTop: 10 }}>{error}</div>}
      {done && <div className={styles.notice} style={{ marginTop: 10 }}>{done}</div>}
      {checked ? (
        <div className={local.preview}>
          <dl className={local.facts}>
            <dt>Network</dt>
            <dd>
              <strong>{checked.name}</strong>
            </dd>
            <dt>Owner</dt>
            <dd>
              <span className={styles.code}>{checked.url}</span>
            </dd>
            <dt>Owner key</dt>
            <dd>
              <span className={styles.code}>{checked.ownerFingerprint}</span>
            </dd>
            <dt>Code valid for</dt>
            <dd>{Math.max(1, Math.round((checked.expires - Date.now()) / 60_000))} more min, once</dd>
          </dl>
          <div className={styles.muted} style={{ fontSize: 12, margin: "8px 0 10px" }}>
            Compare the owner key with the one on the owner&apos;s My Networks page. Joining shares this
            server&apos;s name, address and load figures with every server in the network.
          </div>
          <div className={styles.actions}>
            <button
              type="button"
              className={styles.btn}
              disabled={pending}
              onClick={() =>
                start(async () => {
                  const r = await join(code);
                  if (r.ok) {
                    setCode("");
                    setChecked(null);
                    setDone(r.message ?? "Joined.");
                    onDone?.();
                  } else {
                    setError(r.error);
                  }
                })
              }
            >
              {pending ? "Joining..." : "Join network"}
            </button>
            <button type="button" className={styles.btnGhost} disabled={pending} onClick={reset}>
              Cancel
            </button>
          </div>
        </div>
      ) : (
        <button
          type="button"
          className={styles.btnGhost}
          style={{ marginTop: 10 }}
          disabled={disabled || pending || !code.trim()}
          onClick={() =>
            start(async () => {
              const r = await preview(code);
              if (r.ok) setChecked(r.invite);
              else setError(r.error);
            })
          }
        >
          {pending ? "Checking..." : "Check code"}
        </button>
      )}
    </div>
  );
}
