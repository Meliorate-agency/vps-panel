"use client";

import { useState, useTransition } from "react";
import styles from "@/components/page.module.css";

type Result = { ok: true } | { ok: false; error: string };

function isValidIpOrCidr(s: string): boolean {
  const trimmed = s.trim();
  if (!trimmed) return false;
  const v4 = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})(?:\/(\d{1,2}))?$/;
  const m = trimmed.match(v4);
  if (m) {
    const octets = [m[1], m[2], m[3], m[4]].map(Number);
    if (octets.some((o) => o > 255)) return false;
    if (m[5] !== undefined) {
      const prefix = Number(m[5]);
      if (prefix < 0 || prefix > 32) return false;
    }
    return true;
  }
  if (!trimmed.includes(":")) return false;
  const cidrMatch = trimmed.match(/\/(\d+)$/);
  if (cidrMatch && Number(cidrMatch[1]) > 128) return false;
  const body = trimmed.replace(/\/\d+$/, "");
  return /^[0-9a-fA-F:]+$/.test(body) && body.includes(":");
}

export default function IpAllowlistForm({
  value,
  action,
}: {
  value: string;
  action: (next: string) => Promise<Result>;
}) {
  const [text, setText] = useState(value);
  const [pending, start] = useTransition();
  const [msg, setMsg] = useState<Result | null>(null);
  const [validationError, setValidationError] = useState<string | null>(null);

  function handleSave() {
    setMsg(null);
    setValidationError(null);
    const entries = text
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean);
    const invalid = entries.find((e) => !isValidIpOrCidr(e));
    if (invalid) {
      setValidationError(`Not a valid IP or CIDR: ${invalid}`);
      return;
    }
    start(async () => {
      const r = await action(text);
      setMsg(r);
    });
  }

  return (
    <div style={{ display: "flex", gap: 8, alignItems: "flex-start", flexWrap: "wrap" }}>
      <textarea
        value={text}
        onChange={(e) => setText(e.target.value)}
        rows={3}
        className={styles.textarea}
        placeholder="203.0.113.0/24, 198.51.100.42"
        style={{ flex: 1, minWidth: 280 }}
      />
      <button className={styles.btn} onClick={handleSave} disabled={pending}>
        {pending ? "..." : "Save"}
      </button>
      {validationError && <div className={styles.error}>{validationError}</div>}
      {msg && msg.ok === false && <div className={styles.error}>{msg.error}</div>}
      {msg && msg.ok === true && <div className={styles.notice}>Saved.</div>}
    </div>
  );
}
