import type { ShellStateSummary } from "./types";

/**
 * Returns target-derived presentation text without changing the raw path used
 * for daemon operations. Older daemons omit `cwd_display`, so their raw value
 * remains the compatibility fallback.
 */
export function displayWorkingDirectory(
  shellState: Pick<ShellStateSummary, "cwd" | "cwd_display">,
): string | null {
  return shellState.cwd_display ?? shellState.cwd;
}
