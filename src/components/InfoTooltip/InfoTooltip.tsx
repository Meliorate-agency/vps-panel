"use client";

import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import styles from "./InfoTooltip.module.css";

type Props = {
  /** Tooltip body. Plain text — keep it under ~200 chars for readability. */
  text: string;
  /** Optional aria label override; defaults to "More info". */
  label?: string;
};

/** Below this distance from the top of the viewport the bubble opens below
 *  the icon: there is no room for it above. */
const FLIP_BELOW_PX = 140;
/** Half the bubble's max width plus a margin, so it stays on screen. */
const EDGE_PX = 150;

type Pos = { x: number; y: number; below: boolean; arrowDx: number };

/**
 * Inline info icon (ⓘ) with a hover/focus tooltip. The bubble is portaled to
 * <body> at fixed coordinates, so a table card that scrolls sideways (and
 * clips whatever sticks out of it) cannot cut it off. It closes on any
 * scroll, since fixed coordinates would no longer match the icon.
 */
export default function InfoTooltip({ text, label = "More info" }: Props) {
  const ref = useRef<HTMLSpanElement>(null);
  const [pos, setPos] = useState<Pos | null>(null);

  const show = () => {
    const r = ref.current?.getBoundingClientRect();
    if (!r) return;
    const iconX = r.left + r.width / 2;
    const x = Math.min(Math.max(iconX, EDGE_PX), window.innerWidth - EDGE_PX);
    const below = r.top < FLIP_BELOW_PX;
    setPos({ x, y: below ? r.bottom : r.top, below, arrowDx: iconX - x });
  };
  const hide = () => setPos(null);

  const open = pos !== null;
  useEffect(() => {
    if (!open) return;
    const close = () => setPos(null);
    window.addEventListener("scroll", close, true);
    return () => window.removeEventListener("scroll", close, true);
  }, [open]);

  return (
    <span
      ref={ref}
      className={styles.wrap}
      role="img"
      aria-label={label}
      tabIndex={0}
      onMouseEnter={show}
      onMouseLeave={hide}
      onFocus={show}
      onBlur={hide}
    >
      <span className={styles.icon} aria-hidden="true">
        i
      </span>
      {pos &&
        createPortal(
          <span
            role="tooltip"
            className={`${styles.tooltip} ${pos.below ? styles.below : ""}`}
            style={
              {
                left: pos.x,
                top: pos.y,
                "--arrow-dx": `${pos.arrowDx}px`,
              } as React.CSSProperties
            }
          >
            {text}
          </span>,
          document.body,
        )}
    </span>
  );
}
