"use client";

import { useRef, useState } from "react";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";
import StatusBadge from "@/components/StatusBadge/StatusBadge";
import styles from "@/components/page.module.css";
import type { SubjectEraseItem, SubjectKind, SubjectSource } from "@/lib/agent";
import type { SubjectSearch } from "@/lib/privacy";
import type { PrivacyDataResult } from "@/server-actions/privacy";
import mail from "./mailIp.module.css";
import { Busy, errorText } from "./mailIpParts";
import local from "./privacy.module.css";

type SearchAction = (kind: string, value: string) => Promise<PrivacyDataResult<SubjectSearch>>;
type EraseAction = (
  kind: string,
  value: string,
  sources: string[],
) => Promise<PrivacyDataResult<SubjectEraseItem[]>>;

/** What an erase did, with the titles the sources had when it was started. */
type Erased = { items: SubjectEraseItem[]; labels: Record<string, string>; at: string };

const KINDS: { value: SubjectKind; label: string; placeholder: string }[] = [
  { value: "email", label: "Email address", placeholder: "name@example.com" },
  { value: "ip", label: "IP address", placeholder: "203.0.113.87" },
];

function records(n: number, more = false): string {
  return `${n}${more ? "+" : ""} ${n === 1 && !more ? "record" : "records"}`;
}

function clock(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleTimeString();
}

/**
 * Settings > Privacy > Personal data request, admins only: search what the
 * server and the panel hold about one email address or IP address, download
 * the result, erase what can be erased, then search again.
 *
 * The value only ever travels in a server action's POST body: never in a
 * link or the page address, which would put it into logs. The searches are
 * not transitions: a Save elsewhere on the page must not wait for them.
 */
