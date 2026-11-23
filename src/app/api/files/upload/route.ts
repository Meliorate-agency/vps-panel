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
 *
 * The agent refuses an upload it will not take (the name exists, the disk is
 * nearly full, a protected folder) before it reads any of the body. The body
 * is therefore announced with `Expect: 100-continue` and sent only once the
 * agent asks for it. Sent straight away, the bytes written after such an
 * answer failed with EPIPE, and that error could win the race against the
 * answer: the operator read "upload stream failed: write EPIPE" in place of
 * the reason.
 *
 * Once the body is on its way the agent can still stop (the disk fills up,
 * the agent restarts). Its answer is shown when one arrives. When the
 * connection just breaks, the operator gets a sentence that says so and
 * names the likely causes.
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
          // The agent has started to answer: its answer is the result, not
          // the error of the body still being written to it.
          let answering = false;
          let sending = false;

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
                Expect: "100-continue",
              },
            },
            (res) => {
              answering = true;
              clearTimeout(unasked);
              const chunks: Buffer[] = [];
              res.on("data", (c: Buffer) => chunks.push(c));
              res.on("end", () => {
                finish({
                  code: res.statusCode ?? 500,
                  body: Buffer.concat(chunks).toString("utf8"),
                });
                // Refused before the body was asked for: nothing was sent, so
                // let go of the agent connection and of the browser's upload.
                if (!sending) {
                  upstream.destroy();
                  req.body?.cancel().catch(() => {});
                }
              });
              // An answer that breaks off still carries the agent's status.
              res.on("error", () => {
                const code = res.statusCode ?? 502;
                finish({
                  code: code >= 400 ? code : 502,
                  body: Buffer.concat(chunks).toString("utf8") || "the agent's answer was cut off",
                });
              });
            },
          );
          // The relay broke while the file was on its way and the agent gave
          // no answer of its own (when it does, that answer is shown). Which
          // side broke decides what to say; the socket error ("write EPIPE",
          // "socket hang up") says nothing an operator can act on.
          let browserFailed = false;
          const mb = () => `${(received / (1024 * 1024)).toFixed(received < 10 * 1024 * 1024 ? 1 : 0)} MB`;
          const broken = (): Relayed =>
            browserFailed
              ? {
                  code: 400,
                  body: JSON.stringify({
                    error: `the upload was cut short: the connection from the browser closed after ${mb()}`,
                  }),
                }
              : {
                  code: 502,
                  body: JSON.stringify({
                    error:
                      `the upload stopped after ${mb()}: the agent closed the connection without giving a reason. ` +
                      "Most often the disk filled up, or the agent was restarted (a panel update restarts it). " +
                      "The file was not saved. Check the free space on the Dashboard, then upload it again.",
                  }),
                };
          upstream.on("error", (e) => {
            clearTimeout(unasked);
            if (answering && !stopped) return;
            finish(sending ? broken() : { code: 502, body: `agent unreachable: ${e.message}` });
          });
          // Readable.fromWeb bridges the web ReadableStream to a node stream so
          // it can be piped without ever being fully realised. On any error
          // pipeline destroys the agent request mid-body.
          const send = () => {
            if (sending || answering) return;
            sending = true;
            clearTimeout(unasked);
            const source = Readable.fromWeb(req.body as Parameters<typeof Readable.fromWeb>[0]);
            source.once("error", () => {
              browserFailed = true;
            });
            pipeline(source, counter, upstream, (err) => {
              if (!err || (answering && !stopped)) return;
              finish(broken());
            });
          };
          upstream.once("continue", send);
          // The agent asks within milliseconds. Should the Expect go
          // unanswered, the body is sent anyway, as it always was.
          const unasked = setTimeout(send, 3000);
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
