"use client";

import { useState } from "react";
import styles from "@/components/page.module.css";
import local from "./networks.module.css";
import type { ActionResult, InvitePreview } from "@/server-actions/networks";
import CreateNetworkForm from "./CreateNetworkForm";
import JoinNetworkForm from "./JoinNetworkForm";

type Panel = "create" | "join";

/** Create / Join as two buttons above the networks; the one clicked opens its form below them. */
export default function NetworkSetup({
  createAction,
  previewAction,
  joinAction,
  disabled,
}: {
  createAction: (prev: ActionResult | null, fd: FormData) => Promise<ActionResult>;
  previewAction: (code: string) => Promise<{ ok: true; invite: InvitePreview } | { ok: false; error: string }>;
  joinAction: (code: string) => Promise<ActionResult>;
  disabled: boolean;
}) {
  const [open, setOpen] = useState<Panel | null>(null);
  const toggle = (p: Panel) => setOpen((o) => (o === p ? null : p));

  return (
    <div className={local.setup}>
      <div className={local.setupButtons}>
        <button
          type="button"
          className={styles.btn}
          aria-expanded={open === "create"}
          onClick={() => toggle("create")}
          disabled={disabled}
        >
          + Create network
        </button>
        <button
          type="button"
          className={`${styles.btnGhost} ${local.setupGhost}`}
          aria-expanded={open === "join"}
          onClick={() => toggle("join")}
          disabled={disabled}
        >
          Join a network
        </button>
      </div>

      {open && (
        <div className={`${styles.section} ${local.setupPanel}`}>
          <div className={local.panelHead}>
            <h2 className={styles.sectionTitle}>{open === "create" ? "Create a network" : "Join a network"}</h2>
            <button type="button" className={local.panelClose} onClick={() => setOpen(null)} aria-label="Close" title="Close">
              <svg width="14" height="14" viewBox="0 0 16 16" fill="none" aria-hidden="true">
                <path d="M3 3L13 13M13 3L3 13" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" />
              </svg>
            </button>
          </div>
          {open === "create" ? (
            <>
              <p className={local.panelIntro}>
                This server owns the network: it invites servers with one-time join codes and removes them.
              </p>
              <CreateNetworkForm action={createAction} disabled={disabled} onDone={() => setOpen(null)} />
            </>
          ) : (
            <>
              <p className={local.panelIntro}>
                Paste the code from Invite a server on the network owner&apos;s My Networks page.
              </p>
              <JoinNetworkForm
                preview={previewAction}
                join={joinAction}
                disabled={disabled}
                onDone={() => setOpen(null)}
              />
            </>
          )}
        </div>
      )}
    </div>
  );
}