export default function PrivacyRequest({
  searchAction,
  eraseAction,
}: {
  searchAction: SearchAction;
  eraseAction: EraseAction;
}) {
  const [kind, setKind] = useState<SubjectKind>("email");
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState<"search" | "erase" | null>(null);
  const [report, setReport] = useState<SubjectSearch | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [erased, setErased] = useState<Erased | null>(null);
  const [confirming, setConfirming] = useState(false);
  const [picked, setPicked] = useState<string[]>([]);
  // Only the newest request may show its result.
  const seq = useRef(0);

  const search = async (k: SubjectKind, v: string, id: number) => {
    setBusy("search");
    let r: PrivacyDataResult<SubjectSearch>;
    try {
      r = await searchAction(k, v);
    } catch (e: unknown) {
      r = { ok: false, error: `The search did not finish: ${errorText(e)}` };
    }
    if (id !== seq.current) return;
    setBusy(null);
    if (r.ok) {
      setReport(r.data);
      setError(null);
    } else {
      setReport(null);
      setError(r.error);
    }
  };

  const submit = () => {
    if (busy) return;
    const v = value.trim();
    if (!v) {
      setError(kind === "email" ? "Enter an email address." : "Enter an IP address.");
      return;
    }
    // The list on screen is about the search before: it goes while this one runs.
    setErased(null);
    setReport(null);
    setError(null);
    void search(kind, v, ++seq.current);
  };

  const erasable = report ? report.sources.filter((s) => s.erasable && s.count > 0) : [];
  const kept = report ? report.sources.filter((s) => !s.erasable && s.count > 0) : [];
  const total = report ? report.sources.reduce((sum, s) => sum + s.count, 0) : 0;

  const erase = async () => {
    if (busy || !report || picked.length === 0) return;
    setConfirming(false);
    const { kind: k, value: v } = report;
    const labels = Object.fromEntries(report.sources.map((s) => [s.id, s.label]));
    const id = ++seq.current;
    setBusy("erase");
    setError(null);
    let r: PrivacyDataResult<SubjectEraseItem[]>;
    try {
      r = await eraseAction(k, v, picked);
    } catch (e: unknown) {
      r = {
        ok: false,
        error: `The erase did not finish: ${errorText(e)}. It may still be running on the server: search again in a few minutes.`,
      };
    }
    if (id !== seq.current) return;
    if (!r.ok) {
      setBusy(null);
      setError(r.error);
      return;
    }
    setErased({ items: r.data, labels, at: new Date().toISOString() });
    // The list is from before the erase: search again for what is left now.
    setReport(null);
    await search(k, v, id);
  };

  const download = () => {
    if (!report) return;
    const data = {
      generated_at: report.searched_at,
      subject: { kind: report.kind, value: report.value },
      // Set when only part of the server could be searched.
      incomplete: report.agent_error,
      about:
        "count is the number of matching records in a source; samples holds up to 20 of them, newest first; truncated means more exist than were counted or sampled.",
      sources: report.sources,
    };
    const url = URL.createObjectURL(new Blob([JSON.stringify(data, null, 2)], { type: "application/json" }));
    const a = document.createElement("a");
    a.href = url;
    // No address in the file name: file names show up in download lists.
    a.download = `personal-data-request-${report.searched_at.slice(0, 19).replace(/[:T]/g, "-")}.json`;
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  };

  const current = KINDS.find((k) => k.value === kind) ?? KINDS[0];

  return (
    <div>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          submit();
        }}
      >
        <fieldset className={local.kinds} disabled={busy !== null}>
          <legend className={mail.legend}>Search by</legend>
          {KINDS.map((k) => (
            <label key={k.value} className={local.kind}>
              <input
                type="radio"
                name="privacy-subject-kind"
                value={k.value}
                checked={kind === k.value}
                onChange={() => setKind(k.value)}
              />
              {k.label}
            </label>
          ))}
        </fieldset>
        <div className={local.inline}>
          <label className={`${styles.field} ${local.searchField}`}>
            {current.label}
            <input
              className={styles.input}
              name="privacy-subject-value"
              type="text"
              inputMode={kind === "email" ? "email" : "text"}
              autoComplete="off"
              autoCapitalize="none"
              spellCheck={false}
              maxLength={254}
              placeholder={current.placeholder}
              value={value}
              disabled={busy !== null}
              onChange={(e) => setValue(e.target.value)}
            />
          </label>
          <button type="submit" className={styles.btn} disabled={busy !== null} aria-busy={busy === "search"}>
            {busy === "search" ? <Busy label="Searching..." /> : "Search"}
          </button>
        </div>
        {busy === "search" && (
          <p className={mail.sub} style={{ marginTop: 8 }}>
            Reading through the logs. On a server with large logs this takes a few minutes.
          </p>
        )}
      </form>

      <div aria-live="polite">
        {busy === "erase" && (
          <p className={mail.sub} style={{ marginTop: 10 }}>
            <Busy label="Erasing. Rewriting log files can take a few minutes." />
          </p>
        )}
        {error && (
          <div className={styles.error} style={{ marginTop: 10 }}>
            {error}
          </div>
        )}

        {erased && <EraseResult erased={erased} />}

        {report && (
          <div className={local.result}>
            <div className={local.resultHead}>
              <div className={local.resultTitle}>
                <span className={styles.code}>{report.value}</span>{" "}
                {total === 0 ? "No records found." : `${records(total)} found.`}{" "}
                <span className={mail.checkedAt}>
                  {erased ? "Searched again after the erase, at" : "Searched at"} {clock(report.searched_at)}
                </span>
              </div>
              <div className={mail.row}>
                <button type="button" className={styles.btnGhost} onClick={download} disabled={busy !== null}>
                  Download as JSON
                </button>
                <button
                  type="button"
                  className={styles.btnDanger}
                  disabled={busy !== null || erasable.length === 0}
                  title={erasable.length === 0 ? "Nothing found that can be erased here" : undefined}
                  onClick={() => {
                    setPicked(erasable.map((s) => s.id));
                    setConfirming(true);
                  }}
                >
                  Erase...
                </button>
              </div>
            </div>

            {report.agent_error && (
              <div className={mail.warnBox} role="alert">
                Only the panel&apos;s own records were searched. The server&apos;s logs and mail could not be:{" "}
                {report.agent_error}
              </div>
            )}

            <ul className={local.sources}>
              {report.sources.map((s) => (
                <Source key={s.id} source={s} />
              ))}
            </ul>
            <p className={mail.sub} style={{ marginTop: 10 }}>
              The download holds this list: the counts, the notes and the samples (up to 20 records per source).
            </p>
          </div>
        )}
      </div>

      <ConfirmDialog
        open={confirming && report !== null}
        title="Erase this data?"
        confirmLabel="Erase"
        tone="danger"
        onCancel={() => setConfirming(false)}
        onConfirm={() => void erase()}
      >
        <p>
          About <span className={styles.code}>{report?.value}</span>. These will be changed:
        </p>
        <div className={local.pickList}>
          {erasable.map((s) => (
            <label key={s.id} className={local.pick}>
              <input
                type="checkbox"
                checked={picked.includes(s.id)}
                onChange={(e) =>
                  setPicked((p) => (e.target.checked ? [...p, s.id] : p.filter((id) => id !== s.id)))
                }
              />
              <span>
                <span className={local.strong}>{s.label}</span>, {records(s.count, s.truncated)}. {s.note}
              </span>
            </label>
          ))}
        </div>
        {picked.length === 0 && <p>Tick at least one to erase.</p>}
        {kept.length > 0 && (
          <>
            <p>These will not be changed:</p>
            <ul>
              {kept.map((s) => (
                <li key={s.id}>
                  <span className={local.strong}>{s.label}</span>, {records(s.count, s.truncated)}. {s.note}
                </li>
              ))}
            </ul>
          </>
        )}
        <p>An erase cannot be undone. Backups made earlier still hold the data until they expire.</p>
      </ConfirmDialog>
    </div>
  );
}

