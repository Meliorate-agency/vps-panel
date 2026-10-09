"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import type { HistoryRow } from "@/lib/terminalHistory";
import pageStyles from "@/components/page.module.css";
import styles from "./terminal.module.css";
import FsTree from "./FsTree";
import {
  clearHistoryAction,
  deleteHistoryAction,
  historyAction,
  recordCommandAction,
} from "@/server-actions/terminal";

/** One finished or in-flight command in the scrollback. */
type Block = {
  id: number;
  cwd: string;
  command: string;
  /** Recorded per block so scrollback stays truthful after the picker changes. */
  runAs: string;
  lines: { text: string; stream: "out" | "err" | "notice" }[];
  exitCode: number | null;
  running: boolean;
};

type Frame =
  | { t: "out"; d: string }
  | { t: "err"; d: string }
  | { t: "notice"; d: string }
  | { t: "exit"; code: number };

/**
 * The app a path belongs to, if any. `/srv/apps/myapp/db` -> "myapp".
 *
 * Derived from the path rather than looked up, because it only needs to be
 * right often enough to offer the env toggle — the agent skips env files that
 * turn out not to exist, so a wrong guess costs nothing.
 */
function appOf(cwd: string): string | null {
  const m = /^\/srv\/apps\/([^/]+)/.exec(cwd);
  return m ? m[1] : null;
}

/**
 * The env files to source for `app`, in load order.
 *
 * Both halves matter and neither is reachable from a plain `bash -lc`: the
 * systemd EnvironmentFile is what the running service sees, and the project
 * `.env*` files are what the app's own framework loads.
 */
function envFilesFor(app: string): string[] {
  return [
    `/etc/vps-panel/apps/${app}.env`,
    `/srv/apps/${app}/.env`,
    `/srv/apps/${app}/.env.local`,
  ];
}

/** Resolve a `cd` argument against the current directory, without a server
 *  round trip. `..`, `.`, `~`, absolute and relative forms all land where a
 *  shell would put them. */
