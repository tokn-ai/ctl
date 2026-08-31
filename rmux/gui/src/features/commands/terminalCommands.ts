import type {
  ConnectionPhase,
  SessionSummary,
} from "../../lib/types";
import type {
  AppCommand,
  Keybinding,
  ShortcutPlatform,
} from "./types";

export const COMMAND_IDS = {
  showPalette: "view.show_command_palette",
  newShell: "session.new_shell",
  newTab: "tab.new_shell_here",
  refreshSessions: "session.refresh",
  nextTab: "tab.next",
  previousTab: "tab.previous",
  disconnect: "session.disconnect",
  close: "session.close",
  toggleInput: "terminal.toggle_input",
  toggleResize: "terminal.toggle_resize_with_window",
  reconnect: "terminal.reconnect",
  focus: "terminal.focus",
} as const;

export const SHOW_PALETTE_KEYBINDING: Keybinding = {
  code: "KeyP",
  primary: true,
  shift: true,
};

interface TerminalCommandContext {
  sessions: readonly SessionSummary[];
  tabs: readonly SessionSummary[];
  activeSessionId: string | null;
  phase: ConnectionPhase;
  inputOwned: boolean;
  resizeWithWindow: boolean;
  listLoading: boolean;
  creating: boolean;
  createFormOpen: boolean;
  closingSessionIds: ReadonlySet<string>;
  disconnectingSessionId: string | null;
  terminalReady: boolean;
  currentWorkingDirectory: string | null;
  shortcutPlatform: ShortcutPlatform;
}

interface TerminalCommandActions {
  showPalette(): void;
  showNewShellForm(): void;
  openShellTab(): void;
  refreshSessions(): void;
  selectSession(session: SessionSummary): void;
  disconnectSession(session: SessionSummary): void;
  requestCloseSession(session: SessionSummary): void;
  toggleInput(): void;
  toggleResizeWithWindow(): void;
  reconnect(): void;
  focusTerminal(): void;
}

