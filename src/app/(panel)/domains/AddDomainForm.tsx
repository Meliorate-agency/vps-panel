"use client";

import { useRouter } from "next/navigation";
import { useActionState, useEffect, useMemo, useRef, useState } from "react";
import styles from "@/components/page.module.css";
import TaskProgress from "@/components/TaskProgress/TaskProgress";

type Result =
  | { ok: true; taskId?: string; warnings?: string[] }
  | { ok: false; error: string }
  | { ok: false; error: string; conflict: "agent_only"; domain: string };

type Action = (prev: Result | null, fd: FormData) => Promise<Result>;

/**
 * Walk up the labels of `domain` and return the first one that's already a
 * managed zone. Same logic as findParentManagedZone in src/server-actions/sites.ts
 * — duplicated here because that file is "use server" and can't be imported
 * into a client component.
 */
function findParentManagedZone(
  domain: string,
  managedZones: string[],
): string | null {
  if (!domain.includes(".")) return null;
  const zoneSet = new Set(managedZones);
  const labels = domain.split(".");
  for (let i = 1; i < labels.length; i++) {
    const candidate = labels.slice(i).join(".");
    if (zoneSet.has(candidate)) return candidate;
  }
  return null;
}

export default function AddDomainForm({
  action,
  enableRecovery = false,
  lockedDomain,
  prefillDomain,
  prefillUpstreamPort,
  submitLabel = "Add domain",
  managedZones = [],
}: {
  action: Action;
  enableRecovery?: boolean;
  lockedDomain?: string;
  /**
   * If set, initializes the Domain input with this value (editable). Used
   * by the "Set up HTTPS site" CTA on a dnsSubdomain row in /domains: it
   * links here with ?prefill=<subdomain> so the operator lands on a
   * one-click-ready form. Ignored if `lockedDomain` is also set (lock wins).
   */
  prefillDomain?: string;
  /**
   * If set, initializes Mode = "Reverse proxy to port" and the Upstream
   * port input. Used by the "Route subdomain" link on
   * QuickInstallStrip cards (e.g. phpMyAdmin → 8081) so the operator
   * lands on a form that only needs the subdomain typed before submit.
   */
  prefillUpstreamPort?: number;
  submitLabel?: string;
  /**
   * List of PowerDNS zone names already managed by the panel. Used to detect
   * when the user is entering a subdomain of an existing zone, so we can hint
   * that we'll add an A record in the parent zone instead of creating a new
   * one.
   */
  managedZones?: string[];
}) {
  const router = useRouter();
  const [state, formAction, pending] = useActionState<Result | null, FormData>(
    action,
    null,
  );
  const [mode, setMode] = useState<"folder" | "port" | "dns_only">(
    prefillUpstreamPort !== undefined ? "port" : "folder",
  );
  const [domain, setDomain] = useState(lockedDomain ?? prefillDomain ?? "");
  const formRef = useRef<HTMLFormElement>(null);

  // When the page was loaded with ?prefill, gently scroll the form into view
  // so the operator sees the pre-filled value without hunting for it. Runs
  // once on mount.
  useEffect(() => {
    if (prefillDomain && !lockedDomain) {
      formRef.current?.scrollIntoView({ behavior: "smooth", block: "start" });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  // Sync the input with the URL prefill so soft-navigation between rows
  // (`/domains?prefill=foo` -> `/domains?prefill=bar`) actually updates the
  // field. Without this, the form's useState initializer runs once and the
  // input stays stuck at whatever the first mount value was.
  useEffect(() => {
    if (lockedDomain) return;
    setDomain(prefillDomain ?? "");
  }, [prefillDomain, lockedDomain]);
  const [folderPath, setFolderPath] = useState("");
  const [upstreamPort, setUpstreamPort] = useState(
    prefillUpstreamPort !== undefined ? String(prefillUpstreamPort) : "",
  );
  const [issueSsl, setIssueSsl] = useState(true);
  const [forceWwwRedirect, setForceWwwRedirect] = useState(true);
  const [createDnsZone, setCreateDnsZone] = useState(true);
  const [configureMail, setConfigureMail] = useState(true);

  const parentZone = useMemo(
    () => findParentManagedZone(domain, managedZones),
    [domain, managedZones],
  );
  const subdomainLabel = parentZone
    ? domain.slice(0, domain.length - parentZone.length - 1)
    : null;
  const isSubdomain = useMemo(() => {
    const labels = domain.split(".").filter((l) => l.length > 0);
    return labels.length >= 3;
  }, [domain]);

  const conflict =
    state?.ok === false && "conflict" in state && state.conflict === "agent_only"
      ? state
      : null;

  function handleReplace(e: React.MouseEvent<HTMLButtonElement>) {
    if (
      !window.confirm(
        `This will delete the existing nginx config for ${conflict?.domain ?? domain} on the server and replace it with these form values. Continue?`,
      )
    ) {
      e.preventDefault();
    }
  }

  return (
    <form action={formAction} ref={formRef} id="add-domain">
      <div className={styles.formRow}>
        {lockedDomain ? (
          <input type="hidden" name="domain" value={lockedDomain} />
        ) : (
          <label className={styles.field}>
            Domain
            <input
              name="domain"
              className={styles.input}
              required
              placeholder="example.com"
              autoComplete="off"
              value={domain}
              onChange={(e) => setDomain(e.target.value.trim().toLowerCase())}
            />
          </label>
        )}
        <label className={styles.field}>
          Mode
          <select
            name="mode"
            className={styles.select}
            value={mode}
            onChange={(e) =>
              setMode(e.target.value as "folder" | "port" | "dns_only")
            }
          >
            <option value="folder">Static folder</option>
            <option value="port">Reverse proxy to port</option>
            <option value="dns_only">DNS only (no nginx, no SSL)</option>
          </select>
        </label>
        {mode === "folder" && (
          <label className={styles.field}>
            Folder path
            <input
              name="folder_path"
              className={styles.input}
              placeholder={domain ? `/var/www/${domain}` : "/var/www/example.com"}
              autoComplete="off"
              value={folderPath}
              onChange={(e) => setFolderPath(e.target.value)}
            />
          </label>
        )}
        {mode === "port" && (
          <label className={styles.field}>
            Upstream port
            <input
              name="upstream_port"
              type="number"
              min={1}
              max={65535}
              required
              placeholder="3000"
              className={styles.input}
              value={upstreamPort}
              onChange={(e) => setUpstreamPort(e.target.value)}
            />
          </label>
        )}
      </div>
      {parentZone && createDnsZone && (
        <div className={styles.notice} style={{ marginTop: 8, marginBottom: 8 }}>
          <strong>Subdomain detected.</strong>{" "}
          <span className={styles.code}>{parentZone}</span> is already a managed
          zone. Instead of creating a new zone for{" "}
          <span className={styles.code}>{domain}</span>, the panel will add an{" "}
          <span className={styles.code}>A</span> record named{" "}
          <span className={styles.code}>{subdomainLabel}</span> inside{" "}
          <span className={styles.code}>{parentZone}</span>.{" "}
          {configureMail && (
            <>
              The mail configuration toggle will still set up DKIM/SPF/DMARC at{" "}
              <span className={styles.code}>{domain}</span> &mdash; usually you
              want mail configured at the parent (
              <span className={styles.code}>{parentZone}</span>) instead. Uncheck{" "}
              <em>Configure mail</em> if this is just a hostname, not a separate
              mail domain.
            </>
          )}
        </div>
      )}
      <div style={{ display: "flex", gap: 12, alignItems: "center", flexWrap: "wrap" }}>
        <label className={styles.checkbox}>
          <input
            type="checkbox"
            name="create_dns_zone"
            checked={createDnsZone}
            onChange={(e) => setCreateDnsZone(e.target.checked)}
          />
          {parentZone ? "Add record in parent zone" : "Create DNS zone"}
        </label>
        <label className={styles.checkbox}>
          <input
            type="checkbox"
            name="configure_mail"
            checked={configureMail && !isSubdomain}
            disabled={isSubdomain}
            onChange={(e) => setConfigureMail(e.target.checked)}
          />
          Configure mail (DKIM/MX/SPF/DMARC)
          {isSubdomain ? (
            <span style={{ marginLeft: 8, opacity: 0.6, fontSize: "0.85em" }}>
              (only applies to apex domains)
            </span>
          ) : null}
        </label>
        <label className={styles.checkbox}>
          <input
            type="checkbox"
            name="issue_ssl"
            checked={issueSsl && mode !== "dns_only"}
            disabled={mode === "dns_only"}
            onChange={(e) => setIssueSsl(e.target.checked)}
          />
          Auto-issue SSL
        </label>
        <label className={styles.checkbox}>
          <input
            type="checkbox"
            name="force_www_redirect"
            checked={forceWwwRedirect && !isSubdomain && mode !== "dns_only"}
            disabled={isSubdomain || mode === "dns_only"}
            onChange={(e) => setForceWwwRedirect(e.target.checked)}
          />
          Force www → non-www
          {isSubdomain ? (
            <span style={{ marginLeft: 8, opacity: 0.6, fontSize: "0.85em" }}>
              (only applies to apex domains)
            </span>
          ) : null}
        </label>
        <button
          type="submit"
          name="intent"
          value="create"
          className={styles.btn}
          disabled={pending}
        >
          {pending ? "..." : submitLabel}
        </button>
      </div>
      {conflict && enableRecovery ? (
        <div className={styles.notice} style={{ marginTop: 10 }}>
          <div style={{ marginBottom: 6 }}>
            Domain <strong>{conflict.domain}</strong> already exists on the
            server but is not registered in the panel. Pick one:
          </div>
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
            <button
              type="submit"
              name="intent"
              value="replace"
              className={styles.btnDanger}
              onClick={handleReplace}
              disabled={pending}
            >
              Replace existing
            </button>
            <button
              type="submit"
              name="intent"
              value="adopt"
              className={styles.btnGhost}
              disabled={pending}
            >
              Adopt existing
            </button>
          </div>
          <div className={styles.muted} style={{ marginTop: 6, fontSize: 12 }}>
            Replace deletes the existing nginx config and recreates it from
            these form values. Adopt keeps the existing config and just
            registers it in the panel — edit afterwards if anything differs.
          </div>
        </div>
      ) : (
        state?.ok === false && (
          <div className={styles.error} style={{ marginTop: 8 }}>
            {state.error}
          </div>
        )
      )}
      {state?.ok === true && state.warnings && state.warnings.length > 0 && (
        <div className={styles.error} style={{ marginTop: 12 }}>
          {state.warnings.map((w) => (
            <div key={w}>{w}</div>
          ))}
        </div>
      )}
      {state?.ok === true && state.taskId && (
        <div className={styles.notice} style={{ marginTop: 12 }}>
          <div style={{ marginBottom: 8 }}>
            Site created. Let&apos;s Encrypt issuance is running in the
            background — live log below. You can leave this page; the row on{" "}
            <span className={styles.code}>/domains</span> will flip to a green
            SSL badge once the cert lands (or to a Retry button with the
            failure reason if pre-flight or ACME refuses).
          </div>
          <TaskProgress
            taskId={state.taskId}
            onComplete={(status) => {
              // Trigger a soft refresh so the row in the list table re-renders
              // with the fresh `sites` + cert status. Without this the badge
              // is stuck on "issuing..." even though the task succeeded and
              // the cert is already on disk. sslReconciler also revalidates
              // server-side, but the operator is currently sitting on this
              // page — we need a client-side nudge to actually re-fetch.
              router.refresh();
              void status;
            }}
          />
        </div>
      )}
      {state?.ok === true && !state.taskId && (
        <div className={styles.notice} style={{ marginTop: 12 }}>
          Site created. No SSL task was started — either the existing cert
          already covers this name, or you submitted with{" "}
          <strong>Auto-issue SSL</strong> off. Issue a cert later from{" "}
          <span className={styles.code}>/domains</span>.
        </div>
      )}
    </form>
  );
}
