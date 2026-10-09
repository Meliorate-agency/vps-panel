import styles from "./InfoTooltip.module.css";

type Props = {
  /** Tooltip body. Plain text — keep it under ~200 chars for readability. */
  text: string;
  /** Optional aria label override; defaults to "More info". */
  label?: string;
};

/**
 * Inline info icon (ⓘ) with a CSS-only hover tooltip. No client-side JS —
 * safe to use inside server components. Tooltip is anchored above the icon
 * and centered; clipped to viewport via `max-width` + `overflow-wrap`.
 */
export default function InfoTooltip({ text, label = "More info" }: Props) {
  return (
    <span className={styles.wrap} role="img" aria-label={label}>
      <span className={styles.icon} aria-hidden="true">
        i
      </span>
      <span className={styles.tooltip} role="tooltip">
        {text}
      </span>
    </span>
  );
}
