/** Events the agent reports (agent/src/notify.rs; the network.* ones come
 *  from the panel's My Networks heartbeat through the agent). A channel with
 *  none selected gets all of them. */
export const NOTIFICATION_EVENTS = [
  { id: "ssl.fail", label: "SSL order or renewal failed" },
  { id: "backup.fail", label: "Backup failed" },
  { id: "deploy.fail", label: "App deploy or update failed" },
  { id: "service.fail", label: "A service stopped in the failed state" },
  { id: "disk.full", label: "Root disk 90% full" },
  { id: "task.fail", label: "Any other task failed" },
  { id: "network.peer_down", label: "A server in My Networks stopped answering (1 min)" },
  { id: "network.peer_up", label: "A server in My Networks answers again" },
] as const;
