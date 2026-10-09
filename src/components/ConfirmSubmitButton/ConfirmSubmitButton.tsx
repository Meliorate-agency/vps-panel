"use client";

import { useFormStatus } from "react-dom";

type Props = {
  message: string;
  children: React.ReactNode;
  className?: string;
  pendingLabel?: string;
  name?: string;
  value?: string;
};

export default function ConfirmSubmitButton({
  message,
  children,
  className,
  pendingLabel,
  name,
  value,
}: Props) {
  const { pending } = useFormStatus();
  return (
    <button
      type="submit"
      name={name}
      value={value}
      className={className}
      disabled={pending}
      aria-busy={pending}
      onClick={(e) => {
        if (!window.confirm(message)) {
          e.preventDefault();
        }
      }}
    >
      {pending ? (pendingLabel ?? "...") : children}
    </button>
  );
}
