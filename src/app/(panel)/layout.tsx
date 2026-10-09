import { redirect } from "next/navigation";
import { REMEMBER_TTL_SECONDS, requireSession, resolveSessionTtl } from "@/lib/auth";
import { getSetting } from "@/lib/settings";
import Sidebar from "@/components/Sidebar/Sidebar";
import Topbar from "@/components/Topbar/Topbar";
import RefreshKeepAlive from "@/components/RefreshKeepAlive/RefreshKeepAlive";
import styles from "./layout.module.css";

function safeTimezone(raw: string): string {
  try {
    new Intl.DateTimeFormat("en", { timeZone: raw });
    return raw;
  } catch {
    return "UTC";
  }
}

export default async function PanelLayout({ children }: { children: React.ReactNode }) {
  const s = await requireSession();
  if (getSetting("panel.setup_complete", "false") !== "true") {
    redirect("/setup");
  }
  const timezone = safeTimezone(getSetting("panel.timezone", "UTC"));
  const ttlSeconds = s.remember ? REMEMBER_TTL_SECONDS : resolveSessionTtl().accessSeconds;
  return (
    <div className={styles.shell}>
      <RefreshKeepAlive ttlSeconds={ttlSeconds} />
      <Sidebar />
      <div className={styles.main}>
        <Topbar email={s.email} timezone={timezone} />
        <main className={styles.content}>{children}</main>
      </div>
    </div>
  );
}
