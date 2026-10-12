"use client";

import Link from "next/link";
import { useEffect, useState } from "react";
import styles from "@/components/page.module.css";

/**
 * "Deploy app" entrypoint on /apps. Click opens a centered modal with two
 * source-method cards: GitHub Pull (/apps/new?method=github, a Node app) and
 * Docker (/apps/new?method=docker, an image from a registry).
 */
export default function DeployMethodButton() {
  const [open, setOpen] = useState(false);

  // Body scroll lock + ESC handling, matching the rest of the panel's
  // dialogs. Backdrop click closes; X button closes; ESC closes.
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    const prevOverflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    window.addEventListener("keydown", onKey);
    return () => {
      document.body.style.overflow = prevOverflow;
      window.removeEventListener("keydown", onKey);
    };
  }, [open]);

  return (
    <>
      <button
        type="button"
        className={styles.btn}
        onClick={() => setOpen(true)}
      >
        + Deploy app
      </button>

      {open && (
        <div
          onClick={() => setOpen(false)}
          role="dialog"
          aria-modal="true"
          aria-labelledby="deploy-method-title"
          style={{
            position: "fixed",
            inset: 0,
            background: "var(--scrim)",
            backdropFilter: "blur(6px) saturate(120%)",
            WebkitBackdropFilter: "blur(6px) saturate(120%)",
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            zIndex: 1000,
            padding: 24,
            animation: "fadeIn 0.12s ease-out",
          }}
        >
          <div
            onClick={(e) => e.stopPropagation()}
            style={{
              background: "var(--bg-elev)",
              border: "1px solid var(--border)",
              borderRadius: "var(--radius)",
              padding: "20px 22px 22px",
              width: "100%",
              maxWidth: 560,
              maxHeight: "calc(100vh - 64px)",
              overflowY: "auto",
              boxShadow: "0 24px 64px rgba(0, 0, 0, 0.55)",
              position: "relative",
            }}
          >
            <button
              type="button"
              aria-label="Close"
              onClick={() => setOpen(false)}
              style={{
                position: "absolute",
                top: 10,
                right: 10,
                background: "transparent",
                border: 0,
                color: "var(--text-dim)",
                fontSize: 20,
                lineHeight: 1,
                width: 32,
                height: 32,
                borderRadius: 6,
                cursor: "pointer",
              }}
              onMouseEnter={(e) =>
                ((e.currentTarget as HTMLButtonElement).style.background =
                  "var(--bg-elev-2)")
              }
              onMouseLeave={(e) =>
                ((e.currentTarget as HTMLButtonElement).style.background =
                  "transparent")
              }
            >
              ×
            </button>

            <h3
              id="deploy-method-title"
              style={{
                margin: "0 0 4px 0",
                fontSize: 16,
                fontWeight: 600,
                color: "var(--text)",
              }}
            >
              Deploy app
            </h3>
            <p
              className={styles.muted}
              style={{ fontSize: 13, marginTop: 0, marginBottom: 16 }}
            >
              Pick where the app comes from.
            </p>

            <div
              style={{
                display: "grid",
                gridTemplateColumns: "repeat(auto-fit, minmax(220px, 1fr))",
                gap: 12,
              }}
            >
              <MethodCard
                href="/apps/new?method=github"
                onPick={() => setOpen(false)}
                logo={<GitHubLogo />}
                title="GitHub Pull"
                sub="Clone a public or deploy-keyed repo, then start the app."
              />
              <MethodCard
                href="/apps/new?method=docker"
                onPick={() => setOpen(false)}
                logo={<DockerLogo />}
                title="Docker"
                sub="Run an image from Docker Hub or another registry on one of your domains."
              />
            </div>
          </div>
        </div>
      )}
    </>
  );
}

function MethodCard({
  href,
  onPick,
  logo,
  title,
  sub,
  disabled,
}: {
  href?: string;
  onPick?: () => void;
  logo: React.ReactNode;
  title: string;
  sub: string;
  disabled?: boolean;
}) {
  const inner = (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        gap: 10,
        padding: "18px 16px",
        background: "var(--bg-elev-2)",
        border: "1px solid var(--border)",
        borderRadius: 8,
        cursor: disabled ? "not-allowed" : "pointer",
        opacity: disabled ? 0.25 : 1,
        transition: "border-color 0.12s, transform 0.12s",
        height: "100%",
      }}
      onMouseEnter={(e) => {
        if (disabled) return;
        (e.currentTarget as HTMLDivElement).style.borderColor =
          "var(--accent)";
      }}
      onMouseLeave={(e) => {
        (e.currentTarget as HTMLDivElement).style.borderColor =
          "var(--border)";
      }}
    >
      <div
        style={{
          width: 36,
          height: 36,
          display: "flex",
          alignItems: "center",
          justifyContent: "flex-start",
        }}
      >
        {logo}
      </div>
      <div style={{ fontSize: 14, fontWeight: 600, color: "var(--text)" }}>
        {title}
        {disabled && (
          <span
            style={{
              marginLeft: 8,
              fontSize: 10,
              fontWeight: 700,
              letterSpacing: 0.5,
              textTransform: "uppercase",
              padding: "2px 6px",
              borderRadius: 4,
              background: "var(--bg-elev)",
              color: "var(--text-dim)",
              verticalAlign: "middle",
            }}
          >
            Coming soon
          </span>
        )}
      </div>
      <div
        className={styles.muted}
        style={{ fontSize: 12, lineHeight: 1.45 }}
      >
        {sub}
      </div>
    </div>
  );

  if (disabled || !href) {
    return <div aria-disabled="true">{inner}</div>;
  }
  return (
    <Link
      href={href}
      onClick={() => onPick?.()}
      style={{ textDecoration: "none", color: "inherit" }}
    >
      {inner}
    </Link>
  );
}

