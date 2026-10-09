"use client";

import { useState, useTransition } from "react";
import type { ReconcileZoneResp } from "@/lib/agent";
import styles from "@/components/page.module.css";

type Result =
  | { ok: true; data?: ReconcileZoneResp }
  | { ok: false; error: string };

export default function ReconcileButton({
  zone,
  action,
}: {
  zone: string;
  action: (zone: string) => Promise<Result>;
}) {
  const [pending, start] = useTransition();
  const [result, setResult] = useState<Result | null>(null);

  return (
    <div style={{ display: "inline-block" }}>
      <button
        type="button"
        className={styles.btnGhost}
        disabled={pending}
        onClick={() =>
          start(async () => {
            const r = await action(zone);
            setResult(r);
          })
        }
      >
        {pending ? "Reconciling..." : "Reconcile zone"}
      </button>
      {result?.ok === false && (
        <div className={styles.error} style={{ marginTop: 8 }}>
          {result.error}
        </div>
      )}
      {result?.ok === true && result.data && (
        <ReconcileSummary data={result.data} />
      )}
    </div>
  );
}

function ReconcileSummary({ data }: { data: ReconcileZoneResp }) {
  return (
    <div className={styles.notice} style={{ marginTop: 8 }}>
      <div>
        <strong>+{data.added.length}</strong> added,{" "}
        <strong>{data.updated.length}</strong> updated,{" "}
        <strong>{data.unchanged.length}</strong> unchanged
        {data.non_standard_extras.length > 0 && (
          <>
            , <strong>{data.non_standard_extras.length}</strong> non-standard
            extra{data.non_standard_extras.length === 1 ? "" : "s"}
          </>
        )}
        .
      </div>
      {data.added.length > 0 && <ChangeList label="Added" items={data.added} />}
      {data.updated.length > 0 && (
        <ChangeList label="Updated" items={data.updated} />
      )}
      {data.non_standard_extras.length > 0 && (
        <ChangeList
          label="Non-standard extras (left in place)"
          items={data.non_standard_extras}
        />
      )}
    </div>
  );
}

function ChangeList({ label, items }: { label: string; items: string[] }) {
  return (
    <details style={{ marginTop: 6 }}>
      <summary>{label}</summary>
      <ul style={{ margin: "6px 0 0 20px" }}>
        {items.map((item) => (
          <li key={item}>
            <span className={styles.code}>{item}</span>
          </li>
        ))}
      </ul>
    </details>
  );
}
