import { agent } from "@/lib/agent";
import { requireSession } from "@/lib/auth";
import { db } from "@/lib/db";
import { getSetting } from "@/lib/settings";
import Table from "@/components/Table/Table";
import styles from "@/components/page.module.css";
import {
  addNotificationAction,
  deleteNotificationAction,
  setEmailFromAction,
  setLogLinesAction,
  testNotificationAction,
} from "@/server-actions/notifications";
import { NOTIFICATION_EVENTS } from "@/lib/notificationEvents";
import EmailFromForm from "./EmailFromForm";
import NotificationForm from "./NotificationForm";
import TestButton from "./TestButton";

const EVENT_LABEL = new Map<string, string>(NOTIFICATION_EVENTS.map((e) => [e.id, e.label]));

export const dynamic = "force-dynamic";

type Row = {
  id: number;
  channel: string;
  target: string;
  events: string;
  secret: string | null;
  log_lines: number;
  last_at: string | null;
  last_event: string | null;
  last_ok: number | null;
  last_detail: string | null;
};

/** An agent timestamp (RFC 3339) the way the messages show it. */
function shortTime(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : `${d.toISOString().slice(0, 16).replace("T", " ")} UTC`;
}

/** The last delivery to a channel, as the agent recorded it. */
function LastDelivery({ r }: { r: Row }) {
  if (!r.last_at) return <span className={styles.muted}>Nothing sent yet</span>;
  const what = r.last_event === "test" ? "test" : (r.last_event ?? "");
  return (
    <span style={{ fontSize: 12 }}>
      <span className={styles.muted}>
        {shortTime(r.last_at)}, {what}:
      </span>{" "}
      {r.last_ok ? (
        <span>{r.last_detail ? `sent, but ${r.last_detail}` : "sent"}</span>
      ) : (
        <span className={styles.error} style={{ display: "inline" }}>
          not delivered: {r.last_detail}
        </span>
      )}
    </span>
  );
}

export default async function NotificationsPage() {
  // A webhook URL is a credential (whoever has it can post to the channel),
  // and the rows carry the Telegram bot tokens: admins only, like the logs.
  const s = await requireSession();
  if (s.role !== "admin") {
    return (
      <div className={styles.head}>
        <div>
          <h1 className={styles.title}>Notifications</h1>
          <div className={styles.subtitle}>Only admins can see the notification channels.</div>
        </div>
      </div>
    );
  }
  const rows = db()
    .prepare(
      `SELECT id, channel, target, events, secret, log_lines, last_at, last_event, last_ok, last_detail
         FROM notifications ORDER BY id DESC`,
    )
    .all() as Row[];
  const mailboxList = await agent.mail.listMailboxes().catch(() => null);
  return (
    <div>
      <div className={styles.head}>
        <div>
          <h1 className={styles.title}>Notifications</h1>
          <div className={styles.subtitle}>
            Where the server reports what fails: tasks (SSL, backups, deploys, updates),
            services that fail or stop, full disks, mail stuck in the queue, running out of
            memory, certificates close to expiry. Each problem is reported once when it starts
            and, for services, disks and the mail queue, once when it is fine again. Every
            message says what failed, on which server, since when and where to look in the
            panel. Webhooks get JSON (Slack and Discord webhook URLs work as is); Telegram
            messages come from your own bot. Send test goes out the way a real message does.
          </div>
          <div className={styles.subtitle}>
            Email goes out through this server&apos;s Postfix: when Postfix itself is down, or
            the server cannot send mail, the message waits until it can. Add a webhook or
            Telegram channel as well to hear about mail problems.
          </div>
        </div>
      </div>

      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>Add channel</h2>
        <NotificationForm action={addNotificationAction} />
      </div>

      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>Email sender</h2>
        <EmailFromForm
          action={setEmailFromAction}
          current={getSetting("notifications.email_from")}
          mailboxes={mailboxList ? mailboxList.mailboxes.map((m) => m.email.toLowerCase()) : null}
        />
      </div>

      <Table
        scrollX
        columns={[
          { key: "channel", label: "Channel", width: "120px" },
          { key: "target", label: "Target" },
          { key: "events", label: "Events" },
          { key: "logs", label: "Log lines", width: "130px" },
          { key: "last", label: "Last delivery" },
          { key: "actions", label: "", width: "260px" },
        ]}
        // Each row's Send test keeps its answer: keyed by channel so the
        // answer stays on its row when a channel is added above or removed.
        rowKey={(_r, i) => rows[i].id}
        rows={rows.map((r) => ({
          channel: <strong>{r.channel}</strong>,
          target:
            r.channel === "telegram" ? (
              <span>
                <span className={styles.code}>{r.target}</span>{" "}
                {/* The part before ":" is the bot's public ID, not the secret. */}
                <span className={styles.muted}>via bot {r.secret?.split(":")[0] ?? "?"}</span>
              </span>
            ) : (
              <span className={styles.code}>{r.target}</span>
            ),
          events: (
            <span className={styles.muted}>
              {r.events
                ? r.events
                    .split(",")
                    .map((e) => EVENT_LABEL.get(e.trim()) ?? e.trim())
                    .join("; ")
                : "All, including events later updates add"}
            </span>
          ),
          logs:
            r.channel === "email" ? (
              <span className={styles.muted} style={{ fontSize: 12 }}>
                Always (sent from this server)
              </span>
            ) : (
              <form
                action={setLogLinesAction.bind(null, r.id, !r.log_lines)}
                style={{ display: "flex", gap: 8, alignItems: "center", fontSize: 12 }}
              >
                <span>{r.log_lines ? "On" : "Off"}</span>
                <button
                  type="submit"
                  className={styles.btnGhost}
                  title={
                    r.log_lines
                      ? "Stop sending log lines: they can contain email and IP addresses"
                      : "Send the last log lines too: they can contain email and IP addresses"
                  }
                >
                  {r.log_lines ? "Turn off" : "Turn on"}
                </button>
              </form>
            ),
          last: <LastDelivery r={r} />,
          actions: (
            <TestButton
              action={testNotificationAction.bind(null, r.id)}
              sentLabel={
                r.channel === "email"
                  ? "Handed to the mail server. Not arriving? Track delivery on Mailboxes shows the receiver's answer."
                  : undefined
              }
            >
              <form action={deleteNotificationAction.bind(null, r.id)}>
                <button type="submit" className={styles.btnDanger}>Remove</button>
              </form>
            </TestButton>
          ),
        }))}
        empty="No notification channels yet."
      />
    </div>
  );
}
