"use client";

import { useEffect, useMemo, useState } from "react";
import Link, { useLinkStatus } from "next/link";
import { usePathname } from "next/navigation";
import BoltMark from "@/components/BoltMark/BoltMark";
import NavIcon, { type NavIconName } from "./NavIcon";
import styles from "./Sidebar.module.css";

type NavItem = { href: string; label: string; icon: NavIconName };

const SECTIONS: { group: string; items: NavItem[] }[] = [
  {
    group: "Overview",
    items: [
      { href: "/dashboard", label: "Dashboard", icon: "dashboard" },
    ],
  },
  {
    group: "Workloads",
    items: [
      { href: "/apps", label: "Apps", icon: "apps" },
      { href: "/cron", label: "Cron jobs", icon: "cron" },
      { href: "/services", label: "System services", icon: "services" },
    ],
  },
  {
    // Drive deep-links into the Files browser's `drive` root. Same machinery,
    // separate entry: "my files" and "the server's guts" are different jobs
    // and shouldn't share a starting point.
    group: "Storage",
    items: [
      { href: "/files/drive", label: "Drive", icon: "drive" },
      { href: "/files", label: "Files", icon: "files" },
    ],
  },
  {
    group: "Networking",
    items: [
      { href: "/domains", label: "Domains", icon: "domains" },
      { href: "/redirects", label: "Redirects", icon: "redirects" },
      { href: "/networks", label: "My Networks", icon: "networks" },
    ],
  },
  {
    group: "Mail & data",
    items: [
      { href: "/mailboxes", label: "Mailboxes", icon: "mailboxes" },
      { href: "/databases", label: "Databases", icon: "databases" },
      { href: "/backups", label: "Backups", icon: "backups" },
    ],
  },
  {
    group: "Operations",
    items: [
      { href: "/terminal", label: "Terminal", icon: "terminal" },
      { href: "/logs", label: "Logs", icon: "logs" },
      { href: "/tasks", label: "Tasks", icon: "tasks" },
      { href: "/audit", label: "Audit log", icon: "audit" },
      { href: "/firewall", label: "Firewall & bans", icon: "firewall" },
    ],
  },
  {
    group: "Account",
    items: [
      { href: "/users", label: "Users", icon: "users" },
      { href: "/ssh-keys", label: "SSH keys", icon: "sshKeys" },
      { href: "/notifications", label: "Notifications", icon: "notifications" },
      { href: "/settings", label: "Settings", icon: "settings" },
      { href: "/ai-agent", label: "MCP", icon: "mcp" },
    ],
  },
];

export const SIDEBAR_TOGGLE_EVENT = "vps-panel:toggle-sidebar";

export default function Sidebar() {
  const pathname = usePathname();
  const [open, setOpen] = useState(false);

  // Longest match wins. A plain per-item prefix test would light up BOTH
  // "Drive" (/files/drive) and "Files" (/files) while browsing the drive,
  // since /files/drive is prefixed by /files.
  const activeHref = useMemo(() => {
    let best = "";
    for (const section of SECTIONS) {
      for (const item of section.items) {
        const matches = pathname === item.href || pathname.startsWith(item.href + "/");
        if (matches && item.href.length > best.length) best = item.href;
      }
    }
    return best;
  }, [pathname]);

  useEffect(() => {
    setOpen(false);
  }, [pathname]);

  useEffect(() => {
    const onToggle = () => setOpen((v) => !v);
    window.addEventListener(SIDEBAR_TOGGLE_EVENT, onToggle);
    return () => window.removeEventListener(SIDEBAR_TOGGLE_EVENT, onToggle);
  }, []);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open]);

  useEffect(() => {
    if (typeof document === "undefined") return;
    document.body.classList.toggle(styles.bodyLock, open);
    return () => document.body.classList.remove(styles.bodyLock);
  }, [open]);

  return (
    <>
      <div
        className={[styles.backdrop, open ? styles.backdropOpen : ""].join(" ").trim()}
        onClick={() => setOpen(false)}
        aria-hidden="true"
      />
      <nav className={[styles.nav, open ? styles.navOpen : ""].join(" ").trim()}>
        <div className={styles.brand}>
          <BoltMark size={24} />
          <div className={styles.brandText}>Bolt Panel</div>
          <button
            type="button"
            className={styles.closeBtn}
            onClick={() => setOpen(false)}
            aria-label="Close menu"
          >
            <svg width="16" height="16" viewBox="0 0 16 16" fill="none" aria-hidden="true">
              <path d="M3 3L13 13M13 3L3 13" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" />
            </svg>
          </button>
        </div>
        {SECTIONS.map((section) => (
          <div key={section.group} className={styles.group}>
            <div className={styles.groupTitle}>{section.group}</div>
            <ul className={styles.list}>
              {section.items.map((item) => {
                const active = item.href === activeHref;
                return (
                  <li key={item.href}>
                    <Link
                      href={item.href}
                      className={[styles.link, active ? styles.active : ""].join(" ").trim()}
                      onClick={() => setOpen(false)}
                    >
                      <SidebarLinkLabel icon={item.icon} label={item.label} />
                    </Link>
                  </li>
                );
              })}
            </ul>
          </div>
        ))}
      </nav>
    </>
  );
}

function SidebarLinkLabel({ icon, label }: { icon: NavIconName; label: string }) {
  const { pending } = useLinkStatus();
  return (
    <span className={styles.linkRow}>
      <NavIcon name={icon} className={styles.linkIcon} />
      <span className={styles.linkText}>{label}</span>
      {pending && <span className={styles.linkSpinner} aria-hidden="true" />}
    </span>
  );
}
