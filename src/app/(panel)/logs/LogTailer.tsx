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
  // The stream closed (agent restarted, unit refused): said so instead of
  // the box sitting on its last line as if it were still live.
  const [ended, setEnded] = useState(false);
  // Bumped by Reconnect to open the stream again.
  const [attempt, setAttempt] = useState(0);
  const boxRef = useRef<HTMLPreElement>(null);
  // Follow new lines only while the box is scrolled to its end, so reading
  // back through a busy unit is not yanked down by every new line.
  const atBottom = useRef(true);

  useEffect(() => {
    if (!unit) return;
    setLines([]);
    setEnded(false);
    atBottom.current = true;
    const es = new EventSource(`/api/logs/${encodeURIComponent(unit)}/stream`);
    es.addEventListener("line", (e) => {
      setLines((prev) => [...prev.slice(-2000), (e as MessageEvent).data]);
    });
    es.onerror = () => {
      es.close();
      setEnded(true);
    };
    return () => es.close();
  }, [unit, attempt]);

  useEffect(() => {
    if (atBottom.current) boxRef.current?.scrollTo(0, boxRef.current.scrollHeight);
  }, [lines, ended]);

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
        {ended && unit && (
          <button className={styles.btnGhost} onClick={() => setAttempt((n) => n + 1)}>
            Reconnect
          </button>
        )}
      </div>
      <pre
        ref={boxRef}
        className={local.box}
        onScroll={(e) => {
          const el = e.currentTarget;
          atBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
        }}
      >
        {lines.join("\n") || (ended ? "" : "(waiting for log lines...)")}
        {ended && `${lines.length > 0 ? "\n" : ""}(the log stream ended)`}
      </pre>
    </div>
  );
}
