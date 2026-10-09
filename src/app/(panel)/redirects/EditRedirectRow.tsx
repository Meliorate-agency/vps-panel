"use client";

import { useActionState } from "react";
import styles from "@/components/page.module.css";
import local from "./redirects.module.css";

type Result = { ok: true } | { ok: false; error: string };

/** Plain data for one rule - a subset of lib/redirects RedirectRow that is
 *  safe to hand to a client component. */
export type RedirectSummary = {
  id: number;
  domain: string;
  source_path: string;
  target_url: string;
  code: 301 | 302;
  match_mode: "exact" | "prefix";
};

export default function EditRedirectRow({
  redirect,
  action,
}: {
  redirect: RedirectSummary;
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
}) {
  const [state, formAction, pending] = useActionState<Result | null, FormData>(action, null);

  return (
    <details className={local.editDetails} name="redirect-edit">
      <summary className={local.editSummary}>Edit</summary>
      <div className={local.editPanel}>
        <div className={local.editTitle}>
          Edit redirect on <span className={styles.code}>{redirect.domain}</span>
        </div>
        <form action={formAction}>
          <label className={styles.field} style={{ marginBottom: 10 }}>
            Source path
            <input
              name="source_path"
              className={styles.input}
              defaultValue={redirect.source_path}
              required
              autoComplete="off"
              spellCheck={false}
            />
          </label>
          <label className={styles.field} style={{ marginBottom: 10 }}>
            Redirect to
            <input
              name="target_url"
              className={styles.input}
              defaultValue={redirect.target_url}
              required
              autoComplete="off"
              spellCheck={false}
            />
          </label>
          <div className={styles.formRow}>
            <label className={styles.field}>
              Match
              <select name="match_mode" className={styles.select} defaultValue={redirect.match_mode}>
                <option value="exact">Exact path</option>
                <option value="prefix">Path and everything under it</option>
              </select>
            </label>
            <label className={styles.field}>
              Type
              <select name="code" className={styles.select} defaultValue={String(redirect.code)}>
                <option value="301">301 permanent</option>
                <option value="302">302 temporary</option>
              </select>
            </label>
          </div>
          {state?.ok === false && <div className={styles.error}>{state.error}</div>}
          {state?.ok === true && <div className={styles.notice}>Saved and nginx reloaded.</div>}
          <button type="submit" className={styles.btn} disabled={pending}>
            {pending ? "..." : "Save changes"}
          </button>
        </form>
      </div>
    </details>
  );
}
