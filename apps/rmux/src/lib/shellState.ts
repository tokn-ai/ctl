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

export function terminalPaneTitle(state: ShellStateSummary | null | undefined): string {
  const activity = state?.running_command?.trim() ||
    (state?.shell_type && state.shell_type !== "unknown" ? state.shell_type : "Terminal");
  const directory = state ? displayWorkingDirectory(state) : null;
  return directory ? `${activity} · ${directory}` : activity;
}
