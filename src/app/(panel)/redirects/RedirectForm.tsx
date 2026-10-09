"use client";

import { useActionState, useEffect, useState } from "react";
import styles from "@/components/page.module.css";
import local from "./redirects.module.css";

type Result = { ok: true } | { ok: false; error: string };

export default function RedirectForm({
  action,
  domains,
}: {
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
  domains: string[];
}) {
  const [state, formAction, pending] = useActionState<Result | null, FormData>(action, null);
  const [source, setSource] = useState("/");
  const [target, setTarget] = useState("");
  const [matchMode, setMatchMode] = useState<"exact" | "prefix">("prefix");

  // Defaults are the whole-domain case (source `/` + prefix), which is what
  // most people come here for. After a successful add the source goes back
  // to `/` and the target clears; domain, match and type stay put since the
  // operator usually adds several rules for one site in a row.
  useEffect(() => {
    if (state?.ok) {
      setSource("/");
      setTarget("");
    }
  }, [state]);

  return (
    <form action={formAction}>
      <div className={styles.formRow}>
        <label className={styles.field}>
          Domain
          <select name="domain" className={styles.select} defaultValue={domains[0]}>
            {domains.map((d) => (
              <option key={d} value={d}>
                {d}
              </option>
            ))}
          </select>
        </label>
        <label className={styles.field}>
          Source path
          <input
            name="source_path"
            className={styles.input}
            placeholder="/old-page"
            value={source}
            onChange={(e) => setSource(e.target.value)}
            required
            autoComplete="off"
            spellCheck={false}
          />
          <span className={local.hint}>
            {matchMode === "prefix"
              ? "Use / to redirect the whole domain."
              : "Only this exact path, query strings are not part of the match."}
          </span>
        </label>
      </div>

      <div className={styles.formRow}>
        <label className={styles.field}>
          Redirect to
          <input
            name="target_url"
            className={styles.input}
            placeholder="https://example.com/new-page or /new-page"
            value={target}
            onChange={(e) => setTarget(e.target.value)}
            required
            autoComplete="off"
            spellCheck={false}
          />
          <span className={local.hint}>
            {matchMode === "prefix"
              ? "The rest of the path is appended: /blog/2024/post lands on <target>/2024/post."
              : "A full URL or a path on the same domain."}
          </span>
        </label>
        <label className={styles.field}>
          Match
          <select
            name="match_mode"
            className={styles.select}
            value={matchMode}
            onChange={(e) => setMatchMode(e.target.value === "prefix" ? "prefix" : "exact")}
          >
            <option value="exact">Exact path</option>
            <option value="prefix">Path and everything under it</option>
          </select>
        </label>
        <label className={styles.field}>
          Type
          <select name="code" className={styles.select} defaultValue="301">
            <option value="301">301 permanent</option>
            <option value="302">302 temporary</option>
          </select>
          <span className={local.hint}>
            301 is cached by browsers and search engines. Use 302 for anything that might change back.
          </span>
        </label>
      </div>

      {state?.ok === false && <div className={styles.error}>{state.error}</div>}
      {state?.ok === true && <div className={styles.notice}>Redirect added and nginx reloaded.</div>}
      <button type="submit" className={styles.btn} disabled={pending}>
        {pending ? "..." : "Add redirect"}
      </button>
    </form>
  );
}
