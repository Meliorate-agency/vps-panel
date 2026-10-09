"use client";

import { useEffect, useRef, useState } from "react";
import styles from "./settings.module.css";

export type SettingsTab = {
  id: string;
  label: string;
  /** Element ids inside this tab, so /settings#ssh-server opens it. */
  anchors?: string[];
  content: React.ReactNode;
};

/**
 * Groups the settings cards into tabs. Everything is rendered on the
 * server; this only decides which group is on screen, and keeps the choice
 * in the URL (?tab=…) so a reload stays put. Links that point at one
 * setting (/settings#ssh-server from the Users page, #mcp-token from
 * /ai-agent) open the tab holding it and scroll to it.
 */
export default function SettingsTabs({ tabs }: { tabs: SettingsTab[] }) {
  const [active, setActive] = useState(tabs[0]?.id ?? "");
  const fromUrl = useRef(false);

  useEffect(() => {
    if (fromUrl.current) return;
    fromUrl.current = true;
    const url = new URL(window.location.href);
    const hash = url.hash.replace(/^#/, "");
    const byHash = hash ? tabs.find((t) => t.anchors?.includes(hash)) : undefined;
    const byParam = tabs.find((t) => t.id === url.searchParams.get("tab"));
    const next = byHash ?? byParam;
    if (!next) return;
    setActive(next.id);
    if (byHash) {
      requestAnimationFrame(() =>
        document.getElementById(hash)?.scrollIntoView({ block: "start", behavior: "smooth" }),
      );
    }
  }, [tabs]);

  const select = (id: string) => {
    setActive(id);
    const url = new URL(window.location.href);
    url.searchParams.set("tab", id);
    url.hash = "";
    window.history.replaceState(null, "", url.toString());
  };

  const current = tabs.find((t) => t.id === active) ?? tabs[0];
  if (!current) return null;

  return (
    <>
      <div className={styles.tabs} role="tablist" aria-label="Settings sections">
        {tabs.map((t) => (
          <button
            key={t.id}
            type="button"
            role="tab"
            aria-selected={t.id === current.id}
            className={`${styles.tab} ${t.id === current.id ? styles.tabActive : ""}`}
            onClick={() => select(t.id)}
          >
            {t.label}
          </button>
        ))}
      </div>
      <div role="tabpanel">{current.content}</div>
    </>
  );
}
