import { redirect } from "next/navigation";
import { requireSession } from "@/lib/auth";
import { getSetting } from "@/lib/settings";
import styles from "./setup.module.css";

export default async function SetupLayout({ children }: { children: React.ReactNode }) {
  const s = await requireSession();
  if (getSetting("panel.setup_complete", "false") === "true") {
    redirect("/dashboard");
  }
  // The wizard reads the SSH server settings and key counts and its steps
  // are admin actions: nothing here is for a user account.
  if (s.role !== "admin") {
    return (
      <div className={styles.shell}>
        <main className={styles.main}>
          <p>Setup has not been finished yet. An admin has to sign in and complete it.</p>
        </main>
      </div>
    );
  }
  return (
    <div className={styles.shell}>
      <main className={styles.main}>{children}</main>
    </div>
  );
}
