import { useId } from "react";
import styles from "./BoltMark.module.css";

interface BoltMarkProps {
  size?: number;
  className?: string;
}

export default function BoltMark({ size = 24, className }: BoltMarkProps) {
  const gradientId = useId();
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      xmlns="http://www.w3.org/2000/svg"
      className={[styles.bolt, className].filter(Boolean).join(" ")}
      aria-hidden="true"
    >
      <defs>
        <linearGradient id={gradientId} x1="0%" y1="0%" x2="100%" y2="100%">
          <stop offset="0%" stopColor="var(--accent-glow)" />
          <stop offset="50%" stopColor="var(--accent)" />
          <stop offset="100%" stopColor="var(--accent-2)" />
        </linearGradient>
      </defs>
      <path d="M13 2L3 14h7l-1 8 11-12h-7l1-8z" fill={`url(#${gradientId})`} />
    </svg>
  );
}
