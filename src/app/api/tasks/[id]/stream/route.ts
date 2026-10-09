import { NextRequest } from "next/server";
import http from "node:http";
import { config } from "@/lib/config";
import { getSession } from "@/lib/auth";

export const dynamic = "force-dynamic";
export const runtime = "nodejs";

export async function GET(
  _req: NextRequest,
  { params }: { params: Promise<{ id: string }> },
) {
  const session = await getSession();
  if (!session || session.needsTotp)
    return new Response("unauthorized", { status: 401 });
  // Logs can carry mailbox usernames, client IPs, env values and build
  // secrets: admins only, like everything else that exposes the server.
  if (session.role !== "admin") return new Response("admins only", { status: 403 });
  const { id } = await params;
  if (!/^[a-zA-Z0-9-]{1,64}$/.test(id)) {
    return new Response("invalid id", { status: 400 });
  }
  return proxySse(`/tasks/${id}/stream`);
}

function proxySse(agentPath: string): Response {
  // See the logs stream route: cancel() ends the upstream request when the
  // browser goes away, instead of leaking an agent connection per view.
  let upstream: http.ClientRequest | null = null;
  const stream = new ReadableStream<Uint8Array>({
    cancel() {
      upstream?.destroy();
    },
    start(controller) {
      const req = http.request(
        {
          socketPath: config.agentSocket,
          method: "GET",
          path: agentPath,
          headers: {
            Authorization: `Bearer ${config.agentToken}`,
            Accept: "text/event-stream",
          },
        },
        (res) => {
          if ((res.statusCode ?? 500) >= 400) {
            controller.enqueue(
              new TextEncoder().encode(`event: error\ndata: agent ${res.statusCode}\n\n`),
            );
            controller.close();
            return;
          }
          res.on("data", (chunk: Buffer) => {
            try {
              controller.enqueue(new Uint8Array(chunk));
            } catch {
              // controller closed
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
      );
      req.on("error", (e) => {
        try {
          controller.error(e);
        } catch {
          // already closed
        }
      });
      upstream = req;
      req.end();
    },
  });

  return new Response(stream, {
    headers: {
      "Content-Type": "text/event-stream; charset=utf-8",
      "Cache-Control": "no-cache, no-transform",
      Connection: "keep-alive",
      "X-Accel-Buffering": "no",
    },
  });
}
