import { NextRequest } from "next/server";
import http from "node:http";
import { config } from "@/lib/config";
import { getSession } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";

export const dynamic = "force-dynamic";
export const runtime = "nodejs";

/**
 * Runs one shell command and streams its output back as NDJSON.
 *
 * A server action can't do this: its result is serialised once at the end, so
 * output would only appear after the command finished. This proxies the
 * agent's streaming body straight through instead.
 *
 * The audit row is written BEFORE the command is dispatched, deliberately. A
 * command that reboots the box, kills the panel, or fills the disk would never
 * come back to write its own record afterwards — and that's exactly the
 * command you'd want a record of.
 */
export async function POST(req: NextRequest) {
  const session = await getSession();
  if (!session || session.needsTotp) {
    return new Response("unauthorized", { status: 401 });
  }
  if (session.role !== "admin") {
    return new Response("forbidden", { status: 403 });
  }

  let body: {
    cwd?: string;
    command?: string;
    timeout_secs?: number;
    run_as?: string;
    env_files?: string[];
  };
  try {
    body = await req.json();
  } catch {
    return new Response("invalid JSON body", { status: 400 });
  }

  const cwd = (body.cwd ?? "").trim();
  const command = (body.command ?? "").trim();
  if (!cwd.startsWith("/")) {
    return new Response("cwd must be an absolute path", { status: 400 });
  }
  if (!command) {
    return new Response("command must not be empty", { status: 400 });
  }

  const runAs = (body.run_as ?? "").trim();
  // Env files are paths, so they get the same treatment as any other path the
  // browser supplies: absolute only, no traversal. The agent skips missing
  // ones, but it will happily source anything it's handed — sourcing a file is
  // executing it, so the panel decides the list, not a crafted request.
  const envFiles = (body.env_files ?? [])
    .filter((f) => typeof f === "string" && f.startsWith("/") && !f.includes(".."))
    .slice(0, 8);

  writeAudit({
    actorId: session.userId,
    action: "terminal.exec",
    target: cwd,
    result: "ok",
    message: `${runAs && runAs !== "root" ? `[as ${runAs}] ` : ""}${command}`.slice(0, 2000),
  });

  const payload = JSON.stringify({
    cwd,
    command,
    timeout_secs: body.timeout_secs,
    run_as: runAs || undefined,
    env_files: envFiles,
  });

  return new Promise<Response>((resolve) => {
    const upstream = http.request(
      {
        socketPath: config.agentSocket,
        method: "POST",
        path: "/terminal/exec",
        headers: {
          Authorization: `Bearer ${config.agentToken}`,
          "Content-Type": "application/json",
          "Content-Length": String(Buffer.byteLength(payload)),
        },
      },
      (res) => {
        const status = res.statusCode ?? 500;
        if (status >= 400) {
          const chunks: Buffer[] = [];
          res.on("data", (c: Buffer) => chunks.push(c));
          res.on("end", () =>
            resolve(
              new Response(Buffer.concat(chunks).toString("utf8") || "exec failed", {
                status,
                headers: { "Content-Type": "application/json" },
              }),
            ),
          );
          return;
        }

        const stream = new ReadableStream<Uint8Array>({
          start(controller) {
            res.on("data", (chunk: Buffer) => {
              try {
                controller.enqueue(new Uint8Array(chunk));
              } catch {
                // controller already closed
              }
            });
            res.on("end", () => {
              try {
                controller.close();
              } catch {
                // already closed
              }
            });
            res.on("error", (e) => {
              try {
                controller.error(e);
              } catch {
                // already closed
              }
            });
          },
          cancel() {
            // The operator hit Stop or closed the tab. Destroying the socket
            // propagates to the agent, which drops the response body and kills
            // the command's whole process group.
            res.destroy();
            upstream.destroy();
          },
        });

        resolve(
          new Response(stream, {
            status,
            headers: {
              "Content-Type": "application/x-ndjson",
              "Cache-Control": "no-store",
              "X-Accel-Buffering": "no",
            },
          }),
        );
      },
    );
    upstream.on("error", (e) =>
      resolve(new Response(`agent unreachable: ${e.message}`, { status: 502 })),
    );
    upstream.write(payload);
    upstream.end();
  });
}