function resolveCd(cwd: string, arg: string): string {
  const target = arg.trim() || "/root";
  const base = target.startsWith("/")
    ? []
    : cwd.split("/").filter(Boolean);
  const start = target === "~" || target.startsWith("~/")
    ? ["root"]
    : base;
  const rest = (target === "~" ? "" : target.replace(/^~\//, "")).split("/").filter(Boolean);
  const out = [...start];
  for (const seg of rest) {
    if (seg === ".") continue;
    if (seg === "..") out.pop();
    else out.push(seg);
  }
  return `/${out.join("/")}`.replace(/\/+/g, "/").replace(/(.)\/$/, "$1");
}

export default function TerminalConsole({
  initialPath,
  initialHistory,
  hostname,
  users,
}: {
  initialPath: string;
  initialHistory: HistoryRow[];
  hostname: string;
  /** Linux accounts offered in the "Run as" picker. */
  users: string[];
}) {
  const [cwd, setCwd] = useState(initialPath);
  const [runAs, setRunAs] = useState("root");
  const [loadEnv, setLoadEnv] = useState(true);
  const [timeoutSecs, setTimeoutSecs] = useState(120);
  const [history, setHistory] = useState<HistoryRow[]>(initialHistory);
  const [blocks, setBlocks] = useState<Block[]>([]);
  const [input, setInput] = useState("");
  const [running, setRunning] = useState(false);
  // Where Up/Down are in this session's own command list (separate from the
  // saved per-folder history on the right).
  const [recallIndex, setRecallIndex] = useState<number | null>(null);

  const currentApp = appOf(cwd);
  const abortRef = useRef<AbortController | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const blockId = useRef(0);
  const sessionCommands = useRef<string[]>([]);

  // The saved history pane is per folder — swapping folders swaps the list.
  useEffect(() => {
    let cancelled = false;
    historyAction(cwd)
      .then((rows) => {
        if (!cancelled) setHistory(rows);
      })
      .catch(() => {
        if (!cancelled) setHistory([]);
      });
    return () => {
      cancelled = true;
    };
  }, [cwd]);

  // Keep the newest output in view, but only when already near the bottom, so
  // scrolling up to read something isn't yanked away by a chatty command.
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 120;
    if (nearBottom) el.scrollTop = el.scrollHeight;
  }, [blocks]);

  const appendLine = useCallback(
    (id: number, text: string, stream: "out" | "err" | "notice") => {
      setBlocks((prev) =>
        prev.map((b) => (b.id === id ? { ...b, lines: [...b.lines, { text, stream }] } : b)),
      );
    },
    [],
  );

  const run = useCallback(
    async (command: string) => {
      const cmd = command.trim();
      if (!cmd || running) return;

      // `cd` can't work as a real command — every run is its own `bash -lc`,
      // so the directory would snap back immediately. Treat it as a navigation
      // instruction for the tree instead, which is what the operator means.
      if (cmd === "cd" || cmd.startsWith("cd ")) {
        const target = resolveCd(cwd, cmd.slice(2));
        setCwd(target);
        setInput("");
        setRecallIndex(null);
        sessionCommands.current.unshift(cmd);
        return;
      }
      if (cmd === "clear") {
        setBlocks([]);
        setInput("");
        setRecallIndex(null);
        return;
      }

      const id = ++blockId.current;
      const runCwd = cwd;
      const app = appOf(runCwd);
      const envFiles = loadEnv && app ? envFilesFor(app) : [];
      setBlocks((prev) => [
        ...prev,
        {
          id,
          cwd: runCwd,
          command: cmd,
          runAs,
          lines: [],
          exitCode: null,
          running: true,
        },
      ]);
      setInput("");
      setRecallIndex(null);
      setRunning(true);
      sessionCommands.current.unshift(cmd);

      // Recorded up front so a command that fails or gets stopped is still
      // recallable — usually the one you most want back.
      recordCommandAction(runCwd, cmd)
        .then(setHistory)
        .catch(() => {
          /* history is a convenience; never fail the run over it */
        });

      const controller = new AbortController();
      abortRef.current = controller;

      try {
        const res = await fetch("/api/terminal/exec", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            cwd: runCwd,
            command: cmd,
            run_as: runAs,
            env_files: envFiles,
            timeout_secs: timeoutSecs,
          }),
          signal: controller.signal,
        });

        if (!res.ok || !res.body) {
          const text = await res.text().catch(() => "");
          let message = text || `request failed (${res.status})`;
          try {
            message = (JSON.parse(text) as { error?: string }).error ?? message;
          } catch {
            // non-JSON error body — show it raw
          }
          appendLine(id, message, "err");
          setBlocks((prev) =>
            prev.map((b) => (b.id === id ? { ...b, running: false, exitCode: -1 } : b)),
          );
          return;
        }

        // NDJSON: one frame per line. Chunks split anywhere, so hold the
        // partial tail until its newline arrives.
        const reader = res.body.getReader();
        const decoder = new TextDecoder();
        let buffer = "";
        for (;;) {
          const { done, value } = await reader.read();
          if (done) break;
          buffer += decoder.decode(value, { stream: true });
          const lines = buffer.split("\n");
          buffer = lines.pop() ?? "";
          for (const line of lines) {
            if (!line.trim()) continue;
            let frame: Frame;
            try {
              frame = JSON.parse(line) as Frame;
            } catch {
              continue;
            }
            if (frame.t === "exit") {
              setBlocks((prev) =>
                prev.map((b) =>
                  b.id === id ? { ...b, running: false, exitCode: frame.code } : b,
                ),
              );
            } else {
              appendLine(id, frame.d, frame.t);
            }
          }
        }
      } catch (e: unknown) {
        if (e instanceof DOMException && e.name === "AbortError") {
          appendLine(id, "stopped", "notice");
        } else {
          appendLine(id, e instanceof Error ? e.message : String(e), "err");
        }
      } finally {
        abortRef.current = null;
        setRunning(false);
        setBlocks((prev) => prev.map((b) => (b.id === id ? { ...b, running: false } : b)));
        inputRef.current?.focus();
      }
    },
    // runAs, loadEnv and timeoutSecs are read inside: without them here the
    // callback kept the first render's values, so switching "run as" to
    // `apps` still ran commands as root (and left root-owned build files).
    [cwd, running, appendLine, runAs, loadEnv, timeoutSecs],
  );

  /**
   * Up/Down through this session's commands, newest first, like a shell.
   * Computed from the current index rather than inside a setState updater —
   * an updater must be pure, and React may run it twice in development.
   */
  const recall = (direction: 1 | -1) => {
    const list = sessionCommands.current;
    if (list.length === 0) return;
    const next =
      recallIndex === null
        ? direction === 1
          ? 0
          : -1
        : Math.min(list.length - 1, recallIndex + direction);
    if (next < 0) {
      setRecallIndex(null);
      setInput("");
      return;
    }
    setRecallIndex(next);
    setInput(list[next]);
  };

  return (
    <div className={styles.layout}>
      <aside className={styles.paneLeft}>
        <div className={styles.paneTitle}>Filesystem</div>
        <FsTree selected={cwd} onSelect={setCwd} />
      </aside>

      <section className={styles.paneCenter}>
        <div className={styles.cwdBar}>
          <span className={styles.cwdLabel}>Running in</span>
          <span className={styles.cwdPath} title={cwd}>
            {cwd}
          </span>
        </div>

        <div className={styles.optionsBar}>
          <label className={styles.option}>
            <span className={styles.optionLabel}>as</span>
            <select
              className={styles.optionSelect}
              value={runAs}
              onChange={(e) => setRunAs(e.target.value)}
            >
              {users.map((u) => (
                <option key={u} value={u}>
                  {u}
                </option>
              ))}
            </select>
          </label>

          {currentApp ? (
            <label className={styles.option} title={envFilesFor(currentApp).join("\n")}>
              <input
                type="checkbox"
                checked={loadEnv}
                onChange={(e) => setLoadEnv(e.target.checked)}
              />
              <span className={styles.optionLabel}>
                load <strong>{currentApp}</strong> env
              </span>
            </label>
          ) : (
            <span className={styles.optionHint}>
              no app env here — open a folder under /srv/apps
            </span>
          )}

          <label className={styles.option}>
            <span className={styles.optionLabel}>timeout</span>
            <input
              type="number"
              className={styles.optionNumber}
              min={5}
              max={1800}
              step={5}
              value={timeoutSecs}
              onChange={(e) => setTimeoutSecs(Number(e.target.value) || 120)}
            />
            <span className={styles.optionLabel}>s</span>
          </label>

          {runAs === "root" && currentApp && (
            <span className={styles.optionWarn}>
              root writes here will be root-owned — the app runs as{" "}
              <code>apps</code>
            </span>
          )}
        </div>

        <div className={styles.console} ref={scrollRef}>
          {blocks.length === 0 && (
            <div className={styles.consoleHint}>
              Commands run one at a time, in the folder shown above. Each run is its own
              shell — <code>cd</code> moves the tree instead of the shell, and interactive
              programs (<code>vim</code>, <code>top</code>) won&apos;t work here.
              <br />
              For an app&apos;s own tooling (<code>npm run db:migrate</code>,{" "}
              <code>npm run build</code>) set <strong>as</strong> to <code>apps</code> and
              leave <strong>load env</strong> ticked — that matches how the panel deploys,
              so nothing ends up root-owned and <code>DATABASE_URL</code> is actually set.
            </div>
          )}
          {blocks.map((b) => (
            <div key={b.id} className={styles.block}>
              <div className={styles.promptLine}>
                <span className={styles.promptUser}>
                  {b.runAs}@{hostname}
                </span>
                <span className={styles.promptPath}>:{b.cwd}</span>
                <span className={styles.promptSign}>{b.runAs === "root" ? "# " : "$ "}</span>
                <span className={styles.promptCmd}>{b.command}</span>
              </div>
              {b.lines.map((l, i) => (
                <div
                  key={i}
                  className={[
                    styles.outLine,
                    l.stream === "err" ? styles.outErr : "",
                    l.stream === "notice" ? styles.outNotice : "",
                  ]
                    .join(" ")
                    .trim()}
                >
                  {l.text || " "}
                </div>
              ))}
              {b.running && <div className={styles.outNotice}>running…</div>}
              {!b.running && b.exitCode !== null && b.exitCode !== 0 && (
                <div className={styles.exitBad}>exit {b.exitCode}</div>
              )}
            </div>
          ))}
        </div>

        <form
          className={styles.inputRow}
          onSubmit={(e) => {
            e.preventDefault();
            void run(input);
          }}
        >
          <span className={styles.inputPrompt}>{runAs === "root" ? "#" : "$"}</span>
          <input
            ref={inputRef}
            className={styles.input}
            value={input}
            placeholder={running ? "running…" : `command to run in ${cwd}`}
            spellCheck={false}
            autoComplete="off"
            autoCapitalize="off"
            disabled={running}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "ArrowUp") {
                e.preventDefault();
                recall(1);
              } else if (e.key === "ArrowDown") {
                e.preventDefault();
                recall(-1);
              }
            }}
          />
          {running ? (
            <button
              type="button"
              className={pageStyles.btnDanger}
              onClick={() => abortRef.current?.abort()}
            >
              Stop
            </button>
          ) : (
            <button type="submit" className={pageStyles.btn} disabled={!input.trim()}>
              Run
            </button>
          )}
          <button
            type="button"
            className={pageStyles.btnGhost}
            onClick={() => setBlocks([])}
            disabled={blocks.length === 0}
          >
            Clear
          </button>
        </form>
      </section>

      <aside className={styles.paneRight}>
        <div className={styles.paneTitle}>
          Recent here
          {history.length > 0 && (
            <button
              type="button"
              className={styles.paneAction}
              onClick={() => void clearHistoryAction(cwd).then(setHistory)}
            >
              clear
            </button>
          )}
        </div>
        <div className={styles.paneNote}>
          Commands you&apos;ve run in <span className={styles.cwdPathSmall}>{cwd}</span>.
          Each folder keeps its own list.
        </div>
        {history.length === 0 ? (
          <div className={styles.historyEmpty}>Nothing run in this folder yet.</div>
        ) : (
          <ul className={styles.historyList}>
            {history.map((h) => (
              <li key={h.id} className={styles.historyItem}>
                <button
                  type="button"
                  className={styles.historyCmd}
                  title={`Used ${h.uses}× — click to load, double-click to run`}
                  onClick={() => {
                    setInput(h.command);
                    inputRef.current?.focus();
                  }}
                  onDoubleClick={() => void run(h.command)}
                >
                  {h.command}
                </button>
                <button
                  type="button"
                  className={styles.historyDrop}
                  aria-label={`Forget ${h.command}`}
                  title="Forget"
                  onClick={() => void deleteHistoryAction(h.id, cwd).then(setHistory)}
                >
                  ×
                </button>
              </li>
            ))}
          </ul>
        )}
      </aside>
    </div>
  );
}
