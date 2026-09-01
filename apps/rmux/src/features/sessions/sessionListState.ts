import type {
  ConnectionTarget,
  SessionSummary,
  TerminalSize,
} from "../../lib/types";
import { sessionKey, targetKey } from "../targets/targets";

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
): SessionSummary[] {
  return [...sessions];
}

export function prependSession(
  sessions: readonly SessionSummary[],
  session: SessionSummary,
): SessionSummary[] {
  return [
    session,
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
    return [...(refreshed.get(key) ?? currentByTarget.get(key) ?? [])];
  });
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

function sameTerminalSize(left: TerminalSize, right: TerminalSize): boolean {
  return (
    left.columns === right.columns &&
    left.rows === right.rows &&
    left.pixel_width === right.pixel_width &&
    left.pixel_height === right.pixel_height
  );
}
