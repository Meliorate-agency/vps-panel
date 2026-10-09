import "server-only";
import type { NextRequest } from "next/server";
import { nodeIdentity } from "./identity";
import {
  HEADER_KEY,
  HEADER_NONCE,
  HEADER_SIG,
  HEADER_TS,
  MAX_BODY,
  MAX_SKEW_MS,
  identityMessage,
  isKey,
  isNonce,
  randomToken,
  requestMessage,
  responseMessage,
  sha256,
  verifySig,
} from "./protocol";

const USER_AGENT = "vps-panel-network/1";

/** A request to another server failed. status 0: it was never answered. */
export class PeerError extends Error {
  constructor(
    message: string,
    public status: number,
    public body: Record<string, unknown> | null,
  ) {
    super(message);
  }
}

// ------------------------------------------------------------------ outbound

/** POST a signed request to another server and check its signed answer. */
export async function peerRequest<T extends Record<string, unknown> = Record<string, unknown>>(opts: {
  url: string;
  path: string;
  /** The recipient's public key: only that server can answer. */
  to: string;
  body: unknown;
  timeoutMs: number;
}): Promise<{ data: T; rttMs: number }> {
  const id = nodeIdentity();
  const body = JSON.stringify(opts.body);
  const ts = String(Date.now());
  const nonce = randomToken(16);
  const sig = id.sign(requestMessage({ method: "POST", path: opts.path, to: opts.to, ts, nonce, bodyHash: sha256(body) }));
  const started = performance.now();
  let res: Response;
  let raw: Buffer;
  try {
    res = await fetch(opts.url + opts.path, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "user-agent": USER_AGENT,
        [HEADER_KEY]: id.publicKey,
        [HEADER_TS]: ts,
        [HEADER_NONCE]: nonce,
        [HEADER_SIG]: sig,
      },
      body,
      redirect: "manual",
      cache: "no-store",
      signal: AbortSignal.timeout(opts.timeoutMs),
    });
    raw = await readCapped(res);
  } catch (e) {
    if (e instanceof PeerError) throw e;
    throw new PeerError(describeFetchError(e), 0, null);
  }
  const rttMs = Math.round(performance.now() - started);
  const json = parseObject(raw);
  if (!res.ok) throw new PeerError(describeStatus(res, json), res.status, json);
  const rts = res.headers.get(HEADER_TS) ?? "";
  if (
    res.headers.get(HEADER_KEY) !== opts.to ||
    !verifySig(opts.to, responseMessage(nonce, rts, sha256(raw)), res.headers.get(HEADER_SIG))
  ) {
    throw new PeerError(
      "the answer is not signed by that server's key (does another server answer at its address now?)",
      res.status,
      null,
    );
  }
  if (!json) throw new PeerError("unreadable answer", res.status, null);
  return { data: json as T, rttMs };
}

/** Ask the server at `url` for its key, signed over a fresh challenge. */
export async function fetchIdentity(url: string, timeoutMs = 8_000): Promise<{ key: string }> {
  const challenge = randomToken(24);
  let res: Response;
  let raw: Buffer;
  try {
    res = await fetch(`${url}/api/peer/identity?c=${challenge}`, {
      headers: { "user-agent": USER_AGENT },
      redirect: "manual",
      cache: "no-store",
      signal: AbortSignal.timeout(timeoutMs),
    });
    raw = await readCapped(res);
  } catch (e) {
    if (e instanceof PeerError) throw e;
    throw new PeerError(describeFetchError(e), 0, null);
  }
  const json = parseObject(raw);
  if (!res.ok) throw new PeerError(describeStatus(res, json), res.status, json);
  if (!json || !isKey(json.key) || !verifySig(json.key, identityMessage(challenge), json.sig)) {
    throw new PeerError("its answer is not a vps-panel server identity", res.status, null);
  }
  return { key: json.key };
}

