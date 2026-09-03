import type {
  ConnectionPhase,
  ConnectionTarget,
  SessionSummary,
} from "../../lib/types";
import type { AppCommand, CommandArguments, ShortcutPlatform } from "./types";
import { sessionKey, targetKey, targetLabel } from "../targets/targets";

import { COMMAND_IDS } from "./commandIds";
import { defaultKeybindings } from "./keymap";
export { COMMAND_IDS } from "./commandIds";

interface TerminalCommandContext {
  targets: readonly ConnectionTarget[];
  sessions: readonly SessionSummary[];
  tabs: readonly SessionSummary[];
  activeSessionKey: string | null;
  attachmentSessionKey: string | null;
  phase: ConnectionPhase;
  inputOwned: boolean;
  resizeWithWindow: boolean;
  listLoading: boolean;
  creating: boolean;
  newShellOpen: boolean;
  pendingCloseSessionKey: string | null;
  closingSessionKeys: ReadonlySet<string>;
  disconnectingSessionKey: string | null;
  terminalReady: boolean;
  currentWorkingDirectory: string | null;
  currentWorkingDirectoryDisplay: string | null;
  daemonRestartConfirmationPending: boolean;
  restartingDaemon: boolean;
  shortcutPlatform: ShortcutPlatform;
}

interface TerminalCommandActions {
  showPalette(): void;
  showAddHost(): void;
  showAddExistingSession(): void;
  forgetSession(session: SessionSummary): void;
  showNewShell(): void;
  openShellTab(): void;
  refreshSessions(): void;
  selectSession(session: SessionSummary): void | Promise<void>;
  disconnectSession(session: SessionSummary): void | Promise<void>;
  requestCloseSession(session: SessionSummary): void;
  confirmCloseSession(session: SessionSummary): void | Promise<void>;
  toggleInput(): void;
  toggleResizeWithWindow(): void;
  reconnect(): void;
  focusTerminal(): void;
  requestDaemonRestart(): void;
  connectHost(target: ConnectionTarget): void;
  removeHost(target: ConnectionTarget): void | Promise<void>;
  saveWorkspace(): void | Promise<void>;
  configureKeybindings(): void;
  reloadKeybindings(): void | Promise<void>;
}

