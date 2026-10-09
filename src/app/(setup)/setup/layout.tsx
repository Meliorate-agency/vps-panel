import { redirect } from "next/navigation";
import { requireSession } from "@/lib/auth";
import { getSetting } from "@/lib/settings";
import styles from "./setup.module.css";

export default async function SetupLayout({ children }: { children: React.ReactNode }) {
  await requireSession();
  if (getSetting("panel.setup_complete", "false") === "true") {
    redirect("/dashboard");
  }
  return (
    <div className={styles.shell}>
      <main className={styles.main}>{children}</main>
    </div>
  );
}
