"use client";

import { useActionState } from "react";
import shared from "@/components/page.module.css";
import type { ActionResult } from "@/server-actions/domains";

type Variant = "enable" | "adopt";

type Props = {
  domain: string;
  action: (prev: ActionResult | null, fd: FormData) => Promise<ActionResult>;
  variant?: Variant;
};

const COPY: Record<Variant, { body: string; idle: string; pending: string }> = {
  enable: {
    body: "DNS for this domain is not managed by the panel.",
    idle: "Enable DNS management",
    pending: "Enabling...",
  },
  adopt: {
    body: "An existing PowerDNS zone is present but not yet managed by the panel.",
    idle: "Adopt zone",
    pending: "Adopting...",
  },
};

export default function EnableDnsForm({ domain, action, variant = "enable" }: Props) {
  const [state, formAction, pending] = useActionState<ActionResult | null, FormData>(action, null);
  const copy = COPY[variant];
  return (
    <div>
      <div className={shared.muted} style={{ marginBottom: 10 }}>
        {copy.body}
      </div>
      <form action={formAction}>
        <input type="hidden" name="zone" value={domain} />
        <button type="submit" className={shared.btnGhost} disabled={pending}>
          {pending ? copy.pending : copy.idle}
        </button>
      </form>
      {state?.ok === false && (
        <div className={shared.error} style={{ marginTop: 8 }}>{state.error}</div>
      )}
    </div>
  );
}
