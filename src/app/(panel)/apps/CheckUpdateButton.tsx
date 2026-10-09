"use client";

import { useEffect, useRef, useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import topbarStyles from "@/components/Topbar/Topbar.module.css";
import pageStyles from "@/components/page.module.css";
import type { CheckUpdateResult } from "@/server-actions/apps";

/** How long the outcome label sticks before reverting to "Check". */
const RESULT_TTL_MS = 6000;

function Spinner() {
  return (
    <svg
      className={topbarStyles.spinner}
      width="12"
      height="12"
      viewBox="0 0 24 24"
      fill="none"
      aria-hidden="true"
    >
      <circle
        cx="12"
        cy="12"
        r="9"
        stroke="currentColor"
        strokeWidth="2.5"
        strokeLinecap="round"
        strokeDasharray="42 60"
      />
    </svg>
  );
}

type Outcome = { label: string; color: string; title: string } | null;

/**
 * On-demand update check for one app, shown when the page-load auto-check is
 * turned off. One click runs `git fetch` on the server and, if the app is
 * behind, immediately starts the usual update task (pull + install + build +
 * restart) and navigates to its live log. Anything else — up to date, no
 * repo, diverged — is reported inline on the button and nothing is changed.
 */
export default function CheckUpdateButton({
  appName,
  checkAction,
}: {
  appName: string;
  checkAction: (name: string) => Promise<CheckUpdateResult>;
}) {
  const router = useRouter();
  const [pending, startTransition] = useTransition();
  const [outcome, setOutcome] = useState<Outcome>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Don't leave a timer running (or setState) after the row unmounts —
  // router.refresh() re-renders this list on any folder/app change.
  useEffect(() => {
    return () => {
      if (timer.current) clearTimeout(timer.current);
    };
  }, []);

  const show = (o: Outcome) => {
    setOutcome(o);
    if (timer.current) clearTimeout(timer.current);
    timer.current = setTimeout(() => setOutcome(null), RESULT_TTL_MS);
  };

  const run = () => {
    setOutcome(null);
    startTransition(async () => {
      const r = await checkAction(appName);
      if (!r.ok) {
        show({ label: "Failed", color: "var(--danger, #ff4d6d)", title: r.error });
        return;
      }
      switch (r.kind) {
        case "updating":
          // Behind → update already started; go watch it.
          router.push(`/tasks/${encodeURIComponent(r.taskId)}`);
          return;
        case "up_to_date":
          show({
            label: "Up to date",
            color: "var(--ok, #4ade80)",
            title: `${appName} is up to date — nothing to pull.`,
          });
          return;
        case "no_git":
          show({
            label: "No git",
            color: "var(--text-dim)",
            title: "Source folder is not a git checkout — nothing to check.",
          });
          return;
        case "ahead":
          show({
            label: `${r.ahead} ahead`,
            color: "var(--text-dim)",
            title: `Local has ${r.ahead} unpushed commit${r.ahead === 1 ? "" : "s"} and nothing to pull.`,
          });
          return;
        case "diverged":
          show({
            label: "Diverged",
            color: "var(--warn, #ffc400)",
            title: `Local is ${r.ahead} ahead and ${r.behind} behind — git pull --ff-only would refuse. Resolve on the server first.`,
          });
          return;
      }
    });
  };

  return (
    <button
      type="button"
      onClick={run}
      disabled={pending}
      className={pageStyles.btnGhost}
      title={
        outcome?.title ??
        "Check this app for updates and rebuild if it's behind"
      }
      style={{
        display: "inline-flex",
        alignItems: "center",
        gap: 5,
        whiteSpace: "nowrap",
        ...(outcome ? { color: outcome.color } : null),
      }}
    >
      {pending ? (
        <>
          <Spinner /> Checking…
        </>
      ) : (
        outcome?.label ?? "Check"
      )}
    </button>
  );
}
