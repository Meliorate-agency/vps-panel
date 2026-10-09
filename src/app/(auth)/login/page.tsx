import { loginAction } from "@/server-actions/auth";
import BoltMark from "@/components/BoltMark/BoltMark";
import LoginForm from "./LoginForm";
import styles from "./page.module.css";

export const metadata = { title: "Sign in" };

// The root layout reads the operator's accent from panel.db to build the
// palette. Without this, /login would be prerendered at build time and keep
// serving whatever accent was set when the panel was last built.
export const dynamic = "force-dynamic";

export default function LoginPage() {
  return (
    <div className={styles.shell}>
      <div className={styles.card}>
        <div className={styles.brand}>
          <BoltMark size={48} />
          <h1 className={styles.title}>Bolt Panel</h1>
        </div>
        <p className={styles.subtitle}>Sign in to manage your server.</p>
        <LoginForm action={loginAction} />
      </div>
    </div>
  );
}
