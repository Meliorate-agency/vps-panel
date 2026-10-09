import { NextRequest } from "next/server";
import http from "node:http";
import { config } from "@/lib/config";
import { getSession } from "@/lib/auth";

export const dynamic = "force-dynamic";
export const runtime = "nodejs";

export async function GET(
  req: NextRequest,
  { params }: { params: Promise<{ unit: string }> },
) {
  const session = await getSession();
  // Reject both unauthenticated and pre-TOTP (needsTotp) sessions: pages and
  // server actions redirect needsTotp sessions to /totp, but these API routes
  // are a separate surface — without this check a password-only session could
  // stream live logs / read tasks before completing 2FA, defeating the second
  // factor for the read/stream API.
  if (!session || session.needsTotp)
    return new Response("unauthorized", { status: 401 });
  // Logs can carry mailbox usernames, client IPs, env values and build
  // secrets: admins only, like everything else that exposes the server.
  if (session.role !== "admin") return new Response("admins only", { status: 403 });
  const { unit } = await params;
  if (!/^[a-zA-Z0-9_.@:\-]{1,200}$/.test(unit)) {
    return new Response("invalid unit", { status: 400 });
  }
  const lines = req.nextUrl.searchParams.get("lines") || "200";
  const path = `/logs/${encodeURIComponent(unit)}/stream?lines=${encodeURIComponent(lines)}`;

  // The upstream agent request, so a closed browser tab can end it. Without
  // cancel() every log view left an agent connection and a journalctl -f
  // running on the server.
  let upstream: http.ClientRequest | null = null;
  const stream = new ReadableStream<Uint8Array>({
    cancel() {
      upstream?.destroy();
    },
    start(controller) {
      const r = http.request(
        {
          socketPath: config.agentSocket,
          method: "GET",
          path,
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
              // closed
            }
          });
          res.on("end", () => {
            try { controller.close(); } catch { /* */ }
          });
          res.on("error", (e) => {
            try { controller.error(e); } catch { /* */ }
          });
        },
      );
      r.on("error", (e) => {
        try { controller.error(e); } catch { /* */ }
      });
      upstream = r;
      req.signal.addEventListener("abort", () => r.destroy(), { once: true });
      r.end();
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
