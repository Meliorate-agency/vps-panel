"use client";

import { useFormStatus } from "react-dom";
import styles from "./PendingButton.module.css";

type Props = {
  children: React.ReactNode;
  pendingLabel?: string;
  className?: string;
};

export default function PendingButton({
  children,
  pendingLabel,
  className,
}: Props) {
  const { pending } = useFormStatus();
  return (
    <button
      type="submit"
      className={className}
      disabled={pending}
      aria-busy={pending}
    >
      {pending ? (
        <span className={styles.pending}>
          <span className={styles.spinner} aria-hidden />
          {pendingLabel ?? children}
        </span>
      ) : (
        children
      )}
    </button>
  );
}
