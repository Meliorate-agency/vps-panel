import StatusBadge from "@/components/StatusBadge/StatusBadge";
import styles from "@/components/page.module.css";
import type { MailCheckItem, MailCheckStatus, MailIpProtocol } from "@/lib/agent";
import local from "./mailIp.module.css";

export const PROTOCOL_NAME: Record<MailIpProtocol, string> = { ipv4: "IPv4", ipv6: "IPv6" };

/** Words on the badge, so the status does not rest on its colour. */
const STATUS: Record<MailCheckStatus, { text: string; tone: "ok" | "warn" | "danger" }> = {
  ok: { text: "ok", tone: "ok" },
  warn: { text: "warning", tone: "warn" },
  fail: { text: "problem", tone: "danger" },
};

/** The agent's check items: a badge, the title, and its explanation. */
export function CheckList({ checks }: { checks: MailCheckItem[] }) {
  if (checks.length === 0) return <p className={local.sub}>Nothing was checked.</p>;
  return (
    <ul className={local.checks}>
      {checks.map((c, i) => {
        const s = STATUS[c.status] ?? { text: String(c.status), tone: "muted" as const };
        return (
          <li key={`${c.id}-${i}`} className={local.check}>
            <span className={local.checkBadge}>
              <StatusBadge text={s.text} tone={s.tone} />
            </span>
            <span className={local.checkLabel}>{c.label}</span>
            <span className={local.checkDetail}>{c.detail}</span>
          </li>
        );
      })}
    </ul>
  );
}

/** Some check is not ok: the operator may want to know where to fix it. */
export function hasProblems(checks: MailCheckItem[]): boolean {
  return checks.some((c) => c.status !== "ok");
}

/**
 * The agent refused a change with 409 (IPv6 not ready, a port the server
 * needs, no way left to log in over SSH). Shows its reason and lets the
 * operator apply it anyway; nothing is sent with force until they click.
 * The defaults are the mail IPv6 wording.
 *
 * Inside a `<form action>`, pass `submitName` instead of `onForce`: the
 * button then submits the form with `<submitName>=1`, which the action reads
 * as the confirmation.
 */
export function ForceConfirm({
  message,
  busy,
  onForce,
  submitName,
  onCancel,
  note = "Until that is fixed, receiving servers may refuse mail sent over IPv6 or put it in spam.",
  forceLabel = "Use IPv6 anyway",
  busyLabel = "Saving...",
}: {
  message: string;
  busy: boolean;
  onForce?: () => void;
  submitName?: string;
  onCancel: () => void;
  note?: string | null;
  forceLabel?: string;
  busyLabel?: string;
}) {
  return (
    <div className={local.warnBox} role="alert">
      <p>{message}</p>
      {note && <p className={local.sub}>{note}</p>}
      <div className={local.row}>
        {submitName ? (
          <button
            type="submit"
            name={submitName}
            value="1"
            className={styles.btnGhost}
            disabled={busy}
          >
            {busy ? busyLabel : forceLabel}
          </button>
        ) : (
          <button type="button" className={styles.btnGhost} onClick={onForce} disabled={busy}>
            {busy ? busyLabel : forceLabel}
          </button>
        )}
        <button type="button" className={styles.btnGhost} onClick={onCancel} disabled={busy}>
          Cancel
        </button>
      </div>
    </div>
  );
}

export function Busy({ label }: { label: string }) {
  return (
    <span className={local.busy}>
      <span className={local.spinner} aria-hidden />
      {label}
    </span>
  );
}

export function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}
