"use client";

import { useState } from "react";
import styles from "@/components/page.module.css";
import { useFormActionState } from "@/lib/useFormActionState";
import { NOTIFICATION_EVENTS } from "@/lib/notificationEvents";

type Result = { ok: true } | { ok: false; error: string };

const TARGET_PLACEHOLDER: Record<string, string> = {
  email: "ops@example.com",
  webhook: "https://hooks.slack.com/...",
};

export default function NotificationForm({
  action,
}: {
  action: (prev: Result | null, fd: FormData) => Promise<Result>;
}) {
  // A refused add keeps the fields; an added one clears them, while the
  // channel stays on the last pick.
  const [state, formAction, pending, formRef] = useFormActionState<Result | null>(action, null);
  const [channel, setChannel] = useState("email");
  return (
    <form ref={formRef} action={formAction}>
      <div className={styles.formRow}>
        <label className={styles.field}>
          Channel
          <select
            name="channel"
            className={styles.select}
            value={channel}
            onChange={(e) => setChannel(e.target.value)}
          >
            <option value="email">email</option>
            <option value="webhook">webhook</option>
            <option value="telegram">telegram</option>
          </select>
        </label>
        {channel === "telegram" ? (
          <>
            <label className={styles.field}>
              Bot token
              <input
                name="secret"
                type="password"
                autoComplete="off"
                className={styles.input}
                required
                placeholder="123456789:AAH... from @BotFather"
              />
            </label>
            <label className={styles.field}>
              Your Telegram user ID
              <input
                name="target"
                className={styles.input}
                required
                inputMode="numeric"
                placeholder="123456789"
              />
            </label>
          </>
        ) : (
          <label className={styles.field}>
            Target
            <input
              name="target"
              className={styles.input}
              required
              placeholder={TARGET_PLACEHOLDER[channel]}
            />
          </label>
        )}
      </div>
      {channel === "telegram" && (
        <div className={styles.muted} style={{ fontSize: 12, margin: "-4px 0 12px" }}>
          Make a bot with @BotFather, then open it in Telegram and press Start: a bot can only
          message people who started it. Your numeric user ID comes from @userinfobot. Only
          that ID gets the messages.
        </div>
      )}
      {channel !== "email" && (
        <label style={{ display: "flex", gap: 6, alignItems: "flex-start", fontSize: 13, margin: "0 0 12px" }}>
          {/* Keyed on the channel: switching it starts from off again. */}
          <input key={channel} type="checkbox" name="log_lines" style={{ marginTop: 3 }} />
          <span>
            Include log lines: the last lines of a failed task or a service&apos;s journal. They
            can contain email and IP addresses, which then leave this server. Without them the
            message says what failed and where to look in the panel. Email always has them.
          </span>
        </label>
      )}
      <fieldset style={{ border: "none", padding: 0, margin: "0 0 12px" }}>
        <legend className={styles.muted} style={{ fontSize: 12, marginBottom: 6 }}>
          Events. None selected = all of them, including events later updates add.
        </legend>
        <div style={{ display: "grid", gap: 6 }}>
          {NOTIFICATION_EVENTS.map((e) => (
            <label key={e.id} style={{ display: "flex", gap: 6, alignItems: "flex-start", fontSize: 13 }}>
              <input type="checkbox" name="events" value={e.id} style={{ marginTop: 3 }} />
              {e.label}
            </label>
          ))}
        </div>
      </fieldset>
      {state?.ok === false && <div className={styles.error}>{state.error}</div>}
      <button type="submit" className={styles.btn} disabled={pending}>
        {pending ? "..." : "Add"}
      </button>
    </form>
  );
}