export function buildTerminalCommands(
  context: TerminalCommandContext,
  actions: TerminalCommandActions,
): AppCommand[] {
  const activeSession =
    context.sessions.find(
      (session) => sessionKey(session) === context.activeSessionKey,
    ) ?? null;
  const closeConfirmationPending = context.pendingCloseSessionKey !== null;
  const closeSession = closeConfirmationPending
    ? (context.sessions.find(
        (session) => sessionKey(session) === context.pendingCloseSessionKey,
      ) ?? null)
    : activeSession;
  const closeSessionBusy =
    closeSession !== null &&
    (context.closingSessionKeys.has(sessionKey(closeSession)) ||
      context.disconnectingSessionKey === sessionKey(closeSession));
  const nextTab = adjacentSession(context.tabs, context.activeSessionKey, 1);
  const previousTab = adjacentSession(
    context.tabs,
    context.activeSessionKey,
    -1,
  );
  const activeAttachmentMatchesTab =
    activeSession !== null &&
    sessionKey(activeSession) === context.attachmentSessionKey;
  const activeTabAttached =
    activeAttachmentMatchesTab && context.phase === "attached";
  const canReconnect =
    activeAttachmentMatchesTab &&
    (context.phase === "disconnected" || context.phase === "error");
  const closingActive =
    activeSession !== null &&
    context.closingSessionKeys.has(sessionKey(activeSession));
  const disconnectingActive =
    activeSession !== null &&
    context.disconnectingSessionKey === sessionKey(activeSession);
  const daemonRestartInteractionBlocked =
    context.daemonRestartConfirmationPending || context.restartingDaemon;
  const daemonRestartInteractionDisabledReason = context.restartingDaemon
    ? "rmuxd is restarting."
    : "Confirm or cancel the pending rmuxd restart first.";
  const sessionFor = (args: CommandArguments = {}) =>
    args.session_key === undefined
      ? activeSession
      : (context.sessions.find(
          (session) => sessionKey(session) === args.session_key,
        ) ?? null);
  const targetFor = (args: CommandArguments = {}) =>
    args.target_key === undefined
      ? (activeSession?.target ?? null)
      : (context.targets.find(
          (target) => targetKey(target) === args.target_key,
        ) ?? null);
  const sessionAvailable = (session: SessionSummary | null) =>
    session !== null &&
    !daemonRestartInteractionBlocked &&
    !context.closingSessionKeys.has(sessionKey(session)) &&
    context.disconnectingSessionKey !== sessionKey(session);
  const daemonRestartBlocked =
    context.creating || context.newShellOpen || daemonRestartInteractionBlocked;
  const daemonRestartDisabledReason = context.restartingDaemon
    ? "rmuxd is already restarting."
    : context.daemonRestartConfirmationPending
      ? daemonRestartInteractionDisabledReason
      : context.creating
        ? "Wait for the shell being created to finish."
        : "Close the new-shell dialog before restarting rmuxd.";

  const commands: AppCommand[] = [
    {
      id: COMMAND_IDS.addExistingSession,
      category: "Session",
      title: "Add existing session",
      keywords: ["import", "discover", "known", "workspace"],
      enabled: !daemonRestartInteractionBlocked,
      focusTerminalAfterRun: false,
      run: actions.showAddExistingSession,
    },
    {
      id: COMMAND_IDS.forgetSession,
      category: "Workspace",
      title: "Remove Active Session from Workspace",
      detail: "Forget this entry without terminating its shell.",
      keywords: ["forget", "remove"],
      enabled:
        activeSession !== null &&
        !daemonRestartInteractionBlocked &&
        !closingActive &&
        !disconnectingActive,
      focusTerminalAfterRun: false,
      isEnabled: (args) => sessionAvailable(sessionFor(args)),
      run: (args) => {
        const session = sessionFor(args);
        if (session) actions.forgetSession(session);
      },
    },
    {
      id: COMMAND_IDS.addHost,
      category: "Host",
      title: "Add SSH Host",
      keywords: ["remote", "ssh", "connect"],
      enabled: !daemonRestartInteractionBlocked,
      focusTerminalAfterRun: false,
      run: actions.showAddHost,
    },
    {
      id: COMMAND_IDS.showPalette,
      category: "View",
      title: "Show Command Palette",
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
      enabled:
        !context.creating &&
        !context.newShellOpen &&
        !context.daemonRestartConfirmationPending &&
        !context.restartingDaemon,
      disabledReason: context.restartingDaemon
        ? "rmuxd is restarting."
        : context.daemonRestartConfirmationPending
          ? "Confirm or cancel the pending rmuxd restart first."
          : context.creating
            ? "A shell is being created."
            : "The new-shell dialog is already open.",
      focusTerminalAfterRun: false,
      run: actions.showNewShell,
    },
    {
      id: COMMAND_IDS.newTab,
      category: "Terminal",
      title: "New Tab in Current Folder",
      detail: context.currentWorkingDirectoryDisplay ?? undefined,
      keywords: ["shell", "terminal", "cwd", "folder"],
      enabled:
        context.currentWorkingDirectory !== null &&
        !context.creating &&
        !context.daemonRestartConfirmationPending &&
        !context.restartingDaemon,
      disabledReason: context.restartingDaemon
        ? "rmuxd is restarting."
        : context.daemonRestartConfirmationPending
          ? "Confirm or cancel the pending rmuxd restart first."
          : context.creating
            ? "A terminal tab is already being created."
            : "The current shell has not reported its working directory.",
      focusTerminalAfterRun: false,
      run: actions.openShellTab,
    },
    {
      id: COMMAND_IDS.refreshSessions,
      category: "Session",
      title: "Refresh Known Sessions",
      keywords: ["reload"],
      enabled: !context.listLoading && !daemonRestartInteractionBlocked,
      disabledReason: daemonRestartInteractionBlocked
        ? daemonRestartInteractionDisabledReason
        : "The session list is already refreshing.",
      run: actions.refreshSessions,
    },
    {
      id: COMMAND_IDS.nextTab,
      category: "Terminal",
      title: "Switch to Next Tab",
      enabled: nextTab !== null && !daemonRestartInteractionBlocked,
      disabledReason: daemonRestartInteractionBlocked
        ? daemonRestartInteractionDisabledReason
        : "There is no other tab to select.",
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
      enabled: previousTab !== null && !daemonRestartInteractionBlocked,
      disabledReason: daemonRestartInteractionBlocked
        ? daemonRestartInteractionDisabledReason
        : "There is no other tab to select.",
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
      enabled:
        activeSession !== null &&
        context.phase !== "ended" &&
        !disconnectingActive &&
        !closingActive &&
        !daemonRestartInteractionBlocked,
      disabledReason: daemonRestartInteractionBlocked
        ? daemonRestartInteractionDisabledReason
        : activeSession
          ? "The active tab cannot be detached right now."
          : "No session is active.",
      isEnabled: (args) => {
        const session = sessionFor(args);
        return (
          sessionAvailable(session) &&
          context.tabs.some((tab) => sessionKey(tab) === sessionKey(session!))
        );
      },
      run: (args) => {
        const session = sessionFor(args);
        if (session) {
          return actions.disconnectSession(session);
        }
      },
    },
    {
      id: COMMAND_IDS.close,
      category: "Session",
      title: closeConfirmationPending
        ? "Confirm Close Session"
        : "Close Active Session",
      detail: closeSession?.name,
      keywords: ["exit", "kill", "terminate"],
      enabled:
        !daemonRestartInteractionBlocked &&
        closeSession !== null &&
        !closeSessionBusy,
      disabledReason: daemonRestartInteractionBlocked
        ? daemonRestartInteractionDisabledReason
        : closeSession
          ? "The session is already changing state."
          : closeConfirmationPending
            ? "The session awaiting confirmation is no longer available."
            : "No session is active.",
      focusTerminalAfterRun: false,
      isEnabled: (args) =>
        sessionAvailable(
          closeConfirmationPending ? closeSession : sessionFor(args),
        ),
      run: (args) => {
        const requestedSession = closeConfirmationPending
          ? closeSession
          : sessionFor(args);
        if (requestedSession) {
          if (closeConfirmationPending)
            return actions.confirmCloseSession(requestedSession);
          else actions.requestCloseSession(requestedSession);
        }
      },
    },
    {
      id: COMMAND_IDS.toggleInput,
      category: "Terminal",
      title: context.inputOwned ? "Release Input" : "Request Input",
      keywords: ["lease", "ownership"],
      enabled: activeTabAttached && !daemonRestartInteractionBlocked,
      disabledReason: daemonRestartInteractionBlocked
        ? daemonRestartInteractionDisabledReason
        : "Attach to a running session first.",
      run: actions.toggleInput,
    },
    {
      id: COMMAND_IDS.toggleResize,
      category: "Terminal",
      title: context.resizeWithWindow
        ? "Stop Resizing with Window"
        : "Resize with Window",
      keywords: ["layout", "lease", "ownership"],
      enabled: activeTabAttached && !daemonRestartInteractionBlocked,
      disabledReason: daemonRestartInteractionBlocked
        ? daemonRestartInteractionDisabledReason
        : "Attach to a running session first.",
      run: actions.toggleResizeWithWindow,
    },
    {
      id: COMMAND_IDS.reconnect,
      category: "Terminal",
      title: "Reconnect to Active Session",
      detail: activeSession?.name,
      enabled: canReconnect && !daemonRestartInteractionBlocked,
      disabledReason: daemonRestartInteractionBlocked
        ? daemonRestartInteractionDisabledReason
        : activeSession
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
    {
      id: COMMAND_IDS.restartDaemon,
      category: "Daemon",
      title: "Restart rmuxd",
      detail:
        "Terminate every local rmux session, including other apps’ sessions, and start a new daemon.",
      keywords: ["daemon", "restart", "protocol", "version", "recover"],
      enabled: !daemonRestartBlocked,
      disabledReason: daemonRestartDisabledReason,
      keepPaletteOpen: false,
      focusTerminalAfterRun: false,
      run: actions.requestDaemonRestart,
    },
  ];

  commands.push(
    {
      id: COMMAND_IDS.selectSession,
      category: "Session",
      title: "Connect Active Session",
      enabled: sessionAvailable(activeSession),
      isEnabled: (args) => sessionAvailable(sessionFor(args)),
      run: (args) => {
        const session = sessionFor(args);
        if (session) return actions.selectSession(session);
      },
    },
    {
      id: COMMAND_IDS.connectHost,
      category: "Host",
      title: "Connect Active Host",
      enabled: targetFor()?.kind === "ssh" && !daemonRestartInteractionBlocked,
      isEnabled: (args) =>
        targetFor(args)?.kind === "ssh" && !daemonRestartInteractionBlocked,
      focusTerminalAfterRun: false,
      run: (args) => {
        const target = targetFor(args);
        if (target) actions.connectHost(target);
      },
    },
    {
      id: COMMAND_IDS.removeHost,
      category: "Host",
      title: "Remove Active Host from Workspace",
      enabled: targetFor()?.kind === "ssh" && !daemonRestartInteractionBlocked,
      isEnabled: (args) =>
        targetFor(args)?.kind === "ssh" && !daemonRestartInteractionBlocked,
      focusTerminalAfterRun: false,
      run: (args) => {
        const target = targetFor(args);
        if (target) return actions.removeHost(target);
      },
    },
    {
      id: COMMAND_IDS.saveWorkspace,
      category: "Workspace",
      title: "Retry Saving Workspace",
      enabled: !daemonRestartInteractionBlocked,
      run: actions.saveWorkspace,
    },
    {
      id: COMMAND_IDS.configureKeybindings,
      category: "Settings",
      title: "Configure Keyboard Shortcuts",
      keywords: ["keybindings", "keys", "settings"],
      enabled: true,
      focusTerminalAfterRun: false,
      run: actions.configureKeybindings,
    },
    {
      id: COMMAND_IDS.reloadKeybindings,
      category: "Settings",
      title: "Reload Keyboard Shortcuts",
      enabled: true,
      run: actions.reloadKeybindings,
    },
  );

  for (const session of context.sessions) {
    const identity = sessionKey(session);
    const selected = identity === context.activeSessionKey;
    const matchesAttachment = identity === context.attachmentSessionKey;
    const attaching =
      matchesAttachment &&
      (context.phase === "connecting" || context.phase === "reconnecting");
    const sessionAttached = matchesAttachment && context.phase === "attached";
    const ended = matchesAttachment && context.phase === "ended";
    const retryable = selected && !attaching && !sessionAttached && !ended;
    const title = retryable
      ? matchesAttachment
        ? `Reconnect to ${session.name}`
        : `Attach to ${session.name}`
      : `Switch to ${session.name}`;
    const detail = selected
      ? attaching
        ? "Attaching…"
        : sessionAttached
          ? "Current session"
          : ended
            ? "Session ended"
            : "Retry attachment"
      : `${targetLabel(session.target)} · ${session.terminal_size.columns}×${session.terminal_size.rows} · ${session.status}`;
    commands.push({
      id: sessionSwitchCommandId(session),
      category: "Session",
      title,
      detail,
      keywords: ["attach", session.name],
      enabled:
        !daemonRestartInteractionBlocked &&
        !context.closingSessionKeys.has(identity) &&
        (!selected || retryable),
      disabledReason: daemonRestartInteractionBlocked
        ? daemonRestartInteractionDisabledReason
        : context.closingSessionKeys.has(identity)
          ? "This session is closing."
          : attaching
            ? "This session is already attaching."
            : sessionAttached
              ? "This session is already active."
              : "This session has ended.",
      run: () => actions.selectSession(session),
    });
  }

  const defaults = defaultKeybindings(context.shortcutPlatform);
  return commands.map((command) => ({
    ...command,
    keybinding: defaults.get(command.id),
  }));
}

function adjacentSession(
  sessions: readonly SessionSummary[],
  activeSessionKey: string | null,
  direction: 1 | -1,
): SessionSummary | null {
  if (sessions.length === 0) {
    return null;
  }
  const activeIndex = sessions.findIndex(
    (session) => sessionKey(session) === activeSessionKey,
  );
  if (activeIndex === -1) {
    return direction === 1 ? sessions[0] : sessions[sessions.length - 1];
  }
  if (sessions.length === 1) {
    return null;
  }
  return sessions[
    (activeIndex + direction + sessions.length) % sessions.length
  ];
}

export function sessionSwitchCommandId(session: SessionSummary): string {
  return `session.switch.${encodeURIComponent(sessionKey(session))}`;
}
