import { NextRequest } from "next/server";
import http from "node:http";
import { Readable, Transform, pipeline } from "node:stream";
import { config } from "@/lib/config";
import { getSession } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";
import { originRefusal } from "@/lib/csrf";
import { uploadLimitMb } from "@/lib/uploadLimit";

export const dynamic = "force-dynamic";
export const runtime = "nodejs";

type Relayed = { code: number; body: string };

/**
 * Streams an upload through to the agent.
 *
 * The request body is the file's raw bytes — the client sends the File object
 * directly, no multipart wrapper. That keeps the bytes flowing browser →
 * panel → agent → disk without any hop calling `formData()` and buffering a
 * multi-GB file in memory. The filename rides in the query string.
 *
 * src/proxy.ts does not match this route (it would keep only the first 10 MB
 * of the body), so the Origin check is made here.
 *
 * The size limit is the operator's (Settings > Panel > Uploads, lib/uploadLimit)
 * and is enforced here: up front from Content-Length, and by counting the
 * bytes as they pass. An upload stopped midway reaches the agent as a broken
 * stream, which it discards instead of saving what arrived. Free-space checks
 * and the temp-file-then-rename dance are the agent's job (`files::upload`).
 */
export async function POST(req: NextRequest) {
  const refused = originRefusal(req.headers);
  if (refused) return new Response(refused.message, { status: refused.status });

  const session = await getSession();
  if (!session || session.needsTotp) {
    return new Response("unauthorized", { status: 401 });
  }
  if (session.role !== "admin") {
    return new Response("forbidden", { status: 403 });
  }

  const sp = req.nextUrl.searchParams;
  const root = sp.get("root");
  const name = sp.get("name");
  if (!root || !name) {
    return new Response("root and name are required", { status: 400 });
  }
  if (!req.body) {
    return new Response("empty request body", { status: 400 });
  }

  const limitMb = uploadLimitMb();
  const limit = limitMb * 1024 * 1024;
  const tooLarge: Relayed = {
    code: 413,
    body: JSON.stringify({
      error: `larger than the ${limitMb} MB upload limit (Settings > Panel > Uploads)`,
    }),
  };
  const lengthHeader = req.headers.get("content-length");
  const declared = lengthHeader !== null && /^\d+$/.test(lengthHeader) ? Number(lengthHeader) : null;

  const query = new URLSearchParams({ root, path: sp.get("path") ?? "", name });
  if (sp.get("overwrite") === "1") query.set("overwrite", "true");

  const status: Relayed =
    declared !== null && declared > limit
      ? tooLarge
      : await new Promise<Relayed>((resolve) => {
          // Why the panel stopped the relay, when it did: that is the answer,
          // not the error the broken stream then causes on the agent side.
          let stopped: Relayed | null = null;
          const finish = (r: Relayed) => resolve(stopped ?? r);

          let received = 0;
          const counter = new Transform({
            transform(chunk: Buffer, _enc, cb) {
              received += chunk.length;
              if (received > limit) {
                stopped = tooLarge;
                return cb(new Error("over the upload limit"));
              }
              cb(null, chunk);
            },
            flush(cb) {
              // A body that ends before its Content-Length must not be saved
              // as the whole file.
              if (declared !== null && received !== declared) {
                stopped = {
                  code: 400,
                  body: JSON.stringify({
                    error: `the upload was cut short: ${received} of ${declared} bytes arrived`,
                  }),
                };
                return cb(new Error("body ended early"));
              }
              cb();
            },
          });

          const upstream = http.request(
            {
              socketPath: config.agentSocket,
              method: "POST",
              path: `/files/upload?${query.toString()}`,
              headers: {
                Authorization: `Bearer ${config.agentToken}`,
                "Content-Type": "application/octet-stream",
              },
            },
            (res) => {
              const chunks: Buffer[] = [];
              res.on("data", (c: Buffer) => chunks.push(c));
              res.on("end", () =>
                finish({
                  code: res.statusCode ?? 500,
                  body: Buffer.concat(chunks).toString("utf8"),
                }),
              );
            },
          );
          upstream.on("error", (e) => finish({ code: 502, body: `agent unreachable: ${e.message}` }));
          // Readable.fromWeb bridges the web ReadableStream to a node stream so
          // it can be piped without ever being fully realised. On any error
          // pipeline destroys the agent request mid-body.
          pipeline(
            Readable.fromWeb(req.body as Parameters<typeof Readable.fromWeb>[0]),
            counter,
            upstream,
            (err) => {
              if (err) {
                finish({ code: 400, body: JSON.stringify({ error: `upload stream failed: ${err.message}` }) });
              }
            },
          );
        });

  // Audit the path, never the contents — an uploaded file may be anything.
  writeAudit({
    actorId: session.userId,
    action: "files.upload",
    target: `${root}:${sp.get("path") ?? ""}/${name}`,
    result: status.code < 400 ? "ok" : "error",
    message: status.code < 400 ? undefined : status.body.slice(0, 300),
  });

  return new Response(status.body || (status.code < 400 ? "{}" : "upload failed"), {
    status: status.code,
    headers: { "Content-Type": "application/json" },
  });
}
