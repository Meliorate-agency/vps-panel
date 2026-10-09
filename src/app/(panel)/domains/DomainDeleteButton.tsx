"use client";

import { useState, useTransition } from "react";
import styles from "@/components/page.module.css";
import ConfirmDialog from "@/components/ConfirmDialog/ConfirmDialog";

/**
 * Cascade-delete button for a panel-managed domain. Opens the styled
 * ConfirmDialog listing exactly what will be wiped (DNS zone + all records,
 * apex nginx vhost, sub-site vhosts) and what will be left intact
 * (mailboxes, DKIM keys, certs).
 *
 * Used on:
 *   - /domains list view (apex rows where the panel manages the zone)
 *   - /domains/<zone> detail page (replacing the old window.confirm flow)
 *
 * `subSiteCount` lets the dialog list how many sub-sites the cascade will
 * pull down — purely informational. The action itself re-queries SQLite at
 * delete time so a stale UI count never causes wrong behaviour.
 */
export default function DomainDeleteButton({
  zone,
  action,
  subSiteCount = 0,
  buttonLabel = "Delete",
  buttonClassName,
}: {
  zone: string;
  action: (zone: string) => Promise<void>;
  subSiteCount?: number;
  buttonLabel?: string;
  buttonClassName?: string;
}) {
  const [open, setOpen] = useState(false);
  const [pending, start] = useTransition();

  const onConfirm = () => {
    start(async () => {
      await action(zone);
      setOpen(false);
    });
  };

  return (
    <>
      <button
        type="button"
        className={buttonClassName ?? styles.btnDanger}
        onClick={() => setOpen(true)}
        disabled={pending}
      >
        {pending ? "Deleting…" : buttonLabel}
      </button>

      <ConfirmDialog
        open={open}
        title={`Delete ${zone} and everything under it?`}
        confirmLabel={pending ? "Deleting…" : "Delete everything"}
        cancelLabel="Cancel"
        tone="danger"
        pending={pending}
        onConfirm={onConfirm}
        onCancel={() => {
          if (pending) return;
          setOpen(false);
        }}
      >
        <p>This will permanently remove:</p>
        <ul>
          <li>
            PowerDNS zone <span className={styles.code}>{zone}</span> and{" "}
            <strong>every record inside it</strong> (apex, www, mail, ns1, ns2,
            MX, SPF, DMARC, DKIM, plus any subdomains you added).
          </li>
          <li>
            The nginx vhost for <span className={styles.code}>{zone}</span>.
          </li>
          {subSiteCount > 0 && (
            <li>
              <strong>
                {subSiteCount} sub-site
                {subSiteCount === 1 ? "" : "s"}
              </strong>{" "}
              under <span className={styles.code}>{zone}</span> (their nginx
              vhosts and panel SQLite rows).
            </li>
          )}
        </ul>
        <p>What stays intact:</p>
        <ul>
          <li>
            Mailbox data and credentials under{" "}
            <span className={styles.code}>/var/vmail/{zone}/</span> and the
            virtual_users table. Mail won&apos;t deliver without DNS, but the
            archive survives if you re-add the domain later.
          </li>
          <li>
            DKIM private key at{" "}
            <span className={styles.code}>/var/lib/rspamd/dkim/{zone}.default.key</span>.
          </li>
          <li>
            Any Let&apos;s Encrypt cert at{" "}
            <span className={styles.code}>/etc/vps-panel/certs/{zone}/</span> —
            reused automatically if you re-add the domain.
          </li>
          <li>
            Apps. If an app still routes through this domain or one of its
            subdomains, nothing is deleted: remove the domain on the app&apos;s
            page first.
          </li>
        </ul>
        <div
          style={{
            marginTop: 12,
            padding: "8px 10px",
            background: "rgba(255, 80, 80, 0.08)",
            border: "1px solid rgba(255, 80, 80, 0.32)",
            borderRadius: 6,
            fontSize: 12,
          }}
        >
          This action cannot be undone from the UI. You can re-add the domain
          from <span className={styles.code}>/domains</span> afterwards, but
          the DNS zone&apos;s SOA serial and any custom records you added
          manually will need to be recreated.
        </div>
      </ConfirmDialog>
    </>
  );
}
