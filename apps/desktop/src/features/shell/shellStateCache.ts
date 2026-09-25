import type { ShellStateSummary } from "../../lib/types";

export type ShellStateCache = ReadonlyMap<string, ShellStateSummary>;

function hasNewerRevision(
  candidate: ShellStateSummary,
  current: ShellStateSummary,
): boolean {
  return BigInt(candidate.revision) > BigInt(current.revision);
}

/** Keeps the newest daemon snapshot seen for one session. */
export function rememberShellState(
  cache: ShellStateCache,
  sessionId: string,
  shellState: ShellStateSummary,
  options: { replaceEqualRevision?: boolean } = {},
): ShellStateCache {
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

export function forgetShellState(
  cache: ShellStateCache,
  sessionId: string,
): ShellStateCache {
  if (!cache.has(sessionId)) {
    return cache;
  }

  const next = new Map(cache);
  next.delete(sessionId);
  return next;
}

export function retainShellStates(
  cache: ShellStateCache,
  sessionIds: ReadonlySet<string>,
): ShellStateCache {
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

/**
 * Applies best-effort detached-session inspections without erasing a valid
 * cached snapshot when one inspection times out.
 *
 * Equal revisions retain the cache because an attached client may have been
 * authorized to see a running-command summary that one-shot inspection must
 * redact. A newer inspection still wins, ensuring stale command text is not
 * presented after the daemon reports a state transition.
 */
export function mergeShellStateInspections(
  cache: ShellStateCache,
  inspections: ReadonlyMap<string, ShellStateSummary>,
  sessionIds: ReadonlySet<string>,
): ShellStateCache {
  let next = retainShellStates(cache, sessionIds);
  for (const [sessionId, shellState] of inspections) {
    if (sessionIds.has(sessionId)) {
      next = rememberShellState(next, sessionId, shellState);
    }
  }
  return next;
}
