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
 * The agent refused IPv6 because that side is not ready (409). Shows its
 * reason and lets the operator apply it anyway; nothing is sent with force
 * until they click.
 */
export function ForceConfirm({
  message,
  busy,
  onForce,
  onCancel,
}: {
  message: string;
  busy: boolean;
  onForce: () => void;
  onCancel: () => void;
}) {
  return (
    <div className={local.warnBox} role="alert">
      <p>{message}</p>
      <p className={local.sub}>
        Until that is fixed, receiving servers may refuse mail sent over IPv6 or put it in spam.
      </p>
      <div className={local.row}>
        <button type="button" className={styles.btnGhost} onClick={onForce} disabled={busy}>
          {busy ? "Saving..." : "Use IPv6 anyway"}
        </button>
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
