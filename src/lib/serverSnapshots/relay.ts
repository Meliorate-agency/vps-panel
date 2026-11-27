import "server-only";
import http from "node:http";
import { relayBody } from "@/lib/agent";
import { config } from "@/lib/config";

/** A snapshot's name (its file name without `.vpssnap.tar.zst.age`), as the
 *  agent checks it too: `<host>-<YYYYMMDD-HHMMSS>Z`. */
export const SNAPSHOT_NAME = /^[a-z0-9][a-z0-9-]{0,40}-\d{8}-\d{6}Z$/;

/** What is passed on from the agent's answer. */
const PASS_HEADERS = [
  "content-type",
  "content-length",
  "content-range",
  "accept-ranges",
  "etag",
  "content-disposition",
  "x-content-type-options",
];

/**
 * The agent's answer for one snapshot file (200, 206, 416, or the headers
 * alone for HEAD), relayed as it streams: a snapshot is many gigabytes and
 * only ever passes through, read as fast as the client takes it.
 * `Range` / `If-Range` are passed on so a broken download resumes.
 */
export function relaySnapshotFile(opts: {
  name: string;
  method: "GET" | "HEAD";
  range: string | null;
  ifRange: string | null;
  /** Bytes as they go out (the pull link counts what it served). */
  onBytes?: (n: number) => void;
  /** Once the body ended or the client went away. */
  onEnd?: () => void;
}): Promise<Response> {
  const headers: Record<string, string> = { Authorization: `Bearer ${config.agentToken}` };
  if (opts.range) headers.Range = opts.range;
  if (opts.ifRange) headers["If-Range"] = opts.ifRange;
  return new Promise<Response>((resolve) => {
    const upstream = http.request(
      {
        socketPath: config.agentSocket,
        method: opts.method,
        path: `/server-snapshots/${encodeURIComponent(opts.name)}/file`,
        headers,
      },
      (res) => {
        const status = res.statusCode ?? 500;
        const out = new Headers();
        for (const h of PASS_HEADERS) {
          const v = res.headers[h];
          if (typeof v === "string") out.set(h, v);
        }
        // no-transform: nothing on the way may compress or re-encode the
        // bytes, or a resumed range would not fit the part already held.
        out.set("Cache-Control", "private, no-store, no-transform");
        if (status >= 400 && status !== 416) {
          const chunks: Buffer[] = [];
          res.on("data", (c: Buffer) => chunks.push(c));
          res.on("end", () =>
            resolve(
              new Response(Buffer.concat(chunks).toString("utf8") || "download failed", {
                status,
                headers: { "Content-Type": "application/json", "Cache-Control": "no-store" },
              }),
            ),
          );
          return;
        }
        if (opts.method === "HEAD" || status === 416) {
          res.resume();
          resolve(new Response(null, { status, headers: out }));
          return;
        }
        if (!opts.onBytes && !opts.onEnd) {
          resolve(new Response(relayBody(res), { status, headers: out }));
          return;
        }
        let ended = false;
        const end = () => {
          if (ended) return;
          ended = true;
          opts.onEnd?.();
        };
        // A transfer that broke off (the agent connection dropped) ends here
        // too, so what was sent before the break is counted.
        res.on("close", end);
        const counted = relayBody(res, end).pipeThrough(
          new TransformStream<Uint8Array, Uint8Array>({
            transform(chunk, controller) {
              opts.onBytes?.(chunk.byteLength);
              controller.enqueue(chunk);
            },
            flush() {
              end();
            },
          }),
        );
        resolve(new Response(counted, { status, headers: out }));
      },
    );
    upstream.on("error", (e) => resolve(new Response(`agent unreachable: ${e.message}`, { status: 502 })));
    upstream.end();
  });
}
