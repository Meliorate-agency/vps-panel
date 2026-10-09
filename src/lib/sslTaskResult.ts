/**
 * Shape returned by the SSL server actions that hand back an agent task
 * (renew a cert, issue the mail.<domain> cert). Shared by the server actions
 * and the client-side <SslTaskButton> that follows the task, so it lives in a
 * plain module both can import.
 */
export type SslTaskResult =
  | { ok: true; taskId: string | null; message?: string }
  | { ok: false; error: string };
