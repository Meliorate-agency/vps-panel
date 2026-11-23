import { agent } from "@/lib/agent";
import { requireSession } from "@/lib/auth";
import Table from "@/components/Table/Table";
import ConfirmDeleteButton from "@/components/ConfirmDeleteButton/ConfirmDeleteButton";
import styles from "@/components/page.module.css";
import { createCronAction, deleteCronAction } from "@/server-actions/cron";
import CronForm from "./CronForm";

export const dynamic = "force-dynamic";

export default async function CronPage() {
  // A job's command line often carries a token or a password (a curl with
  // an API key, a database dump): admins only, like the audit log.
  const s = await requireSession();
  if (s.role !== "admin") {
    return (
      <div className={styles.head}>
        <div>
          <h1 className={styles.title}>Cron jobs</h1>
          <div className={styles.subtitle}>Only admins can see the cron jobs.</div>
        </div>
      </div>
    );
  }
  // /system/users is cheap and stable so fetching it on every page load is
  // fine. Empty array fallback keeps the dropdown from going blank if the
  // agent socket is down — CronForm handles that case by prepending root.
  const [jobs, users] = await Promise.all([
    agent.cron.list().catch(() => []),
    agent.system.users().catch(() => []),
  ]);
  return (
    <div>
      <div className={styles.head}>
        <div>
          <h1 className={styles.title}>Cron jobs</h1>
          <div className={styles.subtitle}>Panel-managed entries in /etc/cron.d/.</div>
        </div>
      </div>

      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>Add cron job</h2>
        <CronForm action={createCronAction} users={users} />
      </div>

      <Table
        columns={[
          { key: "label", label: "Label" },
          { key: "schedule", label: "Schedule" },
          { key: "user", label: "User", width: "100px" },
          { key: "command", label: "Command" },
          { key: "actions", label: "", width: "100px" },
        ]}
        rows={jobs.map((j) => ({
          label: j.label || <span className={styles.muted}>(none)</span>,
          schedule: <span className={styles.code}>{j.schedule}</span>,
          user: j.user,
          command: <span className={styles.code}>{j.command}</span>,
          actions: (
            <ConfirmDeleteButton
              action={deleteCronAction.bind(null, j.id)}
              title={j.label ? `Delete cron job "${j.label}"?` : "Delete cron job?"}
              buttonLabel="Delete"
              confirmLabel="Delete job"
              body={
                <p>
                  <code className={styles.code}>
                    {j.schedule} {j.command}
                  </code>{" "}
                  stops running as <strong>{j.user}</strong>: its file in{" "}
                  <span className={styles.code}>/etc/cron.d</span> is removed.
                </p>
              }
            />
          ),
        }))}
        empty="No cron jobs yet."
      />
    </div>
  );
}
