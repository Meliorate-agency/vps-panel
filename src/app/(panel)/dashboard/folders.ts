"use server";

import { agent, type FolderSizes } from "@/lib/agent";
import { requireAdmin, requireSession } from "@/lib/auth";

export type FolderSizesResult =
  | { ok: true; sizes: FolderSizes }
  | { ok: false; error: string };

/** Folder sizes on / for the dashboard. A forced re-measure reads the whole
 *  disk, so only an admin may ask for one. */
export async function folderSizesAction(refresh: boolean): Promise<FolderSizesResult> {
  if (refresh) await requireAdmin();
  else await requireSession();
  try {
    return { ok: true, sizes: await agent.folderSizes(refresh) };
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }
}
