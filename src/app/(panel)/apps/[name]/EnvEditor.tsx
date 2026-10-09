"use client";

import { useActionState } from "react";
import styles from "@/components/page.module.css";

type Result = { ok: true } | { ok: false; error: string };

type Props = {
  name: string;
  action: (fd: FormData) => Promise<Result>;
};

export default function EnvEditor({ action }: Props) {
  const wrapped = async (_prev: Result | null, fd: FormData): Promise<Result> => action(fd);
  const [state, formAction, pending] = useActionState<Result | null, FormData>(wrapped, null);
  return (
    <form action={formAction}>
      <textarea
        name="env"
        className={styles.textarea}
        rows={10}
        placeholder="DATABASE_URL=mysql://...&#10;NODE_ENV=production"
        style={{
          // Full container width + only-vertical resize so the button below
          // never gets pushed out of view by an over-eager horizontal drag.
          width: "100%",
          boxSizing: "border-box",
          display: "block",
          resize: "vertical",
        }}
      />
      {state?.ok === false && (
        <div className={styles.error} style={{ marginTop: 10 }}>
          {state.error}
        </div>
      )}
      {state?.ok === true && (
        <div className={styles.notice} style={{ marginTop: 10 }}>
          Saved. App restarted.
        </div>
      )}
      <div
        style={{
          display: "flex",
          justifyContent: "flex-end",
          marginTop: 12,
        }}
      >
        <button className={styles.btn} type="submit" disabled={pending}>
          {pending ? "Saving..." : "Save and restart"}
        </button>
      </div>
    </form>
  );
}
