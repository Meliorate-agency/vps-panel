import { agent } from "@/lib/agent";
import { db } from "@/lib/db";
import { getSetting } from "@/lib/settings";
import Table from "@/components/Table/Table";
import styles from "@/components/page.module.css";
import {
  addNotificationAction,
  deleteNotificationAction,
  setEmailFromAction,
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
};

export default async function NotificationsPage() {
  const rows = db()
    .prepare(`SELECT id, channel, target, events, secret FROM notifications ORDER BY id DESC`)
    .all() as Row[];
  const mailboxList = await agent.mail.listMailboxes().catch(() => null);
  return (
    <div>
      <div className={styles.head}>
        <div>
          <h1 className={styles.title}>Notifications</h1>
          <div className={styles.subtitle}>
            Where the agent reports failed SSL orders and renewals, backups, deploys and other
            tasks, services that stop in the failed state, and a root disk at 90%. Email goes
            out through this server&apos;s Postfix; webhooks get JSON (Slack and Discord
            webhook URLs work as is); Telegram messages come from your own bot. Use Send test
            after adding a channel.
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
          mailboxes={(mailboxList?.mailboxes ?? []).map((m) => m.email.toLowerCase())}
          server={mailboxList?.server ?? ""}
        />
      </div>

      <Table
        scrollX
        columns={[
          { key: "channel", label: "Channel", width: "120px" },
          { key: "target", label: "Target" },
          { key: "events", label: "Events" },
          { key: "actions", label: "", width: "260px" },
        ]}
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
                    .join(", ")
                : "(all)"}
            </span>
          ),
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
