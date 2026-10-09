"use client";

import { createContext, useContext, useState, useTransition } from "react";
import { createPortal } from "react-dom";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";
import styles from "@/components/page.module.css";
import local from "./networks.module.css";
import type { ActionResult } from "@/server-actions/networks";

type Ask = {
  title: string;
  body: string;
  confirmLabel: string;
  action: () => Promise<ActionResult>;
};

const AskContext = createContext<(ask: Ask) => void>(() => {});

/**
 * The page's confirm popup for Remove / Leave / Delete network. It lives at
 * page level and renders into <body>: the network cards blur their
 * background (backdrop-filter), which traps a fixed-position popup inside
 * the card, and after Leave or Delete the card is gone while the popup may
 * still have something to say (the owner could not be told).
 */
export function ActionDialogs({ children }: { children: React.ReactNode }) {
  const [ask, setAsk] = useState<Ask | null>(null);
  const [result, setResult] = useState<ActionResult | null>(null);
  const [pending, start] = useTransition();

  const close = () => {
    if (pending) return;
    setAsk(null);
    setResult(null);
  };
  const confirm = () => {
    if (!ask) return;
    setResult(null);
    start(async () => {
      const r = await ask.action();
      if (r.ok && !r.message) {
        setAsk(null);
        return;
      }
      setResult(r);
    });
  };
  const done = result?.ok === true;

  return (
    <AskContext.Provider
      value={(a) => {
        setResult(null);
        setAsk(a);
      }}
    >
      {children}
      {ask &&
        createPortal(
          <ConfirmDialog
            open
            title={done ? "Done" : ask.title}
            confirmLabel={ask.confirmLabel}
            tone="danger"
            pending={pending}
            hideActions={done}
            onConfirm={confirm}
            onCancel={close}
          >
            <p>{done && result.message ? result.message : ask.body}</p>
            {result?.ok === false && <div className={styles.error}>{result.error}</div>}
            {done && (
              <div className={local.dialogActions}>
                <button type="button" className={styles.btn} onClick={close}>
                  Close
                </button>
              </div>
            )}
          </ConfirmDialog>,
          document.body,
        )}
    </AskContext.Provider>
  );
}

/** A danger button that asks through the page's popup before it acts. */
export default function ActionButton({
  label,
  title,
  body,
  confirmLabel,
  action,
}: Ask & { label: string }) {
  const ask = useContext(AskContext);
  return (
    <button
      type="button"
      className={styles.btnDanger}
      onClick={() => ask({ title, body, confirmLabel, action })}
    >
      {label}
    </button>
  );
}
