import { logoutAction } from "@/server-actions/auth";
import HamburgerButton from "./HamburgerButton";
import ServerClock from "./ServerClock";
import UpdateButton from "./UpdateButton";
import styles from "./Topbar.module.css";

type Props = {
  email: string;
  timezone: string;
};

export default function Topbar({ email, timezone }: Props) {
  // Capture the server's wall-clock at render time. The client component
  // computes drift against its own clock so the displayed time tracks the
  // server even when the operator's machine is skewed or in a different TZ.
  const initialServerMs = Date.now();
  return (
    <header className={styles.bar}>
      <HamburgerButton />
      <ServerClock initialServerMs={initialServerMs} timezone={timezone} />
      <div className={styles.spacer} />
      <div className={styles.user}>
        <UpdateButton />
        <span className={styles.email}>{email}</span>
        <form action={logoutAction}>
          <button type="submit" className={styles.signOut}>
            Sign out
          </button>
        </form>
      </div>
    </header>
  );
}
