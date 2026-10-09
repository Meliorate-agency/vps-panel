"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";
import TaskProgress from "@/components/TaskProgress/TaskProgress";
import shared from "@/components/page.module.css";
import strip from "./QuickInstallStrip.module.css";
import styles from "./TailscaleManager.module.css";
import type { TailscaleNode, TailscaleOptions, TailscaleStatus } from "@/lib/agent";
import {
  PHASE_LABEL,
  phaseTone,
  tailscaleCommand,
  tailscalePhase,
  type TailscaleCard,
  type TailscalePhase,
} from "@/lib/tailscale";
import { copyText } from "@/lib/clipboard";
import {
  getTailscaleStatusAction,
  saveTailscaleSettingsAction,
  tailscaleConnectAction,
  tailscaleDisconnectAction,
  tailscaleLoginAction,
  tailscaleLogoutAction,
  type TailscaleStepsResult,
} from "@/server-actions/tailscale";

const ADMIN_MACHINES = "https://login.tailscale.com/admin/machines";
const ADMIN_KEYS = "https://login.tailscale.com/admin/settings/keys";

/**
 * The Tailscale card's button: "Log in" until this server is on a tailnet,
 * "Manage" after. Opens the Tailscale dialog. `className` replaces the
 * card's button style (the setup wizard's Apps step uses its own).
 */
export default function TailscaleManager({ card, className }: { card: TailscaleCard; className?: string }) {
  const [open, setOpen] = useState(false);
  const login = card.phase === "needs_login" || card.phase === "waiting_login";
  return (
    <>
      <button
        type="button"
        className={className ?? `${strip.installBtn} ${styles.cardBtn}`}
        onClick={() => setOpen(true)}
      >
        {login ? "Log in" : "Manage"}
      </button>
      {/* Mounted only while open, so every open reads the state fresh. */}
      {open && <TailscaleDialog onClose={() => setOpen(false)} />}
    </>
  );
}

type Busy = null | "login" | "save" | "connect" | "disconnect" | "logout";
type Outcome = { ok: boolean; text: string; steps?: string[] };

/**
 * Everything about Tailscale on this server: state and addresses, log in
 * (a link someone opens, or an auth key), settings, connect / disconnect /
 * log out, the other machines, and a box for any other tailscale command.
 */
