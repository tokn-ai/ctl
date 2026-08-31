import type { SessionSummary, ShellStateSummary } from "../../lib/types";

export interface TerminalTitle {
  path: string;
  command: string | null;
  text: string;
}

function displayShell(shellState: ShellStateSummary): string | null {
  if (shellState.prompt_phase === "running") {
    const command = shellState.running_command?.trim();
    if (command) {
      return command;
    }
  }
  return shellState.shell_type === "unknown" ? null : shellState.shell_type;
}

/**
 * Formats the window and tab title from a daemon-observed shell snapshot.
 * A session name is only a temporary fallback while no path has been observed.
 */
export function formatTerminalTitle(
  session: Pick<SessionSummary, "name"> | null,
  shellState: ShellStateSummary | null,
): TerminalTitle {
  const path = shellState?.cwd || session?.name || "rmux";
  const command = shellState ? displayShell(shellState) : null;

  return {
    path,
    command,
    text: command ? `${path} — ${command}` : path,
  };
}