// Official GitHub Octocat mark, simplified. Public-domain shape used widely
// in service-picker UIs (the GitHub logo is a registered trademark; this
// rendering matches their published brand guidelines for a monochrome
// "mark" usage at this size).
function GitHubLogo() {
  return (
    <svg
      viewBox="0 0 24 24"
      width="36"
      height="36"
      fill="currentColor"
      aria-hidden="true"
      style={{ color: "var(--text)" }}
    >
      <path
        fillRule="evenodd"
        clipRule="evenodd"
        d="M12 0C5.37 0 0 5.37 0 12c0 5.3 3.438 9.8 8.205 11.387.6.111.82-.26.82-.577 0-.286-.011-1.244-.017-2.257-3.338.726-4.043-1.412-4.043-1.412-.546-1.387-1.333-1.757-1.333-1.757-1.089-.745.083-.73.083-.73 1.205.084 1.84 1.236 1.84 1.236 1.07 1.834 2.807 1.304 3.492.997.108-.776.418-1.305.762-1.605-2.665-.303-5.467-1.332-5.467-5.93 0-1.31.468-2.382 1.235-3.222-.124-.303-.535-1.523.117-3.176 0 0 1.008-.322 3.3 1.23a11.5 11.5 0 0 1 3.003-.404c1.02.005 2.047.138 3.005.404 2.29-1.552 3.297-1.23 3.297-1.23.653 1.653.242 2.873.118 3.176.77.84 1.234 1.912 1.234 3.222 0 4.61-2.807 5.624-5.48 5.922.43.371.814 1.103.814 2.222 0 1.604-.015 2.896-.015 3.293 0 .32.217.694.825.576C20.565 21.795 24 17.297 24 12c0-6.63-5.373-12-12-12z"
      />
    </svg>
  );
}

// Docker moby whale silhouette, simplified outline. Same brand-guidelines
// argument as above; this is the standard monochrome mark Docker publishes
// for app integrators.
function DockerLogo() {
  return (
    <svg
      viewBox="0 0 24 24"
      width="36"
      height="36"
      fill="currentColor"
      aria-hidden="true"
      style={{ color: "var(--text)" }}
    >
      <path d="M13.983 11.078h2.119a.186.186 0 0 0 .186-.185V9.006a.186.186 0 0 0-.186-.186h-2.119a.185.185 0 0 0-.185.185v1.888c0 .102.083.185.185.185m-2.954-5.43h2.119a.186.186 0 0 0 .185-.186V3.574a.186.186 0 0 0-.185-.185h-2.119a.185.185 0 0 0-.186.185v1.888c0 .102.084.185.186.185m0 2.716h2.119a.187.187 0 0 0 .185-.186V6.29a.186.186 0 0 0-.185-.185h-2.119a.185.185 0 0 0-.186.185v1.887c0 .102.084.185.186.186m-2.93 0h2.12a.186.186 0 0 0 .184-.186V6.29a.185.185 0 0 0-.185-.185H8.1a.185.185 0 0 0-.185.185v1.887c0 .102.083.185.185.186m-2.964 0h2.119a.186.186 0 0 0 .185-.186V6.29a.185.185 0 0 0-.185-.185H5.136a.185.185 0 0 0-.186.185v1.887c0 .102.084.185.186.186m5.893 2.715h2.12a.186.186 0 0 0 .184-.186V9.006a.185.185 0 0 0-.185-.186h-2.119a.185.185 0 0 0-.185.185v1.888c0 .102.083.185.185.185m-2.93 0h2.12a.185.185 0 0 0 .184-.186V9.006a.185.185 0 0 0-.184-.186h-2.12a.185.185 0 0 0-.185.185v1.888c0 .102.083.185.185.185m-2.964 0h2.119a.185.185 0 0 0 .185-.186V9.006a.185.185 0 0 0-.184-.186h-2.12a.185.185 0 0 0-.186.186v1.887c0 .102.084.185.186.185m-2.92 0h2.12a.185.185 0 0 0 .184-.186V9.006a.185.185 0 0 0-.184-.186h-2.12a.185.185 0 0 0-.185.185v1.888c0 .102.082.185.185.185M23.763 9.89c-.065-.051-.672-.51-1.954-.51-.338.001-.676.03-1.01.087-.248-1.7-1.653-2.53-1.716-2.566l-.344-.199-.226.327c-.284.438-.49.922-.612 1.43-.23.97-.09 1.882.403 2.661-.595.332-1.55.413-1.744.42H.751a.751.751 0 0 0-.75.748 11.376 11.376 0 0 0 .692 4.062c.545 1.428 1.355 2.48 2.41 3.124 1.18.723 3.1 1.137 5.275 1.137a16.09 16.09 0 0 0 2.91-.262 12.5 12.5 0 0 0 3.795-1.376 10.41 10.41 0 0 0 2.598-2.137c1.236-1.398 1.972-2.953 2.516-4.337h.218c1.376 0 2.221-.55 2.689-1.011.31-.295.553-.654.712-1.053l.098-.286Z" />
    </svg>
  );
}
