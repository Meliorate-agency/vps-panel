"use client";

import { useActionState, useState } from "react";
import styles from "@/components/page.module.css";

type Result = { ok: true } | { ok: false; error: string };

type Props = {
  domain: string;
  initialMode: "port" | "folder";
  initialUpstreamPort: number | null;
  initialFolderPath: string | null;
  initialIssueSsl: boolean;
  initialForceWwwRedirect: boolean;
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
};

export default function EditRoutingForm({
  domain,
  initialMode,
  initialUpstreamPort,
  initialFolderPath,
  initialIssueSsl,
  initialForceWwwRedirect,
  action,
}: Props) {
  const [state, formAction, pending] = useActionState<Result | null, FormData>(action, null);
  const [mode, setMode] = useState<"folder" | "port">(initialMode);
  const isSubdomain =
    domain.split(".").filter((l) => l.length > 0).length >= 3;

  return (
    <form action={formAction}>
      <div className={styles.formRow}>
        <label className={styles.field}>
          Mode
          <select
            name="mode"
            className={styles.select}
            value={mode}
            onChange={(e) => setMode(e.target.value as "folder" | "port")}
          >
            <option value="folder">Static folder</option>
            <option value="port">Reverse proxy to port</option>
          </select>
        </label>
        {mode === "folder" ? (
          <label className={styles.field}>
            Folder path
            <input
              name="folder_path"
              className={styles.input}
              defaultValue={initialFolderPath ?? `/var/www/${domain}`}
              placeholder={`/var/www/${domain}`}
              autoComplete="off"
            />
          </label>
        ) : (
          <label className={styles.field}>
            Upstream port
            <input
              name="upstream_port"
              type="number"
              min={1}
              max={65535}
              required
              defaultValue={initialUpstreamPort ?? undefined}
              placeholder="3000"
              className={styles.input}
            />
          </label>
        )}
      </div>
      <div style={{ display: "flex", gap: 12, alignItems: "center", flexWrap: "wrap" }}>
        <label className={styles.checkbox}>
          <input type="checkbox" name="issue_ssl" defaultChecked={initialIssueSsl} />
          Auto-issue SSL
        </label>
        <label className={styles.checkbox}>
          <input
            type="checkbox"
            name="force_www_redirect"
            defaultChecked={initialForceWwwRedirect && !isSubdomain}
            disabled={isSubdomain}
          />
          Force www → non-www
          {isSubdomain ? (
            <span style={{ marginLeft: 8, opacity: 0.6, fontSize: "0.85em" }}>
              (only applies to apex domains)
            </span>
          ) : null}
        </label>
        <button type="submit" className={styles.btn} disabled={pending}>
          {pending ? "Saving..." : "Save"}
        </button>
      </div>
      {state?.ok === false && (
        <div className={styles.error} style={{ marginTop: 8 }}>{state.error}</div>
      )}
      {state?.ok === true && (
        <div className={styles.notice} style={{ marginTop: 8 }}>Routing updated.</div>
      )}
    </form>
  );
}
