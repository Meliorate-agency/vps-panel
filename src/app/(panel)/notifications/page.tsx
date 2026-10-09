import { db } from "@/lib/db";
import Table from "@/components/Table/Table";
import styles from "@/components/page.module.css";
import {
  addNotificationAction,
  deleteNotificationAction,
  testNotificationAction,
} from "@/server-actions/notifications";
import { NOTIFICATION_EVENTS } from "@/lib/notificationEvents";
import NotificationForm from "./NotificationForm";
import TestButton from "./TestButton";

const EVENT_LABEL = new Map<string, string>(NOTIFICATION_EVENTS.map((e) => [e.id, e.label]));

export const dynamic = "force-dynamic";

type Row = {
  id: number;
  channel: string;
  target: string;
  events: string;
};

export default function NotificationsPage() {
  const rows = db()
    .prepare(`SELECT id, channel, target, events FROM notifications ORDER BY id DESC`)
    .all() as Row[];
  return (
    <div>
      <div className={styles.head}>
        <div>
          <h1 className={styles.title}>Notifications</h1>
          <div className={styles.subtitle}>
            Where the agent reports failed SSL orders and renewals, backups, deploys and other
            tasks, services that stop in the failed state, and a root disk at 90%. Email goes
            out through this server&apos;s Postfix; webhooks get JSON (Slack and Discord
            webhook URLs work as is). Use Send test after adding a channel.
          </div>
        </div>
      </div>

      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>Add channel</h2>
        <NotificationForm action={addNotificationAction} />
      </div>

      <Table
        columns={[
          { key: "channel", label: "Channel", width: "120px" },
          { key: "target", label: "Target" },
          { key: "events", label: "Events" },
          { key: "actions", label: "", width: "260px" },
        ]}
        rows={rows.map((r) => ({
          channel: <strong>{r.channel}</strong>,
          target: <span className={styles.code}>{r.target}</span>,
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
            <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
              <TestButton action={testNotificationAction.bind(null, r.id)} />
              <form action={deleteNotificationAction.bind(null, r.id)}>
                <button type="submit" className={styles.btnDanger}>Remove</button>
              </form>
            </div>
          ),
        }))}
        empty="No notification channels yet."
      />
    </div>
  );
}
