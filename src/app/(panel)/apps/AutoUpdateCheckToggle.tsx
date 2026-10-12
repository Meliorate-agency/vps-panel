"use client";

import { useState, useTransition } from "react";
import styles from "./AutoUpdateCheckToggle.module.css";

/**
 * Controls whether /apps runs a `git fetch` per panel-managed app on every
 * page load. On = the Git column is populated automatically (costs one
 * round trip per app). Off = the column stays idle and each row gets a
 * "Check" button that fetches on demand and updates if the app is behind.
 *
 * Covers the operator's own (panel-managed) apps ONLY. Quick-install apps
 * with a registry probe carry their own toggle in their card's settings
 * popup (currently n8n), so this switch never gates a Docker Hub call.
 *
 * Optimistic: the switch flips immediately, then the server action persists
 * and its revalidatePath("/apps") re-renders the list in the new mode via
 * the action response itself - no extra router.refresh(), which would run
 * the whole page's git fetches (and n8n registry probe) a second time.
 */
export default function AutoUpdateCheckToggle({
  enabled,
  action,
}: {
  enabled: boolean;
  action: (enabled: boolean) => Promise<void>;
}) {
  const [on, setOn] = useState(enabled);
  const [pending, startTransition] = useTransition();

  const toggle = () => {
    const next = !on;
    setOn(next);
    startTransition(async () => {
      try {
        await action(next);
      } catch {
        setOn(!next); // revert on failure
      }
    });
  };

  return (
    <div className={styles.row}>
      <button
        type="button"
        role="switch"
        aria-checked={on}
        aria-label="Automatically check your apps for updates on page load"
        onClick={toggle}
        disabled={pending}
        className={`${styles.switch} ${on ? styles.switchOn : ""}`}
      >
        <span className={styles.knob} />
      </button>
      <span className={styles.label} onClick={pending ? undefined : toggle}>
        Automatically check your apps for updates on page load
      </span>
      <span className={styles.hint}>
        {on
          ? "— a git fetch per app (a registry check for Docker apps), every load. Quick-install apps have their own toggle in their settings."
          : "— faster loads; use each app's Check button instead"}
      </span>
    </div>
  );
}
