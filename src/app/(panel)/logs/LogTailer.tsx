"use client";

import { useEffect, useRef, useState } from "react";
import styles from "@/components/page.module.css";
import local from "./logs.module.css";

export default function LogTailer({
  units,
  initial,
}: {
  units: string[];
  initial?: string;
}) {
  const [unit, setUnit] = useState<string>(initial || units[0] || "");
  const [lines, setLines] = useState<string[]>([]);
  const boxRef = useRef<HTMLPreElement>(null);

  useEffect(() => {
    if (!unit) return;
    setLines([]);
    const es = new EventSource(`/api/logs/${encodeURIComponent(unit)}/stream`);
    es.addEventListener("line", (e) => {
      setLines((prev) => [...prev.slice(-2000), (e as MessageEvent).data]);
    });
    es.onerror = () => es.close();
    return () => es.close();
  }, [unit]);

  useEffect(() => {
    boxRef.current?.scrollTo(0, boxRef.current.scrollHeight);
  }, [lines]);

  return (
    <div>
      <div className={local.controls}>
        <select
          className={[styles.select, local.unitPicker].join(" ")}
          value={unit}
          onChange={(e) => setUnit(e.target.value)}
        >
          <option value="">Pick a unit...</option>
          {units.map((u) => (
            <option key={u} value={u}>{u}</option>
          ))}
        </select>
        <button className={styles.btnGhost} onClick={() => setLines([])}>Clear</button>
      </div>
      <pre ref={boxRef} className={local.box}>
        {lines.join("\n") || "(waiting for log lines...)"}
      </pre>
    </div>
  );
}
