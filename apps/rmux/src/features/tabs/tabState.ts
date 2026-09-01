import type { SessionSummary, TerminalSize } from "../../lib/types";

export interface ClosedTabState {
  tabs: SessionSummary[];
  nextTab: SessionSummary | null;
}

export function openTerminalTab(
  tabs: readonly SessionSummary[],
  session: SessionSummary,
): SessionSummary[] {
  const existingIndex = tabs.findIndex(
    (tab) => tab.session_id === session.session_id,
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
  sessionId: string,
): ClosedTabState {
  const closedIndex = tabs.findIndex((tab) => tab.session_id === sessionId);
  if (closedIndex === -1) {
    return { tabs: [...tabs], nextTab: null };
  }

  const remaining = tabs.filter((tab) => tab.session_id !== sessionId);
  const nextIndex = Math.min(closedIndex, remaining.length - 1);
  return {
    tabs: remaining,
    nextTab: nextIndex >= 0 ? remaining[nextIndex] : null,
  };
}

export function syncTabTerminalSize(
  tabs: readonly SessionSummary[],
  sessionId: string,
  terminalSize: TerminalSize,
): SessionSummary[] {
  return tabs.map((tab) =>
    tab.session_id === sessionId
      ? { ...tab, terminal_size: terminalSize }
      : tab,
  );
}

export function reconcileTerminalTabs(
  tabs: readonly SessionSummary[],
  listedSessions: readonly SessionSummary[],
  preservedSessionId: string | null,
): SessionSummary[] {
  const listedById = new Map(
    listedSessions.map((session) => [session.session_id, session]),
  );
  return tabs.flatMap((tab) => {
    const listed = listedById.get(tab.session_id);
    if (listed) {
      return [listed];
    }
    return tab.session_id === preservedSessionId ? [tab] : [];
  });
}
