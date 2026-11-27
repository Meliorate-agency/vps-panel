"use server";

import { agent } from "@/lib/agent";
import { requireSession } from "@/lib/auth";

export async function fetchLoadOne(): Promise<number> {
  await requireSession();
  const info = await agent.sysinfo().catch(() => null);
  return info?.load_1 ?? 0;
}

export async function fetchMemUsedPct(): Promise<number> {
  await requireSession();
  const info = await agent.sysinfo().catch(() => null);
  if (!info || !info.mem_total_kb) return 0;
  return ((info.mem_total_kb - info.mem_available_kb) / info.mem_total_kb) * 100;
}
