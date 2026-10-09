import { NextResponse } from "next/server";
import { getSession } from "@/lib/auth";
import { agent } from "@/lib/agent";

export const dynamic = "force-dynamic";
export const runtime = "nodejs";

export async function GET() {
  const session = await getSession();
  if (!session || session.needsTotp)
    return new Response("unauthorized", { status: 401 });
  try {
    const rows = await agent.system.services();
    return NextResponse.json(rows);
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    return new Response(msg, { status: 502 });
  }
}
