import type { SessionSummary, TerminalSize } from "../../lib/types";
import { sessionKey } from "../targets/targets";

export interface ClosedTabState {
  tabs: SessionSummary[];
  nextTab: SessionSummary | null;
}

export function openTerminalTab(
  tabs: readonly SessionSummary[],
  session: SessionSummary,
): SessionSummary[] {
  const existingIndex = tabs.findIndex(
    (tab) => sessionKey(tab) === sessionKey(session),
  );
  if (existingIndex === -1) {
    return [...tabs, session];
  }
  if (tabs[existingIndex] === session) {
    return [...tabs];
  }

  const updated = [...tabs];
  updated[existingIndex] = session;
  return updated;
}

export function closeTerminalTab(
  tabs: readonly SessionSummary[],
  identity: string,
): ClosedTabState {
  const closedIndex = tabs.findIndex((tab) => sessionKey(tab) === identity);
  if (closedIndex === -1) {
    return { tabs: [...tabs], nextTab: null };
  }

  const remaining = tabs.filter((tab) => sessionKey(tab) !== identity);
  const nextIndex = Math.min(closedIndex, remaining.length - 1);
  return {
    tabs: remaining,
    nextTab: nextIndex >= 0 ? remaining[nextIndex] : null,
  };
}

export function syncTabTerminalSize(
  tabs: readonly SessionSummary[],
  identity: string,
  terminalSize: TerminalSize,
): SessionSummary[] {
  return tabs.map((tab) =>
    sessionKey(tab) === identity
      ? { ...tab, terminal_size: terminalSize }
      : tab,
  );
}

export function reconcileTerminalTabs(
  tabs: readonly SessionSummary[],
  listedSessions: readonly SessionSummary[],
  preservedSessionKey: string | null,
): SessionSummary[] {
  const listedById = new Map(
    listedSessions.map((session) => [sessionKey(session), session]),
  );
  return tabs.flatMap((tab) => {
    const identity = sessionKey(tab);
    const listed = listedById.get(identity);
    if (listed) {
      return [listed];
    }
    return identity === preservedSessionKey ? [tab] : [];
  });
}
