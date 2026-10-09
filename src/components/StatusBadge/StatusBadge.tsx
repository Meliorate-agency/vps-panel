import styles from "./StatusBadge.module.css";

type Tone = "ok" | "warn" | "danger" | "muted" | "info";

const TONE_BY_KEYWORD: Record<string, Tone> = {
  active: "ok",
  running: "info",
  succeeded: "ok",
  enabled: "ok",
  reloading: "info",
  activating: "info",
  pending: "warn",
  inactive: "muted",
  dead: "muted",
  disabled: "muted",
  failed: "danger",
  error: "danger",
};

export default function StatusBadge({
  text,
  tone,
}: {
  text: string;
  tone?: Tone;
}) {
  const t = tone ?? TONE_BY_KEYWORD[text.toLowerCase()] ?? "muted";
  return <span className={`${styles.badge} ${styles[t]}`}>{text}</span>;
}
