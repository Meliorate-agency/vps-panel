import { NextRequest } from "next/server";
import http from "node:http";
import { config } from "@/lib/config";
import { getSession } from "@/lib/auth";

export const dynamic = "force-dynamic";
export const runtime = "nodejs";

/**
 * Streams a file out of the agent to the browser.
 *
 * A plain server action can't do this — action results are serialised, so a
 * 2 GB video would have to be materialised in memory first. This proxies the
 * agent's response body straight through instead.
 *
 * All the path safety lives in the agent (`files::download`): it canonicalises
 * the leaf as well as the parent so a symlink can't read out of the root, and
 * refuses protected paths. This route only checks the session and relays.
 */
export async function GET(req: NextRequest) {
  const session = await getSession();
  if (!session || session.needsTotp) {
    return new Response("unauthorized", { status: 401 });
  }
  if (session.role !== "admin") {
    return new Response("forbidden", { status: 403 });
  }

  const sp = req.nextUrl.searchParams;
  const root = sp.get("root");
  const path = sp.get("path");
  if (!root || !path) {
    return new Response("root and path are required", { status: 400 });
  }

  const query = new URLSearchParams({ root, path });
  if (sp.get("inline") === "1") query.set("inline", "true");

  return new Promise<Response>((resolve) => {
    const upstream = http.request(
      {
        socketPath: config.agentSocket,
        method: "GET",
        path: `/files/download?${query.toString()}`,
        headers: { Authorization: `Bearer ${config.agentToken}` },
      },
      (res) => {
        const status = res.statusCode ?? 500;
        if (status >= 400) {
          // Error bodies are small JSON — buffer them so the message reaches
          // the UI instead of an opaque status code.
          const chunks: Buffer[] = [];
          res.on("data", (c: Buffer) => chunks.push(c));
          res.on("end", () =>
            resolve(
              new Response(Buffer.concat(chunks).toString("utf8") || "download failed", {
                status,
                headers: { "Content-Type": "application/json" },
              }),
            ),
          );
          return;
        }

        const body = new ReadableStream<Uint8Array>({
          start(controller) {
            res.on("data", (chunk: Buffer) => {
              try {
                controller.enqueue(new Uint8Array(chunk));
              } catch {
                // controller already closed (client went away)
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
            res.destroy();
          },
        });

        const headers = new Headers();
        for (const h of [
          "content-type",
          "content-length",
          "content-disposition",
          "x-content-type-options",
        ]) {
          const v = res.headers[h];
          if (typeof v === "string") headers.set(h, v);
        }
        headers.set("Cache-Control", "private, no-store");
        resolve(new Response(body, { status, headers }));
      },
    );
    upstream.on("error", (e) =>
      resolve(new Response(`agent unreachable: ${e.message}`, { status: 502 })),
    );
    upstream.end();
  });
}
