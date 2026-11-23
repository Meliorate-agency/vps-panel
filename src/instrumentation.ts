/**
 * Runs once when the panel's server starts. Starts the My Networks
 * heartbeat (every 10 s this server pings the others in its networks) and
 * the daily clean-up of expired sessions and old login attempts. Not
 * during `next build`, and only in the Node.js runtime.
 */
export async function register() {
  if (process.env.NEXT_RUNTIME !== "nodejs" || process.env.NEXT_PHASE === "phase-production-build") return;
  const { startHeartbeat } = await import("./lib/networks/heartbeat");
  startHeartbeat();
  const { startHousekeeping } = await import("./lib/housekeeping");
  startHousekeeping();
}
