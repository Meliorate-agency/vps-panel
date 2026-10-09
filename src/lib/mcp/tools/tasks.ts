import "server-only";
import { agent } from "@/lib/agent";
import { requireString, type Tool } from "../types";

export const TOOLS: Tool[] = [
  {
    page: "Tasks",
    name: "tasks.list",
    description:
      "List the newest 200 background tasks (id, kind, status, start/finish; kept 30 days). Logs are omitted: use tasks.get for one task's log.",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    call: async () => agent.tasks.list(),
  },
  {
    page: "Tasks",
    name: "tasks.get",
    description:
      "Fetch one task's full state including its complete log. Use after a tool that returns a task_id (apps.update, backups.run, apps.create with step≠deploy) to follow progress.",
    inputSchema: {
      type: "object",
      properties: { id: { type: "string", description: "Task id (from any tool that returned task_id)." } },
      required: ["id"],
      additionalProperties: false,
    },
    call: async (args) => agent.tasks.get(requireString(args, "id")),
  },
];
