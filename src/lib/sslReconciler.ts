import "server-only";
import { revalidatePath } from "next/cache";
import { agent } from "@/lib/agent";
import { db } from "@/lib/db";
import { writeAudit } from "@/lib/audit";

/**
 * Background fire-and-forget watcher: poll the agent's task status until the
 * SSL task `taskId` for `domain` settles, then update `sites.last_ssl_error`
 * / `sites.ssl_issued_at` so the `/domains` UI flips out of the misleading
 * "issuing..." state.
 *
 * Why poll instead of SSE: the existing `/api/tasks/:id/stream` route lives
 * in Next.js and proxies the agent's SSE, but it requires a session cookie
 * and routes through the panel itself. Direct unix-socket SSE from the panel
 * to the agent would need a custom SSE parser. The agent already exposes a
 * cheap `GET /tasks/:id` snapshot endpoint — polling that every 5s is fewer
 * moving parts than wiring up an SSE consumer for a job that usually finishes
 * in under a minute.
 *
 * Lifecycle: spawned via `void pollSslTaskAndReconcile(...)` from a Server
 * Action right after the agent accepts the create/update. The promise
 * outlives the action's response — Next.js running under `next start` keeps
 * the Node process alive long enough for it to finish. If the panel restarts
 * mid-poll the worst case is a stuck "issuing..." until the next page load
 * triggers another reconciliation (a future safety net we may add).
 */
export function pollSslTaskAndReconcile(
  domain: string,
  taskId: string,
  actorId: number,
): void {
  const POLL_INTERVAL_MS = 5_000;
  const MAX_TOTAL_MS = 15 * 60 * 1000; // 15 min hard cap

  const started = Date.now();

  const tick = async (): Promise<void> => {
    if (Date.now() - started > MAX_TOTAL_MS) {
      const msg = `ssl task ${taskId} did not finish within 15 minutes`;
      try {
        db()
          .prepare(`UPDATE sites SET last_ssl_error = ? WHERE domain = ?`)
          .run(msg, domain);
        writeAudit({
          actorId,
          action: "sites.ssl_reconcile",
          target: domain,
          result: "error",
          message: msg,
        });
      } catch {
        // best effort
      }
      return;
    }

    let task;
    try {
      task = await agent.tasks.get(taskId);
    } catch {
      // Agent transiently unreachable. Try again.
      setTimeout(tick, POLL_INTERVAL_MS).unref?.();
      return;
    }

    if (task.status === "pending" || task.status === "running") {
      setTimeout(tick, POLL_INTERVAL_MS).unref?.();
      return;
    }

    try {
      if (task.status === "succeeded") {
        db()
          .prepare(
            `UPDATE sites SET ssl_issued_at = datetime('now'), last_ssl_error = NULL WHERE domain = ?`,
          )
          .run(domain);
        writeAudit({
          actorId,
          action: "sites.ssl_reconcile",
          target: domain,
          result: "ok",
          message: `task ${taskId} succeeded`,
        });
      } else {
        // Failed. Surface the last log line as the error message so the
        // /domains "Retry SSL" button has something meaningful next to it.
        const lastLine =
          task.log.length > 0 ? task.log[task.log.length - 1] : "ssl task failed";
        const msg = lastLine.length > 500 ? lastLine.slice(0, 500) : lastLine;
        db()
          .prepare(`UPDATE sites SET last_ssl_error = ? WHERE domain = ?`)
          .run(msg, domain);
        writeAudit({
          actorId,
          action: "sites.ssl_reconcile",
          target: domain,
          result: "error",
          message: `task ${taskId}: ${msg}`,
        });
      }
      // Settle => invalidate the cached snapshot so the next request to
      // /domains (or the per-domain detail page) re-queries `sites` and
      // re-fetches the cert status from the agent. Without this, the row
      // stays on "issuing..." forever from the operator's perspective even
      // though the cert is already on disk.
      revalidatePath("/domains");
      revalidatePath(`/domains/${encodeURIComponent(domain)}`);
    } catch {
      // best effort — the site row may have been deleted between create and reconcile
    }
  };

  // Don't await — let it run in the background.
  setTimeout(tick, POLL_INTERVAL_MS).unref?.();
}