async function readCapped(res: Response): Promise<Buffer> {
  if (Number(res.headers.get("content-length") ?? 0) > MAX_BODY) {
    await res.body?.cancel().catch(() => {});
    throw new PeerError("the answer is too large", res.status, null);
  }
  if (!res.body) return Buffer.alloc(0);
  const reader = res.body.getReader();
  const chunks: Buffer[] = [];
  let size = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    size += value.byteLength;
    if (size > MAX_BODY) {
      await reader.cancel().catch(() => {});
      throw new PeerError("the answer is too large", res.status, null);
    }
    chunks.push(Buffer.from(value));
  }
  return Buffer.concat(chunks);
}

function parseObject(raw: Buffer): Record<string, unknown> | null {
  try {
    const v = JSON.parse(raw.toString("utf8"));
    return v && typeof v === "object" && !Array.isArray(v) ? v : null;
  } catch {
    return null;
  }
}

function describeStatus(res: Response, json: Record<string, unknown> | null): string {
  if (res.status >= 300 && res.status < 400) {
    const to = res.headers.get("location");
    return `the address redirects${to ? ` to ${to.slice(0, 120)}` : ""}; use the address it ends up at`;
  }
  const error = typeof json?.error === "string" ? json.error.slice(0, 240) : null;
  if (error === "clock skew" && typeof json?.now === "number") {
    const off = Math.round(Math.abs(json.now - Date.now()) / 1000);
    return `the two servers' clocks are ${off} s apart; turn on time sync on both (timedatectl set-ntp true)`;
  }
  if (error) return error;
  if (res.status === 403) {
    return "HTTP 403 before its panel (its Settings > Panel access, or its panel needs the update that adds My Networks)";
  }
  if (res.status === 404) return "HTTP 404: no My Networks there (its panel needs updating?)";
  if (res.status === 502 || res.status === 503 || res.status === 504) return `HTTP ${res.status}: its panel is not running`;
  return `HTTP ${res.status}`;
}

function describeFetchError(e: unknown): string {
  const err = e as { name?: string; message?: string; cause?: { code?: string; message?: string } };
  if (err?.name === "TimeoutError" || err?.name === "AbortError") return "no answer in time";
  const code = err?.cause?.code ?? "";
  if (code === "ENOTFOUND" || code === "EAI_AGAIN") return "its name does not resolve (DNS)";
  if (code === "ECONNREFUSED") return "connection refused (nothing listens on port 443?)";
  if (code === "ECONNRESET") return "the connection was reset";
  if (code === "UND_ERR_SOCKET") return "the connection closed without an answer";
  if (code === "UND_ERR_CONNECT_TIMEOUT" || code === "ETIMEDOUT") return "connecting timed out (a firewall?)";
  if (code === "EHOSTUNREACH" || code === "ENETUNREACH") return "the host is unreachable";
  if (/CERT|SSL|TLS|SELF_SIGNED|UNABLE_TO_VERIFY/.test(code)) {
    return `its HTTPS certificate is not valid (${err?.cause?.message ?? code})`;
  }
  return err?.cause?.message ?? err?.message ?? String(e);
}

// ------------------------------------------------------------------- inbound

export type SignedRequest = { key: string; nonce: string; body: Record<string, unknown> };

/** nginx sets X-Real-IP; the panel only listens on 127.0.0.1. */
export function clientIp(req: NextRequest): string {
  return req.headers.get("x-real-ip") || req.headers.get("x-forwarded-for")?.split(",")[0]?.trim() || "unknown";
}

/**
 * Read and check a request from another server: size, rate, signature (made
 * for this server's key, over this method, path and body), clock and nonce.
 * Returns the sender's key and the parsed body, or the error response.
 */
