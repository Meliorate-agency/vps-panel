"use client";

import { useEffect, useState } from "react";
import styles from "./HealthChart.module.css";

type Sample = { ts: number; v: number };

/**
 * Live line for one number, sampled every `intervalMs`.
 *
 * The top of the chart is `scaleMax` when given (memory: 0..100%), else a
 * round number a bit above the highest sample so far. It used to scale to
 * the highest sample exactly, which pinned that sample to the very top: a
 * steady value (memory sitting at 35%) drew a straight line along the top
 * edge and looked like a pegged meter.
 */
export default function HealthChart({
  fetchValue,
  label,
  color = "var(--accent)",
  intervalMs = 2000,
  max = 60,
  scaleMax,
  unit = "",
}: {
  fetchValue: () => Promise<number>;
  label: string;
  color?: string;
  intervalMs?: number;
  /** How many samples to keep. */
  max?: number;
  /** Fixed top of the chart. Left out: a round number above the peak. */
  scaleMax?: number;
  unit?: string;
}) {
  const [samples, setSamples] = useState<Sample[]>([]);

  useEffect(() => {
    let cancelled = false;
    async function tick() {
      try {
        const v = await fetchValue();
        if (cancelled) return;
        setSamples((prev) => {
          const next = [...prev, { ts: Date.now(), v }];
          return next.slice(-max);
        });
      } catch {
        // skip
      }
    }
    tick();
    const id = setInterval(tick, intervalMs);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [fetchValue, intervalMs, max]);

  const w = 600;
  const h = 80;
  const top = scaleMax ?? niceMax(Math.max(...samples.map((s) => s.v), 0));
  const y = (v: number) => h - (Math.min(v, top) / top) * (h - 4) - 2;
  const points = samples
    .map((s, i) => `${((i / Math.max(samples.length - 1, 1)) * w).toFixed(1)},${y(s.v).toFixed(1)}`)
    .join(" ");

  const last = samples.at(-1)?.v ?? 0;

  return (
    <div className={styles.box}>
      <div className={styles.head}>
        <span className={styles.label}>{label}</span>
        <span className={styles.value}>
          {last.toFixed(unit === "%" ? 1 : 2)}
          {unit}
        </span>
      </div>
      <svg viewBox={`0 0 ${w} ${h}`} preserveAspectRatio="none" className={styles.svg}>
        {/* Half-scale line, so the height of the line means something. */}
        <line x1="0" y1={y(top / 2)} x2={w} y2={y(top / 2)} className={styles.grid} />
        {samples.length > 1 && (
          <>
            <polyline
              points={`0,${h} ${points} ${w},${h}`}
              fill={color}
              fillOpacity="0.1"
              stroke="none"
            />
            <polyline
              points={points}
              fill="none"
              stroke={color}
              strokeWidth="1.5"
              strokeLinejoin="round"
            />
          </>
        )}
      </svg>
      <div className={styles.scale}>
        0 to {top.toFixed(top < 10 ? 1 : 0)}
        {unit}
      </div>
    </div>
  );
}

/** 1, 2 or 5 times a power of ten, at or above `v` with a little headroom. */
function niceMax(v: number): number {
  if (!Number.isFinite(v) || v <= 0) return 1;
  const target = v * 1.15;
  const magnitude = 10 ** Math.floor(Math.log10(target));
  for (const step of [1, 2, 5, 10]) {
    if (step * magnitude >= target) return step * magnitude;
  }
  return 10 * magnitude;
}
