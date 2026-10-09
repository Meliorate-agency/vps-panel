"use client";

import { useState } from "react";
import { copyText } from "@/lib/clipboard";
import styles from "@/components/page.module.css";

export default function CopyMailboxConfig({ email, server }: { email: string; server: string }) {
  const [copied, setCopied] = useState(false);

  // Use mail.<domain> derived from the mailbox's email address. The MX/DKIM/SPF
  // setup the panel publishes points every mail domain at its own mail.<domain>
  // A record (which resolves to the same VPS IP). Falls back to the panel host
  // if the email has no @-part for some reason.
  const domain = email.split("@")[1] ?? "";
  const mailHost = domain ? `mail.${domain}` : server;

  async function copy() {
    const text = [
      `Mail server:  ${mailHost}`,
      `IMAP:         ${mailHost}:993 (SSL/TLS)`,
      `SMTP:         ${mailHost}:587 (STARTTLS)`,
      `POP3:         ${mailHost}:995 (SSL/TLS)`,
      `Username:     ${email}`,
    ].join("\n");
    if (!(await copyText(text))) return;
    setCopied(true);
    setTimeout(() => setCopied(false), 1800);
  }

  return (
    <button type="button" className={styles.btnGhost} onClick={copy} style={{ fontSize: 12 }}>
      {copied ? "Copied!" : "Copy config"}
    </button>
  );
}