function TailscaleDialog({ onClose }: { onClose: () => void }) {
  const router = useRouter();
  const [status, setStatus] = useState<TailscaleStatus | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [busy, setBusy] = useState<Busy>(null);
  const [outcome, setOutcome] = useState<Outcome | null>(null);
  // The login task to follow: started here, or found running on open.
  const [loginTask, setLoginTask] = useState<string | null>(null);
  const [keyLogin, setKeyLogin] = useState(false);
  const [confirm, setConfirm] = useState<null | "disconnect" | "logout">(null);

  const refresh = useCallback(async () => {
    try {
      const r = await getTailscaleStatusAction();
      if (r.ok) {
        setStatus(r.data);
        setLoadError(null);
        if (r.data.login_task) setLoginTask(r.data.login_task);
      } else {
        setLoadError(r.error);
      }
    } catch (e) {
      setLoadError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const phase = tailscalePhase(status);
  // Poll while something happens without a click here: a login waiting for
  // its link, tailscaled starting, an admin approving the machine.
  const moving = phase === "waiting_login" || phase === "starting" || phase === "needs_approval";
  useEffect(() => {
    if (!moving) return;
    const t = window.setInterval(() => {
      if (document.visibilityState === "visible") void refresh();
    }, 3000);
    return () => window.clearInterval(t);
  }, [moving, refresh]);

  const act = async (kind: Exclude<Busy, null>, call: () => Promise<TailscaleStepsResult>, done: string) => {
    setBusy(kind);
    setOutcome(null);
    setConfirm(null);
    try {
      const r = await call();
      setOutcome(r.ok ? { ok: true, text: done, steps: r.data.steps } : { ok: false, text: r.error });
    } catch (e) {
      setOutcome({ ok: false, text: e instanceof Error ? e.message : String(e) });
    } finally {
      setBusy(null);
      await refresh();
    }
  };

  const startLogin = async (o: TailscaleOptions, authKey: string) => {
    setBusy("login");
    setOutcome(null);
    try {
      const r = await tailscaleLoginAction({ ...o, authKey });
      if (r.ok) {
        setLoginTask(r.data.taskId);
        setKeyLogin(authKey !== "");
      } else {
        setOutcome({ ok: false, text: r.error });
      }
    } catch (e) {
      setOutcome({ ok: false, text: e instanceof Error ? e.message : String(e) });
    } finally {
      setBusy(null);
      await refresh();
    }
  };

  const loginFinished = (s: "succeeded" | "failed") => {
    if (s === "succeeded") {
      setOutcome({ ok: true, text: "Logged in: this server is on your tailnet." });
      setLoginTask(null);
    }
    void refresh();
    router.refresh();
  };

  // "Starting" is a logged-in server connecting; NoState (also shown as
  // starting) is tailscaled not knowing yet, so it gets no controls.
  const loggedIn =
    phase === "running" ||
    phase === "stopped" ||
    (phase === "starting" && status?.backend_state === "Starting");

  return (
    <ConfirmDialog
      open
      wide
      title="Tailscale"
      hideActions
      pending={busy !== null}
      onConfirm={() => {}}
      onCancel={onClose}
    >
      {!status && !loadError && <p className={shared.muted}>Reading Tailscale&apos;s state...</p>}
      {loadError && <div className={shared.error}>{loadError}</div>}
      {status && (
        <>
          <Overview status={status} phase={phase} />
          {outcome && <OutcomeNote outcome={outcome} />}

          {phase === "daemon_down" && (
            <div className={styles.section}>
              <p className={styles.text}>
                tailscaled, the Tailscale service, is {status.daemon}. Start it to log in or reconnect.
              </p>
              <div className={styles.actions}>
                <button
                  type="button"
                  className={shared.btn}
                  disabled={busy !== null}
                  onClick={() => act("connect", tailscaleConnectAction, "Tailscale started.")}
                >
                  {busy === "connect" ? "Starting..." : "Start Tailscale"}
                </button>
              </div>
            </div>
          )}

          {phase === "needs_login" && (
            <LoginForm status={status} busy={busy} onLogin={startLogin} />
          )}

          {phase === "waiting_login" && (
            <LoginLink
              url={status.auth_url}
              withKey={keyLogin}
              busy={busy}
              onCancel={async () => {
                await act("logout", tailscaleLogoutAction, "Login cancelled.");
                setLoginTask(null);
              }}
            />
          )}

          {loginTask && (
            <div className={styles.section}>
              <TaskProgress key={loginTask} taskId={loginTask} onComplete={loginFinished} />
            </div>
          )}

          {phase === "needs_approval" && (
            <div className={styles.section}>
              <p className={styles.text}>
                Logged in. Your tailnet approves new machines by hand: an admin approves this server
                under{" "}
                <a href={ADMIN_MACHINES} target="_blank" rel="noreferrer noopener">
                  Machines in the Tailscale admin console
                </a>
                . This page notices by itself.
              </p>
            </div>
          )}

          {(loggedIn || phase === "needs_approval") && (
            <ConnectionActions
              phase={phase}
              busy={busy}
              confirm={confirm}
              setConfirm={setConfirm}
              onConnect={() => act("connect", tailscaleConnectAction, "Connected.")}
              onDisconnect={() =>
                act("disconnect", tailscaleDisconnectAction, "Disconnected. The server stays logged in.")
              }
              onLogout={() => act("logout", tailscaleLogoutAction, "Logged out.")}
            />
          )}

          {(loggedIn || phase === "needs_approval") && status.prefs && (
            // Keyed on the saved settings: after a save the form starts from them.
            <SettingsForm
              key={JSON.stringify(status.prefs)}
              status={status}
              busy={busy}
              onSave={(o) => act("save", () => saveTailscaleSettingsAction(o), "Settings saved.")}
            />
          )}

          {status.peers.length > 0 && <Machines peers={status.peers} />}
          <CommandBox />
        </>
      )}
    </ConfirmDialog>
  );
}

function Overview({ status, phase }: { status: TailscaleStatus; phase: TailscalePhase }) {
  const node = status.node;
  return (
    <div className={styles.overview}>
      <div className={styles.overviewHead}>
        <span className={`${styles.dot} ${styles[phaseTone(phase)]}`} />
        <strong>{PHASE_LABEL[phase]}</strong>
        {status.version && <span className={styles.version}>Tailscale {status.version}</span>}
      </div>
      {(node || status.user) && (
        <dl className={styles.facts}>
          {node && (
            <>
              <dt>This server</dt>
              <dd>
                <span className={shared.code}>{node.dns_name || node.name}</span>
              </dd>
            </>
          )}
          {node && node.ips.length > 0 && (
            <>
              <dt>Addresses</dt>
              <dd>
                {node.ips.map((ip) => (
                  <span key={ip} className={shared.code}>
                    {ip}
                  </span>
                ))}
              </dd>
            </>
          )}
          {status.user && (
            <>
              <dt>Account</dt>
              <dd>{status.user}</dd>
            </>
          )}
          {status.tailnet && status.tailnet !== status.user && (
            <>
              <dt>Tailnet</dt>
              <dd>{status.tailnet}</dd>
            </>
          )}
        </dl>
      )}
      {status.error && <p className={styles.warnText}>{status.error}</p>}
      {status.health.length > 0 && (
        <ul className={styles.health}>
          {status.health.map((h) => (
            <li key={h}>{h}</li>
          ))}
        </ul>
      )}
    </div>
  );
}

function OutcomeNote({ outcome }: { outcome: Outcome }) {
  return (
    <div className={outcome.ok ? shared.notice : shared.error}>
      <div className={styles.outcomeText}>{outcome.text}</div>
      {outcome.steps && outcome.steps.length > 0 && (
        <pre className={styles.steps}>{outcome.steps.join("\n")}</pre>
      )}
    </div>
  );
}

function LoginForm({
  status,
  busy,
  onLogin,
}: {
  status: TailscaleStatus;
  busy: Busy;
  onLogin: (o: TailscaleOptions, authKey: string) => void;
}) {
  const [method, setMethod] = useState<"link" | "key">("link");
  const [authKey, setAuthKey] = useState("");
  // A login sets everything (tailscale up --reset), so the form starts from
  // the server-friendly defaults rather than whatever tailscaled holds.
  const [opts, setOpts] = useState<TailscaleOptions>({
    hostname: status.prefs?.hostname ?? "",
    ssh: false,
    accept_routes: false,
    accept_dns: false,
    exit_node: false,
  });
  const keyMissing = method === "key" && authKey.trim() === "";

  return (
    <form
      className={styles.section}
      onSubmit={(e) => {
        e.preventDefault();
        if (!keyMissing) onLogin(opts, method === "key" ? authKey.trim() : "");
      }}
    >
      <h4 className={styles.sectionTitle}>Log in</h4>
      <p className={styles.hint}>
        Adds this server to your tailnet. You need a Tailscale account; it is free for personal use.
      </p>
      <div className={styles.methods} role="radiogroup" aria-label="How to log in">
        <label className={styles.method}>
          <input
            type="radio"
            name="ts-method"
            checked={method === "link"}
            onChange={() => setMethod("link")}
          />
          <span>
            <strong>With a login link</strong>
            <span className={styles.hint}>You get a link here: open it and sign in.</span>
          </span>
        </label>
        <label className={styles.method}>
          <input
            type="radio"
            name="ts-method"
            checked={method === "key"}
            onChange={() => setMethod("key")}
          />
          <span>
            <strong>With an auth key</strong>
            <span className={styles.hint}>A key made in the Tailscale admin console.</span>
          </span>
        </label>
      </div>
      {method === "key" && (
        <label className={shared.field}>
          Auth key
          <input
            className={shared.input}
            type="password"
            value={authKey}
            onChange={(e) => setAuthKey(e.target.value)}
            placeholder="tskey-auth-..."
            autoComplete="off"
            spellCheck={false}
            data-1p-ignore
            data-lpignore="true"
          />
          <span className={styles.hint}>
            Make one at{" "}
            <a href={ADMIN_KEYS} target="_blank" rel="noreferrer noopener">
              Settings &gt; Keys
            </a>{" "}
            in the admin console. The panel hands it to Tailscale in a file only root can read, and
            never writes it to a log.
          </span>
        </label>
      )}
      <OptionFields value={opts} onChange={setOpts} serverHostname={status.server_hostname} />
      <div className={styles.actions}>
        <button type="submit" className={shared.btn} disabled={busy !== null || keyMissing}>
          {busy === "login" ? "Starting..." : "Log in"}
        </button>
      </div>
    </form>
  );
}

function LoginLink({
  url,
  withKey,
  busy,
  onCancel,
}: {
  url: string | null;
  withKey: boolean;
  busy: Busy;
  onCancel: () => void;
}) {
  const [copied, setCopied] = useState(false);
  return (
    <div className={`${styles.section} ${styles.linkBox}`}>
      {url ? (
        <>
          <h4 className={styles.sectionTitle}>Open this link and sign in</h4>
          <a href={url} target="_blank" rel="noreferrer noopener" className={styles.link}>
            {url}
          </a>
          <div className={styles.actions}>
            <a
              href={url}
              target="_blank"
              rel="noreferrer noopener"
              className={`${shared.btn} ${styles.linkBtn}`}
            >
              Open login page
            </a>
            <button
              type="button"
              className={shared.btnGhost}
              onClick={async () => setCopied(await copyText(url))}
            >
              {copied ? "Copied" : "Copy link"}
            </button>
            <button type="button" className={shared.btnGhost} onClick={onCancel} disabled={busy !== null}>
              {busy === "logout" ? "Cancelling..." : "Cancel"}
            </button>
          </div>
          <p className={styles.hint}>
            Waiting for the sign-in, up to 15 minutes; this updates by itself. Anyone who opens the link
            can add this server to their tailnet, so keep it to yourself.
          </p>
        </>
      ) : (
        <p className={styles.text}>
          {withKey ? "Logging in with the auth key..." : "Getting a login link from Tailscale..."}
        </p>
      )}
    </div>
  );
}

function ConnectionActions({
  phase,
  busy,
  confirm,
  setConfirm,
  onConnect,
  onDisconnect,
  onLogout,
}: {
  phase: TailscalePhase;
  busy: Busy;
  confirm: null | "disconnect" | "logout";
  setConfirm: (c: null | "disconnect" | "logout") => void;
  onConnect: () => void;
  onDisconnect: () => void;
  onLogout: () => void;
}) {
  if (confirm) {
    return (
      <div className={styles.confirm}>
        <p className={styles.text}>
          {confirm === "logout"
            ? "Log this server out of Tailscale? It leaves the tailnet until someone logs it in again."
            : "Disconnect this server from the tailnet? It stays logged in, and Connect brings it back."}{" "}
          If you reach this panel through Tailscale, you lose that connection.
        </p>
        <div className={styles.actions}>
          <button
            type="button"
            className={shared.btnDanger}
            disabled={busy !== null}
            onClick={confirm === "logout" ? onLogout : onDisconnect}
          >
            {confirm === "logout" ? "Log out" : "Disconnect"}
          </button>
          <button type="button" className={shared.btnGhost} onClick={() => setConfirm(null)}>
            Keep it
          </button>
        </div>
      </div>
    );
  }
  return (
    <div className={styles.actions}>
      {phase === "stopped" ? (
        <button type="button" className={shared.btn} disabled={busy !== null} onClick={onConnect}>
          {busy === "connect" ? "Connecting..." : "Connect"}
        </button>
      ) : phase !== "needs_approval" ? (
        <button
          type="button"
          className={shared.btnGhost}
          disabled={busy !== null}
          onClick={() => setConfirm("disconnect")}
        >
          {busy === "disconnect" ? "Disconnecting..." : "Disconnect"}
        </button>
      ) : null}
      <button
        type="button"
        className={shared.btnGhost}
        disabled={busy !== null}
        onClick={() => setConfirm("logout")}
      >
        {busy === "logout" ? "Logging out..." : "Log out"}
      </button>
    </div>
  );
}

function SettingsForm({
  status,
  busy,
  onSave,
}: {
  status: TailscaleStatus;
  busy: Busy;
  onSave: (o: TailscaleOptions) => void;
}) {
  const p = status.prefs;
  const initial: TailscaleOptions = {
    hostname: p?.hostname ?? "",
    ssh: !!p?.ssh,
    accept_routes: !!p?.accept_routes,
    accept_dns: !!p?.accept_dns,
    exit_node: !!p?.exit_node,
  };
  const [opts, setOpts] = useState<TailscaleOptions>(initial);
  const dirty =
    opts.hostname.trim() !== initial.hostname ||
    opts.ssh !== initial.ssh ||
    opts.accept_routes !== initial.accept_routes ||
    opts.accept_dns !== initial.accept_dns ||
    opts.exit_node !== initial.exit_node;

  return (
    <form
      className={styles.section}
      onSubmit={(e) => {
        e.preventDefault();
        onSave({ ...opts, hostname: opts.hostname.trim() });
      }}
    >
      <h4 className={styles.sectionTitle}>Settings</h4>
      <OptionFields value={opts} onChange={setOpts} serverHostname={status.server_hostname} />
      {p && p.advertised_routes.length > 0 && (
        <p className={styles.hint}>
          Also shares {p.advertised_routes.join(", ")} (set with a command). Saving keeps it.
        </p>
      )}
      {p?.exit_node && (
        <p className={styles.hint}>
          Offered as an exit node. It works once approved in the{" "}
          <a href={ADMIN_MACHINES} target="_blank" rel="noreferrer noopener">
            admin console
          </a>{" "}
          (this server &gt; Edit route settings).
        </p>
      )}
      <div className={styles.actions}>
        <button type="submit" className={shared.btn} disabled={!dirty || busy !== null}>
          {busy === "save" ? "Saving..." : "Save"}
        </button>
      </div>
    </form>
  );
}

function OptionFields({
  value,
  onChange,
  serverHostname,
}: {
  value: TailscaleOptions;
  onChange: (o: TailscaleOptions) => void;
  serverHostname: string;
}) {
  const set = (patch: Partial<TailscaleOptions>) => onChange({ ...value, ...patch });
  return (
    <>
      <label className={shared.field}>
        Name on the tailnet
        <input
          className={shared.input}
          value={value.hostname}
          onChange={(e) => set({ hostname: e.target.value })}
          placeholder={serverHostname || "the server's hostname"}
          maxLength={63}
          autoComplete="off"
          spellCheck={false}
        />
        <span className={styles.hint}>
          Empty uses the server&apos;s hostname{serverHostname ? ` (${serverHostname})` : ""}. Letters,
          digits and hyphens.
        </span>
      </label>
      <div className={styles.checks}>
        <Check
          checked={value.ssh}
          onChange={(v) => set({ ssh: v })}
          label="Tailscale SSH"
          hint="Tailnet members can SSH in as your tailnet's access rules allow, without SSH keys. The normal SSH server stays as it is."
        />
        <Check
          checked={value.exit_node}
          onChange={(v) => set({ exit_node: v })}
          label="Exit node"
          hint="Your devices can send their internet traffic through this server. Turns on IP forwarding; approve it in the admin console."
        />
        <Check
          checked={value.accept_routes}
          onChange={(v) => set({ accept_routes: v })}
          label="Use shared networks"
          hint="Reach private networks (subnet routes) that other machines share on the tailnet."
        />
        <Check
          checked={value.accept_dns}
          onChange={(v) => set({ accept_dns: v })}
          label="Use tailnet DNS"
          hint="Look up tailnet names (MagicDNS). Off keeps this server's own DNS settings, which its mail and DNS services rely on."
        />
      </div>
    </>
  );
}

function Check({
  checked,
  onChange,
  label,
  hint,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label: string;
  hint: string;
}) {
  return (
    <label className={styles.check}>
      <input type="checkbox" checked={checked} onChange={(e) => onChange(e.target.checked)} />
      <span>
        <strong>{label}</strong>
        <span className={styles.hint}>{hint}</span>
      </span>
    </label>
  );
}

function Machines({ peers }: { peers: TailscaleNode[] }) {
  return (
    <details className={styles.section} open={peers.length <= 8}>
      <summary className={styles.summary}>Other machines on the tailnet ({peers.length})</summary>
      <div className={styles.tableWrap}>
        <table className={styles.table}>
          <thead>
            <tr>
              <th>Name</th>
              <th>Address</th>
              <th>OS</th>
              <th>Status</th>
            </tr>
          </thead>
          <tbody>
            {peers.map((p) => (
              <tr key={p.dns_name || p.name}>
                <td title={p.dns_name}>
                  {p.name}
                  {p.exit_node ? (
                    <span className={styles.tag}>exit node in use</span>
                  ) : p.exit_node_option ? (
                    <span className={styles.tag}>exit node</span>
                  ) : null}
                </td>
                <td>
                  <span className={shared.code}>{p.ips.find((a) => !a.includes(":")) ?? p.ips[0] ?? "-"}</span>
                </td>
                <td>{p.os || "-"}</td>
                <td>
                  {p.online ? (
                    <span className={styles.online}>online</span>
                  ) : (
                    <span className={shared.muted}>{p.last_seen ? seen(p.last_seen) : "offline"}</span>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </details>
  );
}

function seen(iso: string): string {
  const secs = (Date.now() - new Date(iso).getTime()) / 1000;
  if (!Number.isFinite(secs) || secs < 0) return "offline";
  if (secs < 3600) return `seen ${Math.max(1, Math.round(secs / 60))} min ago`;
  if (secs < 86400) return `seen ${Math.round(secs / 3600)} h ago`;
  const days = Math.round(secs / 86400);
  return `seen ${days} day${days === 1 ? "" : "s"} ago`;
}

type Frame =
  | { t: "out"; d: string }
  | { t: "err"; d: string }
  | { t: "notice"; d: string }
  | { t: "exit"; code: number };

const EXAMPLES = ["status", "netcheck", "ip -4", "serve status", "version"];

/**
 * A one-line box for any other tailscale command (advertise routes, serve,
 * ping a machine...). It goes through the Terminal's endpoint (admin only,
 * audited, streamed, Stop kills it), but always as `tailscale` with the typed
 * words as plain arguments: see lib/tailscale.ts::tailscaleCommand.
 */
function CommandBox() {
  const [input, setInput] = useState("");
  const [ran, setRan] = useState<string | null>(null);
  const [lines, setLines] = useState<{ text: string; err: boolean }[]>([]);
  const [exitCode, setExitCode] = useState<number | null>(null);
  const [running, setRunning] = useState(false);
  const abortRef = useRef<AbortController | null>(null);
  const outRef = useRef<HTMLPreElement>(null);

  useEffect(() => {
    outRef.current?.scrollTo(0, outRef.current.scrollHeight);
  }, [lines]);
  // Closing the dialog stops a command still running.
  useEffect(() => () => abortRef.current?.abort(), []);

  const run = async () => {
    if (running) return;
    const built = tailscaleCommand(input);
    setExitCode(null);
    if ("error" in built) {
      setRan(null);
      setLines([{ text: built.error, err: true }]);
      return;
    }
    setRan(`tailscale ${input.trim().replace(/^tailscale\s+/, "")}`);
    setLines([]);
    setRunning(true);
    const controller = new AbortController();
    abortRef.current = controller;
    const add = (text: string, err: boolean) => setLines((prev) => [...prev, { text, err }]);
    try {
      const res = await fetch("/api/terminal/exec", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ cwd: "/root", command: built.command, timeout_secs: 300 }),
        signal: controller.signal,
      });
      if (!res.ok || !res.body) {
        add((await res.text().catch(() => "")) || `request failed (${res.status})`, true);
        return;
      }
      // NDJSON, one frame per line; a chunk can end mid-frame.
      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      let buffer = "";
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });
        const parts = buffer.split("\n");
        buffer = parts.pop() ?? "";
        for (const part of parts) {
          if (!part.trim()) continue;
          let frame: Frame;
          try {
            frame = JSON.parse(part) as Frame;
          } catch {
            continue;
          }
          if (frame.t === "exit") setExitCode(frame.code);
          else add(frame.d, frame.t === "err");
        }
      }
    } catch (e) {
      if (e instanceof DOMException && e.name === "AbortError") add("stopped", true);
      else add(e instanceof Error ? e.message : String(e), true);
    } finally {
      abortRef.current = null;
      setRunning(false);
    }
  };

  return (
    <details className={styles.section}>
      <summary className={styles.summary}>Run a tailscale command</summary>
      <form
        className={styles.cmdRow}
        onSubmit={(e) => {
          e.preventDefault();
          void run();
        }}
      >
        <span className={styles.prompt}>tailscale</span>
        <input
          className={`${shared.input} ${styles.cmdInput}`}
          value={input}
          onChange={(e) => setInput(e.target.value)}
          placeholder="status"
          autoComplete="off"
          spellCheck={false}
          aria-label="tailscale command"
        />
        {running ? (
          <button type="button" className={shared.btnGhost} onClick={() => abortRef.current?.abort()}>
            Stop
          </button>
        ) : (
          <button type="submit" className={shared.btn} disabled={!input.trim()}>
            Run
          </button>
        )}
      </form>
      <div className={styles.examples}>
        {EXAMPLES.map((ex) => (
          <button key={ex} type="button" className={styles.chip} onClick={() => setInput(ex)}>
            {ex}
          </button>
        ))}
      </div>
      {(ran || lines.length > 0) && (
        <pre ref={outRef} className={styles.output}>
          {ran && (
            <span className={styles.echo}>
              $ {ran}
              {"\n"}
            </span>
          )}
          {lines.map((l, i) => (
            <span key={i} className={l.err ? styles.errLine : undefined}>
              <Linkified text={l.text} />
              {"\n"}
            </span>
          ))}
          {running && <span className={styles.echo}>(running...)</span>}
          {exitCode !== null && exitCode !== 0 && (
            <span className={styles.errLine}>exit code {exitCode}</span>
          )}
        </pre>
      )}
      <p className={styles.hint}>
        Runs as root on this server and is saved in the audit log. Only tailscale runs here: no
        pipes or other commands. For those, use <Link href="/terminal">Terminal</Link>.
      </p>
    </details>
  );
}

/** Output text with its https links clickable (`tailscale up` and `serve`
 *  print the URL to open). */
function Linkified({ text }: { text: string }) {
  const parts = text.split(/(https:\/\/[^\s"'<>]+)/g);
  return (
    <>
      {parts.map((p, i) =>
        p.startsWith("https://") ? (
          <a key={i} href={p} target="_blank" rel="noreferrer noopener">
            {p}
          </a>
        ) : (
          p
        ),
      )}
    </>
  );
}
