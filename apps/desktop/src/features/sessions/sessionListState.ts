import type {
  ConnectionTarget,
  SessionSummary,
  TerminalSize,
} from "../../lib/types";
import { sessionKey, targetKey } from "../targets/targets";
import { hasKnownTerminalSize, mergeSessionObservation, observedAt, sameTerminalSize } from "./sessionObservation";

export interface SessionListRefreshToken {
  requestId: number;
  mutationRevision: number;
}

export class SessionListRefreshGuard {
  private latestRequestId = 0;
  private mutationRevision = 0;

  begin(): SessionListRefreshToken {
    this.latestRequestId += 1;
    return {
      requestId: this.latestRequestId,
      mutationRevision: this.mutationRevision,
    };
  }

  recordMutation(): void {
    this.mutationRevision += 1;
  }

  canApply(token: SessionListRefreshToken): boolean {
    return (
      token.requestId === this.latestRequestId &&
      token.mutationRevision === this.mutationRevision
    );
  }

  isLatest(token: SessionListRefreshToken): boolean {
    return token.requestId === this.latestRequestId;
  }
}

export function replaceSessionList(
  sessions: readonly SessionSummary[],
  current: readonly SessionSummary[] = [],
): SessionSummary[] {
  const previous = new Map(current.map((session) => [sessionKey(session), session]));
  return sessions.map((session) => mergeSessionObservation(previous.get(sessionKey(session)), session));
}

export function prependSession(
  sessions: readonly SessionSummary[],
  session: SessionSummary,
): SessionSummary[] {
  return [
    mergeSessionObservation(sessions.find((item) => sessionKey(item) === sessionKey(session)), session),
    ...sessions.filter((item) => sessionKey(item) !== sessionKey(session)),
  ];
}

/**
 * Applies successful per-target refreshes while retaining the last known rows
 * for failed targets. Removed targets are always dropped.
 */
export function mergeTargetSessionLists(
  current: readonly SessionSummary[],
  targets: readonly ConnectionTarget[],
  refreshed: ReadonlyMap<string, readonly SessionSummary[]>,
): SessionSummary[] {
  const currentByTarget = new Map<string, SessionSummary[]>();
  for (const session of current) {
    const key = targetKey(session.target);
    const existing = currentByTarget.get(key) ?? [];
    existing.push(session);
    currentByTarget.set(key, existing);
  }

  return targets.flatMap((target) => {
    const key = targetKey(target);
    return replaceSessionList(refreshed.get(key) ?? currentByTarget.get(key) ?? [], currentByTarget.get(key));
  });
}

export function syncSessionObservation(
  sessions: SessionSummary[],
  identity: string,
  observed: SessionSummary,
): SessionSummary[] {
  if (sessionKey(observed) !== identity || observedAt(observed) === null && !hasKnownTerminalSize(observed)) return sessions;
  const index = sessions.findIndex((session) => sessionKey(session) === identity);
  if (index === -1) return sessions;
  const session = sessions[index];
  const observation = mergeSessionObservation(session, observed);
  if (sameTerminalSize(session.terminal_size, observation.terminal_size) &&
    observedAt(session) === observedAt(observation) && hasKnownTerminalSize(session) === hasKnownTerminalSize(observation)) return sessions;
  const updated = [...sessions];
  updated[index] = {
    ...session,
    terminal_size: observation.terminal_size,
    terminal_size_known: observation.terminal_size_known,
    last_seen_at_ms: observation.last_seen_at_ms,
  };
  return updated;
}

export function syncSessionTerminalSize(
  sessions: SessionSummary[],
  identity: string,
  terminalSize: TerminalSize,
): SessionSummary[] {
  const index = sessions.findIndex((session) => sessionKey(session) === identity);
  if (index === -1) {
    return sessions;
  }

  const session = sessions[index];
  if (sameTerminalSize(session.terminal_size, terminalSize)) {
    return sessions;
  }

  const updated = [...sessions];
  updated[index] = {
    ...session,
    terminal_size: terminalSize,
  };
  return updated;
}

export function removeSession(
  sessions: readonly SessionSummary[],
  identity: string,
): SessionSummary[] {
  return sessions.filter((session) => sessionKey(session) !== identity);
}
