import type { SessionSummary, ShellStateSummary } from "../../lib/types";

/**
 * Native title bars have a finite visual width. Keep this comfortably below a
 * typical title-bar width so the useful end of a path is not hidden by a
 * second, platform-provided truncation.
 */
export const NATIVE_TERMINAL_TITLE_MAX_LENGTH = 80;

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
 * Compacts a native title by omitting its prefix, not its tail.
 *
 * Working-directory prefixes are commonly shared across sessions, whereas the
 * final path components and current command distinguish them. A leading
 * ellipsis makes the omitted prefix explicit while preserving that useful end.
 */
export function compactTerminalTitle(
  text: string,
  maxLength = NATIVE_TERMINAL_TITLE_MAX_LENGTH,
): string {
  const characters = Array.from(text);
  if (characters.length <= maxLength) {
    return text;
  }

  if (maxLength <= 1) {
    return "…";
  }

  return `…${characters.slice(-(maxLength - 1)).join("")}`;
}

/**
 * Compacts a formatted title while retaining both its path and command
 * portions whenever there is room. Compacting the already-combined text can
 * let a long command hide the working-directory tail completely, even though
 * the two portions answer different navigation questions.
 */
export function compactTerminalTitleParts(
  title: TerminalTitle,
  maxLength = NATIVE_TERMINAL_TITLE_MAX_LENGTH,
): string {
  if (Array.from(title.text).length <= maxLength) {
    return title.text;
  }

  if (!title.command) {
    return compactTerminalTitle(title.path, maxLength);
  }

  const separator = " — ";
  const availableLength = maxLength - Array.from(separator).length;

  // Below this threshold neither portion can retain both an ellipsis and a
  // useful tail character, so the generic single-tail representation is more
  // truthful and legible.
  if (availableLength < 4) {
    return compactTerminalTitle(title.text, maxLength);
  }

  const pathLength = Array.from(title.path).length;
  const commandLength = Array.from(title.command).length;
  let pathBudget = Math.min(pathLength, Math.ceil(availableLength / 2));
  let commandBudget = Math.min(commandLength, availableLength - pathBudget);
  let remainingLength = availableLength - pathBudget - commandBudget;

  // If either component already fits inside its half, give the unused room to
  // the other one. Alternate allocations to keep two long components balanced.
  while (remainingLength > 0) {
    let allocated = false;
    if (pathBudget < pathLength) {
      pathBudget += 1;
      remainingLength -= 1;
      allocated = true;
    }
    if (remainingLength > 0 && commandBudget < commandLength) {
      commandBudget += 1;
      remainingLength -= 1;
      allocated = true;
    }
    if (!allocated) {
      break;
    }
  }

  return `${compactTerminalTitle(title.path, pathBudget)}${separator}${compactTerminalTitle(
    title.command,
    commandBudget,
  )}`;
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
