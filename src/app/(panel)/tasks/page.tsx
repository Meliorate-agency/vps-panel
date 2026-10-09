import Link from "next/link";
import { getPanelTimezone } from "@/lib/audit";
import {
  formatTaskTs,
  listTaskDays,
  listTasksForDay,
  todayForTasks,
} from "@/lib/tasksDb";
import Table from "@/components/Table/Table";
import StatusBadge from "@/components/StatusBadge/StatusBadge";
import styles from "@/components/page.module.css";

export const dynamic = "force-dynamic";

const DATE_RE = /^\d{4}-\d{2}-\d{2}$/;

function formatLongDate(day: string, tz: string): string {
  // Day is "YYYY-MM-DD" in the configured tz. Render at noon UTC of that
  // calendar day so the extracted weekday is unambiguous around DST.
  const d = new Date(`${day}T12:00:00Z`);
  if (Number.isNaN(d.getTime())) return day;
  return d.toLocaleDateString(undefined, {
    weekday: "long",
    year: "numeric",
    month: "long",
    day: "numeric",
    timeZone: tz,
  });
}

export default async function TasksPage({
  searchParams,
}: {
  searchParams: Promise<{ day?: string }>;
}) {
  const sp = await searchParams;
  const tz = getPanelTimezone();
  const today = todayForTasks();
  const requestedDay = sp.day && DATE_RE.test(sp.day) ? sp.day : null;

  const days = listTaskDays(); // newest -> oldest
  const newestWithTasks = days[0] ?? null;
  const oldestWithTasks = days[days.length - 1] ?? null;

  // Default to the newest day that has tasks; fall back to today on a fresh
  // install with no tasks yet. Clamp pasted future URLs to today.
  let currentDay = requestedDay ?? newestWithTasks ?? today;
  if (currentDay > today) currentDay = today;

  // Prev = older day with tasks.
  let prevDay: string | null = null;
  for (const d of days) {
    if (d < currentDay) {
      prevDay = d;
      break;
    }
  }

  // Next = newer day with tasks, capped at today.
  let nextDay: string | null = null;
  for (let i = days.length - 1; i >= 0; i--) {
    if (days[i] > currentDay) {
      nextDay = days[i];
      break;
    }
  }
  if (nextDay && nextDay > today) nextDay = null;

  const rows = listTasksForDay(currentDay);
  const isToday = currentDay === today;
  const hasTasksThisDay = rows.length > 0;

  return (
    <div>
      <div className={styles.head}>
        <div>
          <h1 className={styles.title}>Tasks</h1>
          <div className={styles.subtitle}>
            Background operations the agent is running or has run, kept for 30 days.
            Times in {tz.replace("_", " ")}.
          </div>
        </div>
      </div>

      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: 12,
          marginBottom: 14,
          flexWrap: "wrap",
        }}
      >
        <DayNavLink
          href={prevDay ? `/tasks?day=${prevDay}` : null}
          disabled={!prevDay}
          label="← Older"
          tooltip={
            prevDay
              ? `Jump to ${prevDay}`
              : oldestWithTasks
                ? `No tasks before ${oldestWithTasks}`
                : "No tasks"
          }
        />
        <div
          style={{
            display: "flex",
            flexDirection: "column",
            alignItems: "center",
            minWidth: 220,
          }}
        >
          <strong style={{ fontSize: 15 }}>
            {formatLongDate(currentDay, tz)}
          </strong>
          <span className={styles.muted} style={{ fontSize: 12 }}>
            {currentDay}
            {isToday ? " · today" : ""}
            {" · "}
            {hasTasksThisDay
              ? `${rows.length} ${rows.length === 1 ? "task" : "tasks"}`
              : "no tasks"}
          </span>
        </div>
        <DayNavLink
          href={nextDay ? `/tasks?day=${nextDay}` : null}
          disabled={!nextDay}
          label="Newer →"
          tooltip={
            nextDay
              ? `Jump to ${nextDay}`
              : isToday
                ? "Already on today"
                : "No newer tasks"
          }
        />
        <div style={{ flex: 1 }} />
        {!isToday && (
          <Link href="/tasks" className={styles.btnGhost}>
            Today
          </Link>
        )}
      </div>

      {hasTasksThisDay ? (
        <Table
          scrollX
          columns={[
            { key: "kind", label: "Kind" },
            { key: "status", label: "Status", width: "120px" },
            { key: "started", label: "Started", width: "180px" },
            { key: "finished", label: "Finished", width: "180px" },
          ]}
          rows={rows.map((t) => ({
            kind: (
              <Link
                href={`/tasks/${encodeURIComponent(t.id)}`}
                className={styles.code}
              >
                {t.kind}
              </Link>
            ),
            status: <StatusBadge text={t.status} />,
            started: (
              <span className={styles.muted}>{formatTaskTs(t.started_at)}</span>
            ),
            finished: (
              <span className={styles.muted}>
                {formatTaskTs(t.finished_at)}
              </span>
            ),
          }))}
          empty="No tasks."
        />
      ) : (
        <div className={styles.empty}>
          No tasks for {currentDay}. Use <strong>← Older</strong> to jump back
          to the previous day that has activity.
        </div>
      )}
    </div>
  );
}

function DayNavLink({
  href,
  disabled,
  label,
  tooltip,
}: {
  href: string | null;
  disabled: boolean;
  label: string;
  tooltip: string;
}) {
  if (disabled || !href) {
    return (
      <span
        className={styles.btnGhost}
        title={tooltip}
        style={{
          opacity: 0.4,
          cursor: "not-allowed",
        }}
      >
        {label}
      </span>
    );
  }
  return (
    <Link href={href} className={styles.btnGhost} title={tooltip}>
      {label}
    </Link>
  );
}
