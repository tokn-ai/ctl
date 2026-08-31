export const DAEMON_RESTART_UNSUPPORTED = "daemon_restart_unsupported";

/**
 * A daemon that rejects restart during preflight has not detached the GUI or
 * changed any session. Every other failure is treated as potentially
 * destructive, because it may happen after session termination begins.
 */
export function restartFailurePreservesLocalState(
  code: string | null,
): boolean {
  return code === DAEMON_RESTART_UNSUPPORTED;
}
