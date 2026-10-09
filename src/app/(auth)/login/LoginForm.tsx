"use client";

import { useActionState, useState } from "react";
import type { ActionResult } from "@/server-actions/auth";
import PasswordInput from "@/components/PasswordInput/PasswordInput";
import styles from "./page.module.css";

type Props = {
  action: (prev: ActionResult | null, fd: FormData) => Promise<ActionResult>;
};

export default function LoginForm({ action }: Props) {
  const [state, formAction, pending] = useActionState<ActionResult | null, FormData>(action, null);
  const [showPassword, setShowPassword] = useState(false);

  return (
    <form action={formAction} className={styles.form}>
      <label className={styles.label}>
        Email
        <input
          name="email"
          type="email"
          autoComplete="username"
          required
          autoFocus
          className={styles.input}
        />
      </label>
      <label className={styles.label}>
        Password
        <PasswordInput
          name="password"
          autoComplete="current-password"
          visible={showPassword}
          onToggle={() => setShowPassword((v) => !v)}
          className={styles.input}
        />
      </label>
      <label className={styles.rememberRow}>
        <input
          type="checkbox"
          name="remember"
          className={styles.checkbox}
          defaultChecked={false}
        />
        <span>Remember me for 30 days</span>
      </label>
      {state && state.ok === false && (
        <div className={styles.error}>{state.error}</div>
      )}
      <button type="submit" className={styles.submit} disabled={pending}>
        {pending ? "Signing in..." : "Sign in"}
      </button>
    </form>
  );
}
