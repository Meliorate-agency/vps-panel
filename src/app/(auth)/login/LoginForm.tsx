"use client";

import { useState } from "react";
import type { ActionResult } from "@/server-actions/auth";
import { useFormActionState } from "@/lib/useFormActionState";
import PasswordInput from "@/components/PasswordInput/PasswordInput";
import styles from "./page.module.css";

type Props = {
  action: (prev: ActionResult | null, fd: FormData) => Promise<ActionResult>;
};

export default function LoginForm({ action }: Props) {
  // A refused sign-in keeps the email and the Remember me box as typed.
  const [state, formAction, pending, formRef] = useFormActionState<ActionResult | null>(action, null);
  const [showPassword, setShowPassword] = useState(false);

  return (
    <form ref={formRef} action={formAction} className={styles.form}>
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
