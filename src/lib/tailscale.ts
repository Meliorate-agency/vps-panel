import type { TailscaleStatus } from "./agent";

/** Where this server stands with Tailscale, from the agent's status. */
export type TailscalePhase =
  | "unknown"
  | "daemon_down"
  | "starting"
  | "needs_login"
  | "waiting_login"
  | "needs_approval"
  | "running"
  | "stopped";

export function tailscalePhase(s: TailscaleStatus | null): TailscalePhase {
  if (!s || !s.installed) return "unknown";
  // tailscaled answering is what counts (a hand install can run it under
  // another unit); the unit's state only explains a silent one.
  if (!s.backend_state) return s.daemon === "active" ? "unknown" : "daemon_down";
  if (s.login_task) return "waiting_login";
  switch (s.backend_state) {
    case "Running":
      return "running";
    case "Starting":
    case "NoState":
      return "starting";
    case "Stopped":
      return "stopped";
    case "NeedsMachineAuth":
      return "needs_approval";
    case "NeedsLogin":
      return "needs_login";
    default:
      return "unknown";
  }
}

export const PHASE_LABEL: Record<TailscalePhase, string> = {
  unknown: "State unknown",
  daemon_down: "Service stopped",
  starting: "Starting",
  needs_login: "Not logged in",
  waiting_login: "Waiting for login",
  needs_approval: "Waiting for approval",
  running: "Connected",
  stopped: "Disconnected",
};

export function phaseTone(p: TailscalePhase): "ok" | "warn" | "muted" {
  if (p === "running") return "ok";
  if (p === "starting" || p === "stopped" || p === "unknown") return "muted";
  return "warn";
}

/** What the Tailscale card on /apps shows. Built on the server from the
 *  status, so the card never carries the login link or the machine list. */
export type TailscaleCard = {
  phase: TailscalePhase;
  /** This server's tailnet IPv4 while connected. */
  ip: string | null;
};

export function tailscaleCard(s: TailscaleStatus | null): TailscaleCard {
  const phase = tailscalePhase(s);
  const ip =
    phase === "running" ? (s?.node?.ips.find((a) => !a.includes(":")) ?? s?.node?.ips[0] ?? null) : null;
  return { phase, ip };
}

/**
 * Splits what was typed after `tailscale` into arguments, the way a shell
 * does for words and quotes ('...', "...", backslash). Returns null when a
 * quote is left open.
 */
export function splitArgs(input: string): string[] | null {
  const out: string[] = [];
  let cur = "";
  let inWord = false;
  let quote: "'" | '"' | null = null;
  for (let i = 0; i < input.length; i++) {
    const c = input[i];
    if (quote === "'") {
      if (c === "'") quote = null;
      else cur += c;
      continue;
    }
    if (quote === '"') {
      if (c === '"') quote = null;
      else if (c === "\\" && (input[i + 1] === '"' || input[i + 1] === "\\")) cur += input[++i];
      else cur += c;
      continue;
    }
    if (c === "'" || c === '"') {
      quote = c;
      inWord = true;
    } else if (c === "\\" && i + 1 < input.length) {
      cur += input[++i];
      inWord = true;
    } else if (/\s/.test(c)) {
      if (inWord) out.push(cur);
      cur = "";
      inWord = false;
    } else {
      cur += c;
      inWord = true;
    }
  }
  if (quote) return null;
  if (inWord) out.push(cur);
  return out;
}

/** One argument in single quotes, so bash hands it over untouched. */
export function shellQuote(arg: string): string {
  return `'${arg.replace(/'/g, `'\\''`)}'`;
}

/**
 * The command line /api/terminal/exec runs for the command box in the
 * Tailscale dialog. It is always `tailscale` followed by the typed arguments
 * as literal words, so the box runs tailscale and nothing else: `|`, `;` or
 * `$(...)` reach tailscale as plain text. A leading "tailscale" typed out of
 * habit is dropped.
 */
export function tailscaleCommand(input: string): { command: string } | { error: string } {
  const args = splitArgs(input.trim());
  if (args === null) return { error: "A quote is not closed." };
  if (args[0] === "tailscale") args.shift();
  if (args.length === 0) return { error: "Type a tailscale command, for example: status" };
  return { command: ["tailscale", ...args.map(shellQuote)].join(" ") };
}
