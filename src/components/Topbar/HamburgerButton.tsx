"use client";

import styles from "./Topbar.module.css";
import { SIDEBAR_TOGGLE_EVENT } from "@/components/Sidebar/Sidebar";

export default function HamburgerButton() {
  return (
    <button
      type="button"
      className={styles.hamburger}
      onClick={() => window.dispatchEvent(new Event(SIDEBAR_TOGGLE_EVENT))}
      aria-label="Open menu"
    >
      <svg width="20" height="20" viewBox="0 0 20 20" fill="none" aria-hidden="true">
        <path d="M3 5h14M3 10h14M3 15h14" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" />
      </svg>
    </button>
  );
}
