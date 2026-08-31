import type { ShellStateSummary } from "../../lib/types";

export type TabShellStateCache = ReadonlyMap<string, ShellStateSummary>;

function hasNewerRevision(
  candidate: ShellStateSummary,
  current: ShellStateSummary,
): boolean {
  return BigInt(candidate.revision) > BigInt(current.revision);
}

/**
 * Keeps the newest daemon snapshot seen by this window for each local tab.
 *
 * Only the active tab has an attachment, so inactive tab labels deliberately
 * use their last observed shell state until they are selected again.
 */
export function rememberTabShellState(
  cache: TabShellStateCache,
  sessionId: string,
  shellState: ShellStateSummary,
  options: { replaceEqualRevision?: boolean } = {},
): TabShellStateCache {
  const current = cache.get(sessionId);
  const replacesEqualRevision =
    options.replaceEqualRevision && shellState.revision === current?.revision;
  if (current && !hasNewerRevision(shellState, current) && !replacesEqualRevision) {
    return cache;
  }

  const next = new Map(cache);
  next.set(sessionId, shellState);
  return next;
}

export function forgetTabShellState(
  cache: TabShellStateCache,
  sessionId: string,
): TabShellStateCache {
  if (!cache.has(sessionId)) {
    return cache;
  }

  const next = new Map(cache);
  next.delete(sessionId);
  return next;
}

export function retainTabShellStates(
  cache: TabShellStateCache,
  sessionIds: ReadonlySet<string>,
): TabShellStateCache {
  let next: Map<string, ShellStateSummary> | null = null;
  for (const sessionId of cache.keys()) {
    if (sessionIds.has(sessionId)) {
      continue;
    }
    next ??= new Map(cache);
    next.delete(sessionId);
  }
  return next ?? cache;
}
