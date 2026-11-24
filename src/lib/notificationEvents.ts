/** Events the agent sends (agent/src/notify.rs, EVENTS: a test there checks
 *  that this list has the same ids; the network.* ones come from the panel's
 *  My Networks heartbeat through the agent). Each label says when it fires.
 *  A channel with none selected gets all of them, including events added by
 *  later updates. */
export const NOTIFICATION_EVENTS = [
  {
    id: "ssl.fail",
    label: "SSL: a certificate order or renewal failed, or a certificate in use expires within 14 days",
  },
  { id: "backup.fail", label: "Backup: a backup or restore failed" },
  { id: "deploy.fail", label: "Apps: a deploy, update or build recovery failed" },
  {
    id: "task.fail",
    label:
      "Other tasks: any other task failed (panel update, package upgrade, install, import), or the privacy settings could not be applied",
  },
  {
    id: "service.fail",
    label:
      "Services: a service failed, or one the panel needs (nginx, Postfix, Dovecot, MariaDB, PowerDNS, Rspamd, fail2ban, the panel) has been stopped or disabled for 5 minutes or more",
  },
  { id: "service.ok", label: "Services: a reported service runs again" },
  { id: "disk.full", label: "Disk: a disk is 90% full, or 90% of its inodes are used" },
  { id: "disk.ok", label: "Disk: a reported disk is below 85% again" },
  {
    id: "mail.fail",
    label:
      "Mail: 50 messages wait in the queue or 3 are deferred for over 6 hours, or the outgoing mail settings could not be applied",
  },
  { id: "mail.ok", label: "Mail: a stuck queue has cleared" },
  { id: "memory.oom", label: "Memory: the server ran out of memory and processes were killed (at most hourly)" },
  { id: "network.peer_down", label: "My Networks: a server stopped answering for 1 minute" },
  { id: "network.peer_up", label: "My Networks: a server answers again" },
] as const;
