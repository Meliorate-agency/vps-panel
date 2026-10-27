"use client";

import { useActionState, useEffect, useRef, useState } from "react";
import styles from "@/components/page.module.css";
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
  const [state, formAction, pending] = useActionState<Result | null, FormData>(action, null);
  const [channel, setChannel] = useState("email");
  const formRef = useRef<HTMLFormElement>(null);
  const channelRef = useRef<HTMLSelectElement>(null);
  // React 19 resets the form after the action; the <select> DOM snaps back
  // to "email" while `channel` (and the fields it shows) stays. Re-apply it
  // once the reset has landed, as DeployForm does.
  useEffect(() => {
    const form = formRef.current;
    if (!form) return;
    const onReset = () => {
      queueMicrotask(() => {
        if (channelRef.current) channelRef.current.value = channel;
      });
    };
    form.addEventListener("reset", onReset);
    return () => form.removeEventListener("reset", onReset);
  }, [channel]);
  return (
    <form ref={formRef} action={formAction}>
      <div className={styles.formRow}>
        <label className={styles.field}>
          Channel
          <select
            ref={channelRef}
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
      <fieldset style={{ border: "none", padding: 0, margin: "0 0 12px" }}>
        <legend className={styles.muted} style={{ fontSize: 12, marginBottom: 6 }}>
          Events (none selected = all)
        </legend>
        <div style={{ display: "flex", flexWrap: "wrap", gap: "6px 16px" }}>
          {NOTIFICATION_EVENTS.map((e) => (
            <label key={e.id} style={{ display: "flex", gap: 6, alignItems: "center", fontSize: 13 }}>
              <input type="checkbox" name="events" value={e.id} />
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