function Source({ source: s }: { source: SubjectSource }) {
  return (
    <li className={`${local.source} ${s.count === 0 ? local.sourceNothing : ""}`}>
      <div className={local.sourceHead}>
        <span className={local.sourceName}>{s.label}</span>
        {s.count === 0 ? (
          <StatusBadge text="nothing found" tone="muted" />
        ) : (
          <>
            <StatusBadge text={records(s.count, s.truncated)} tone="info" />
            <span className={mail.sub}>{s.erasable ? "can be erased here" : "cannot be erased here"}</span>
          </>
        )}
      </div>
      {s.note && <div className={local.sourceNote}>{s.note}</div>}
      {s.truncated && (
        <div className={local.sourceNote}>More records exist than are counted or shown here.</div>
      )}
      {s.samples.length > 0 && (
        <details className={local.samples}>
          <summary>
            Show samples ({s.samples.length}
            {s.count > s.samples.length || s.truncated ? `, newest of ${records(s.count, s.truncated)}` : ""})
          </summary>
          <pre className={local.sampleLines}>{s.samples.join("\n")}</pre>
        </details>
      )}
    </li>
  );
}

function EraseResult({ erased }: { erased: Erased }) {
  const failed = erased.items.some((r) => !r.ok);
  return (
    <div className={local.source} style={{ marginTop: 10 }} role="status">
      <div className={local.sourceHead}>
        <span className={local.sourceName}>Erase at {clock(erased.at)}</span>
        {failed ? <StatusBadge text="not everywhere" tone="warn" /> : <StatusBadge text="done" tone="ok" />}
      </div>
      <ul className={mail.notes}>
        {erased.items.map((r) => (
          <li key={r.id}>
            <span className={local.strong}>{erased.labels[r.id] ?? r.id}</span>: {r.ok ? "done." : "failed."}{" "}
            {r.detail || (r.ok ? `${records(r.changed)} changed.` : "")}
          </li>
        ))}
      </ul>
    </div>
  );
}
