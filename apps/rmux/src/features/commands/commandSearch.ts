import type { AppCommand } from "./types";

export function searchCommands(
  commands: readonly AppCommand[],
  query: string,
): AppCommand[] {
  const visible = commands.filter(
    (command) => command.visibleInPalette !== false,
  );
  const normalizedQuery = normalize(query).replace(/^>\s*/, "");
  if (!normalizedQuery) {
    return [...visible];
  }

  return visible
    .map((command, index) => ({
      command,
      index,
      score: commandScore(command, normalizedQuery),
    }))
    .filter((entry) => entry.score !== null)
    .sort((left, right) =>
      left.score === right.score
        ? left.index - right.index
        : (right.score ?? 0) - (left.score ?? 0),
    )
    .map((entry) => entry.command);
}

export function firstEnabledIndex(commands: readonly AppCommand[]): number {
  return commands.findIndex((command) => command.enabled);
}

export function nextEnabledIndex(
  commands: readonly AppCommand[],
  currentIndex: number,
  direction: 1 | -1,
): number {
  if (commands.length === 0 || firstEnabledIndex(commands) === -1) {
    return -1;
  }

  let index = currentIndex;
  if (index < 0 || index >= commands.length) {
    index = direction === 1 ? -1 : 0;
  }
  for (let visited = 0; visited < commands.length; visited += 1) {
    index = (index + direction + commands.length) % commands.length;
    if (commands[index].enabled) {
      return index;
    }
  }
  return -1;
}

function commandScore(command: AppCommand, query: string): number | null {
  const title = normalize(command.title);
  const category = normalize(command.category);
  const haystack = normalize(
    [
      command.category,
      command.title,
      command.detail ?? "",
      ...(command.keywords ?? []),
    ].join(" "),
  );
  const terms = query.split(/\s+/).filter(Boolean);

  if (title === query) {
    return 1_000;
  }
  if (title.startsWith(query)) {
    return 800 - title.length;
  }
  if (`${category} ${title}`.startsWith(query)) {
    return 700 - title.length;
  }
  if (terms.every((term) => haystack.includes(term))) {
    return 500 - haystack.indexOf(terms[0]);
  }
  if (isSubsequence(query.replace(/\s/g, ""), haystack.replace(/\s/g, ""))) {
    return 100 - haystack.length;
  }
  return null;
}

function isSubsequence(query: string, candidate: string): boolean {
  let queryIndex = 0;
  for (const character of candidate) {
    if (character === query[queryIndex]) {
      queryIndex += 1;
      if (queryIndex === query.length) {
        return true;
      }
    }
  }
  return query.length === 0;
}

function normalize(value: string): string {
  return value.trim().toLocaleLowerCase();
}