export async function readSignedRequest(req: NextRequest, perMinute: number): Promise<SignedRequest | Response> {
  const ip = clientIp(req);
  if (blocked(ip)) return peerError(429, "too many bad requests from this address; try again in a few minutes");
  if (overLimit(req.nextUrl.pathname, ip, perMinute, 60_000)) return peerError(429, "too many requests");
  if (Number(req.headers.get("content-length") ?? 0) > MAX_BODY) return peerError(413, "request too large");
  const text = await req.text();
  if (Buffer.byteLength(text) > MAX_BODY) return peerError(413, "request too large");

  const key = req.headers.get(HEADER_KEY);
  const ts = req.headers.get(HEADER_TS) ?? "";
  const nonce = req.headers.get(HEADER_NONCE);
  if (!isKey(key) || !/^\d{12,15}$/.test(ts) || !isNonce(nonce)) {
    noteFailure(ip);
    return peerError(400, "not a signed request");
  }
  const now = Date.now();
  if (Math.abs(now - Number(ts)) > MAX_SKEW_MS) return peerError(401, "clock skew", { now });
  const signed = requestMessage({
    method: req.method,
    path: req.nextUrl.pathname,
    to: nodeIdentity().publicKey,
    ts,
    nonce,
    bodyHash: sha256(text),
  });
  if (!verifySig(key, signed, req.headers.get(HEADER_SIG))) {
    noteFailure(ip);
    return peerError(401, "bad signature (not meant for this server, or changed on the way)");
  }
  if (replayed(`${key}:${nonce}`, now)) return peerError(401, "replayed request");
  let body: unknown;
  try {
    body = JSON.parse(text);
  } catch {
    return peerError(400, "the body is not JSON");
  }
  if (!body || typeof body !== "object" || Array.isArray(body)) return peerError(400, "the body is not a JSON object");
  return { key, nonce, body: body as Record<string, unknown> };
}

/** A 200 answer signed for the request with `requestNonce`. */
export function signedJson(data: Record<string, unknown>, requestNonce: string): Response {
  const id = nodeIdentity();
  const text = JSON.stringify(data);
  const ts = String(Date.now());
  return new Response(text, {
    headers: {
      "content-type": "application/json",
      "cache-control": "no-store",
      [HEADER_KEY]: id.publicKey,
      [HEADER_TS]: ts,
      [HEADER_SIG]: id.sign(responseMessage(requestNonce, ts, sha256(text))),
    },
  });
}

export function peerError(status: number, error: string, extra: Record<string, unknown> = {}): Response {
  return Response.json({ error, ...extra }, { status, headers: { "cache-control": "no-store" } });
}

// Rate limits and seen nonces, per panel process. On globalThis: the route
// handlers can be separate module instances.
type Window = { start: number; count: number };
type Guard = { windows: Map<string, Window>; nonces: Map<string, number> };

function guard(): Guard {
  const g = globalThis as { __vpsnetGuard?: Guard };
  return (g.__vpsnetGuard ??= { windows: new Map(), nonces: new Map() });
}

const FAILURE_WINDOW_MS = 10 * 60_000;
const MAX_FAILURES = 30;

function count(key: string, windowMs: number, add: boolean): number {
  const { windows } = guard();
  const now = Date.now();
  let w = windows.get(key);
  if (!w || now - w.start >= windowMs) {
    if (!add) return 0;
    w = { start: now, count: 0 };
    windows.set(key, w);
  }
  if (add) w.count += 1;
  if (windows.size > 10_000) {
    for (const [k, v] of windows) if (now - v.start >= FAILURE_WINDOW_MS) windows.delete(k);
  }
  return w.count;
}

export function overLimit(bucket: string, ip: string, max: number, windowMs: number): boolean {
  return count(`${bucket}|${ip}`, windowMs, true) > max;
}

/** A bad signature or join code. Enough of them lock the address out. */
export function noteFailure(ip: string): void {
  count(`fail|${ip}`, FAILURE_WINDOW_MS, true);
}

function blocked(ip: string): boolean {
  return count(`fail|${ip}`, FAILURE_WINDOW_MS, false) >= MAX_FAILURES;
}

function replayed(id: string, now: number): boolean {
  const { nonces } = guard();
  if (nonces.has(id)) return true;
  nonces.set(id, now);
  if (nonces.size > 5_000) {
    // A request is only accepted within MAX_SKEW_MS of its timestamp.
    for (const [k, t] of nonces) if (now - t > 3 * MAX_SKEW_MS) nonces.delete(k);
  }
  return false;
}
