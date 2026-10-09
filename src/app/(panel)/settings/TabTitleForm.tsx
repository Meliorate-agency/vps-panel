"use client";

import { useState, useTransition } from "react";
import BoltMark from "@/components/BoltMark/BoltMark";
import shared from "@/components/page.module.css";
import styles from "./TabTitleForm.module.css";
import { DEFAULT_TAB_TITLE, MAX_TAB_TITLE, tabTitleValue } from "@/lib/tabTitle";

type Result = { ok: true } | { ok: false; error: string };

/**
 * Settings > Appearance: the panel's name in browser tabs, with a mock tab
 * that shows it next to the favicon. The favicon is the BoltMark in the
 * theme's three accent shades; the mark here reads them from the same CSS
 * variables, so it follows the theme form above, saved or only previewed.
 */
export default function TabTitleForm({
  value,
  host,
  action,
}: {
  value: string;
  host: string;
  action: (name: string) => Promise<Result>;
}) {
  const [name, setName] = useState(value);
  const [pending, start] = useTransition();
  const [msg, setMsg] = useState<Result | null>(null);
  const shown = name.replace(/\s+/g, " ").trim() || DEFAULT_TAB_TITLE;

  const save = () => {
    const r = tabTitleValue(name);
    if ("error" in r) {
      setMsg({ ok: false, error: r.error });
      return;
    }
    start(async () => {
      setMsg(await action(r.value));
    });
  };

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        save();
      }}
    >
      <div className={styles.browser} aria-hidden="true">
        <div className={styles.tabs}>
          <div className={`${styles.tab} ${styles.active}`}>
            <BoltMark size={16} className={styles.icon} />
            <span className={styles.title}>{shown}</span>
            <span className={styles.close}>×</span>
          </div>
          <div className={styles.tab}>
            <span className={styles.blank} />
            <span className={styles.title}>New tab</span>
          </div>
        </div>
        <div className={styles.bar}>
          <span className={styles.url}>{host}</span>
        </div>
      </div>
      <div className={styles.row}>
        <label className={`${shared.field} ${styles.field}`}>
          Tab name
          <input
            type="text"
            value={name}
            onChange={(e) => {
              setName(e.target.value);
              setMsg(null);
            }}
            placeholder={DEFAULT_TAB_TITLE}
            maxLength={MAX_TAB_TITLE}
            className={shared.input}
            autoComplete="off"
          />
        </label>
        <button type="submit" className={shared.btn} disabled={pending}>
          {pending ? "..." : "Save"}
        </button>
      </div>
      {msg?.ok === false && (
        <div className={shared.error} style={{ marginTop: 10 }}>
          {msg.error}
        </div>
      )}
      {msg?.ok === true && (
        <div className={shared.notice} style={{ marginTop: 10 }}>
          Saved. Other open tabs show it after their next page load.
        </div>
      )}
    </form>
  );
}