export function buildTerminalCommands(
  context: TerminalCommandContext,
  actions: TerminalCommandActions,
): AppCommand[] {
  const activeSession =
    context.sessions.find(
      (session) => session.session_id === context.activeSessionId,
    ) ?? null;
  const nextTab = adjacentSession(
    context.tabs,
    context.activeSessionId,
    1,
  );
  const previousTab = adjacentSession(
    context.tabs,
    context.activeSessionId,
    -1,
  );
  const attached = context.phase === "attached";
  const canReconnect =
    activeSession !== null &&
    (context.phase === "disconnected" || context.phase === "error");
  const closingActive =
    activeSession !== null &&
    context.closingSessionIds.has(activeSession.session_id);
  const disconnectingActive =
    activeSession !== null &&
    context.disconnectingSessionId === activeSession.session_id;

  const commands: AppCommand[] = [
    {
      id: COMMAND_IDS.showPalette,
      category: "View",
      title: "Show Command Palette",
      keybinding: SHOW_PALETTE_KEYBINDING,
      enabled: true,
      visibleInPalette: false,
      focusTerminalAfterRun: false,
      run: actions.showPalette,
    },
    {
      id: COMMAND_IDS.newShell,
      category: "Session",
      title: "New Shell",
      keywords: ["create", "start"],
      keybinding: { code: "KeyN", primary: true, shift: true },
      enabled: !context.creating && !context.createFormOpen,
      disabledReason: context.creating
        ? "A shell is being created."
        : "The new-shell form is already open.",
      focusTerminalAfterRun: false,
      run: actions.showNewShellForm,
    },
    {
      id: COMMAND_IDS.newTab,
      category: "Terminal",
      title: "New Tab in Current Folder",
      detail: context.currentWorkingDirectory ?? undefined,
      keywords: ["shell", "terminal", "cwd", "folder"],
      keybinding: {
        code: "KeyT",
        primary: true,
        shift: context.shortcutPlatform === "other",
      },
      enabled: context.currentWorkingDirectory !== null && !context.creating,
      disabledReason: context.creating
        ? "A terminal tab is already being created."
        : "The current shell has not reported its working directory.",
      focusTerminalAfterRun: false,
      run: actions.openShellTab,
    },
    {
      id: COMMAND_IDS.refreshSessions,
      category: "Session",
      title: "Refresh Session List",
      keywords: ["reload"],
      enabled: !context.listLoading,
      disabledReason: "The session list is already refreshing.",
      run: actions.refreshSessions,
    },
    {
      id: COMMAND_IDS.nextTab,
      category: "Terminal",
      title: "Switch to Next Tab",
      keybinding: {
        code: "BracketRight",
        primary: true,
        shift: true,
      },
      enabled: nextTab !== null,
      disabledReason: "There is no other tab to select.",
      run: () => {
        if (nextTab) {
          actions.selectSession(nextTab);
        }
      },
    },
    {
      id: COMMAND_IDS.previousTab,
      category: "Terminal",
      title: "Switch to Previous Tab",
      keybinding: {
        code: "BracketLeft",
        primary: true,
        shift: true,
      },
      enabled: previousTab !== null,
      disabledReason: "There is no other tab to select.",
      run: () => {
        if (previousTab) {
          actions.selectSession(previousTab);
        }
      },
    },
    {
      id: COMMAND_IDS.disconnect,
      category: "Session",
      title: "Detach Active Tab",
      detail: activeSession?.name,
      keywords: ["detach", "disconnect"],
      keybinding: {
        code: "KeyW",
        primary: true,
        shift: context.shortcutPlatform === "other",
      },
      macosNativeKeybinding: true,
      enabled:
        activeSession !== null &&
        context.phase !== "ended" &&
        !disconnectingActive &&
        !closingActive,
      disabledReason: activeSession
        ? "The active tab cannot be detached right now."
        : "No session is active.",
      run: () => {
        if (activeSession) {
          actions.disconnectSession(activeSession);
        }
      },
    },
    {
      id: COMMAND_IDS.close,
      category: "Session",
      title: "Close Active Session",
      detail: activeSession?.name,
      keywords: ["exit", "kill", "terminate"],
      keybinding: {
        code: "KeyE",
        primary: true,
        shift: context.shortcutPlatform === "other",
      },
      macosNativeKeybinding: true,
      enabled: activeSession !== null && !closingActive && !disconnectingActive,
      disabledReason: activeSession
        ? "The active session is already changing state."
        : "No session is active.",
      focusTerminalAfterRun: false,
      run: () => {
        if (activeSession) {
          actions.requestCloseSession(activeSession);
        }
      },
    },
    {
      id: COMMAND_IDS.toggleInput,
      category: "Terminal",
      title: context.inputOwned ? "Release Input" : "Request Input",
      keywords: ["lease", "ownership"],
      enabled: attached,
      disabledReason: "Attach to a running session first.",
      run: actions.toggleInput,
    },
    {
      id: COMMAND_IDS.toggleResize,
      category: "Terminal",
      title: context.resizeWithWindow
        ? "Stop Resizing with Window"
        : "Resize with Window",
      keywords: ["layout", "lease", "ownership"],
      enabled: attached,
      disabledReason: "Attach to a running session first.",
      run: actions.toggleResizeWithWindow,
    },
    {
      id: COMMAND_IDS.reconnect,
      category: "Terminal",
      title: "Reconnect to Active Session",
      detail: activeSession?.name,
      enabled: canReconnect,
      disabledReason: activeSession
        ? "The active session is not disconnected."
        : "No session is active.",
      run: actions.reconnect,
    },
    {
      id: COMMAND_IDS.focus,
      category: "Terminal",
      title: "Focus Terminal",
      enabled: context.terminalReady,
      disabledReason: "The terminal renderer is not ready.",
      run: actions.focusTerminal,
    },
  ];

  for (const session of context.sessions) {
    const selected = session.session_id === context.activeSessionId;
    commands.push({
      id: `session.switch.${session.session_id}`,
      category: "Session",
      title: `Switch to ${session.name}`,
      detail: selected
        ? "Current session"
        : `${session.terminal_size.columns}×${session.terminal_size.rows} · ${session.status}`,
      keywords: ["attach", session.name],
      enabled: !selected && !context.closingSessionIds.has(session.session_id),
      disabledReason: selected
        ? "This session is already active."
        : "This session is closing.",
      run: () => actions.selectSession(session),
    });
  }

  return commands;
}

function adjacentSession(
  sessions: readonly SessionSummary[],
  activeSessionId: string | null,
  direction: 1 | -1,
): SessionSummary | null {
  if (sessions.length === 0) {
    return null;
  }
  const activeIndex = sessions.findIndex(
    (session) => session.session_id === activeSessionId,
  );
  if (activeIndex === -1) {
    return direction === 1 ? sessions[0] : sessions[sessions.length - 1];
  }
  if (sessions.length === 1) {
    return null;
  }
  return sessions[(activeIndex + direction + sessions.length) % sessions.length];
}
