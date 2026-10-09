"use client";

import { useRouter } from "next/navigation";
import TaskProgress from "@/components/TaskProgress/TaskProgress";

/**
 * Streams a running task and re-renders the page when it ends, so the
 * header badge and finish time show the result instead of staying on
 * "running".
 */
export default function LiveTaskLog({ taskId }: { taskId: string }) {
  const router = useRouter();
  return <TaskProgress taskId={taskId} onComplete={() => router.refresh()} />;
}
