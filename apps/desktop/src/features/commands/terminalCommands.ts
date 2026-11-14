import type {
  ConnectionPhase,
  ConnectionTarget,
  SessionSummary,
} from "../../lib/types";
import { localizeCommand, type AppCommand, type CommandArguments, type LocalizedCommand, type ShortcutPlatform } from "./types";
import { formatter, message, translate, type Locale, type MessageValues } from "../i18n/messages";
import { sessionKey, targetKey, targetLabel } from "../targets/targets";

import { COMMAND_IDS } from "./commandIds";
import { defaultKeybindings } from "./keymap";
import type { ResizeControlStatus } from "../attachment/componentActions";
export { COMMAND_IDS } from "./commandIds";

interface TerminalCommandContext {
  targets: readonly ConnectionTarget[];
  /** Host-level availability can include an alternate to the target's method. */
  connectableHostKeys?: ReadonlySet<string>;
  sessions: readonly SessionSummary[];
  tabs: readonly SessionSummary[];
  activeSessionKey: string | null;
  attachmentSessionKey: string | null;
  phase: ConnectionPhase;
  inputOwned: boolean;
  resizeWithWindow: boolean;
  resizeControlStatus: ResizeControlStatus;
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
  showConnectHost(): void;
  showAddRoutedHost(): void;
  showAddExistingSession(): void;
  forgetSession(session: SessionSummary): void;
  showNewShell(target?: ConnectionTarget): void;
  openShellTab(): void;
  refreshSessions(): void;
  selectSession(session: SessionSummary): void | Promise<void>;
  disconnectSession(session: SessionSummary): void | Promise<void>;
  requestCloseSession(session: SessionSummary): void;
  confirmCloseSession(session: SessionSummary): void | Promise<void>;
  toggleInput(): void;
  toggleResizeWithWindow(): void | Promise<void>;
  requestResizeControl(acquire: boolean): void | Promise<void>;
  reconnect(): void;
  focusTerminal(): void;
  requestDaemonRestart(): void;
  connectHost(target: ConnectionTarget): void;
  configureHost(target: ConnectionTarget): void;
  removeHost(target: ConnectionTarget): void | Promise<void>;
  managePortForwards(target: ConnectionTarget): void;
  saveWorkspace(): void | Promise<void>;
  configureKeybindings(): void;
  reloadKeybindings(): void | Promise<void>;
}

export function buildTerminalCommands(
  context: TerminalCommandContext,
  actions: TerminalCommandActions,
  locale: Locale = "en",
): AppCommand[] {
  const t = (id: string, values?: MessageValues) => translate(locale, id, values);
  const intl = formatter(locale);
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
    (context.phase === "disconnected" || context.phase === "error" || context.phase === "retry_wait");
  const closingActive =
    activeSession !== null &&
    context.closingSessionKeys.has(sessionKey(activeSession));
  const disconnectingActive =
    activeSession !== null &&
    context.disconnectingSessionKey === sessionKey(activeSession);
  const daemonRestartInteractionBlocked =
    context.daemonRestartConfirmationPending || context.restartingDaemon;
  const daemonRestartInteractionDisabledReason = context.restartingDaemon
    ? t("command.daemon_restarting")
    : t("command.confirm_daemon_restart");
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
  const canConnectHost = (target: ConnectionTarget | null) =>
    target?.kind === "ssh" && !daemonRestartInteractionBlocked &&
    (context.connectableHostKeys?.has(targetKey(target)) ?? !target.unavailable);
  const hasConnectableHost = context.targets.some(canConnectHost);
  const sessionAvailable = (session: SessionSummary | null) =>
    session !== null &&
    !daemonRestartInteractionBlocked &&
    !context.closingSessionKeys.has(sessionKey(session)) &&
    context.disconnectingSessionKey !== sessionKey(session);
  const daemonRestartBlocked =
    context.creating || context.newShellOpen || daemonRestartInteractionBlocked;
  const daemonRestartDisabledReason = context.restartingDaemon
    ? t("command.already_restarting")
    : context.daemonRestartConfirmationPending
      ? daemonRestartInteractionDisabledReason
      : context.creating
        ? t("command.wait_for_shell")
        : t("command.close_shell_dialog");

  const commands: LocalizedCommand[] = [
    {
      id: COMMAND_IDS.addExistingSession,
      category: message("command.category_session"),
      title: message("command.add_existing"),
      keywords: ["import", "discover", "known", "workspace"],
      enabled: !daemonRestartInteractionBlocked,
      focusTerminalAfterRun: false,
      run: actions.showAddExistingSession,
    },
    {
      id: COMMAND_IDS.forgetSession,
      category: message("command.category_workspace"),
      title: message("command.forget_active"),
      detail: t("command.forget_detail"),
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
      category: message("command.category_host"),
      title: message("command.add_host"),
      detail: t("command.add_host_detail"),
      keywords: ["remote", "ssh", "connect", "gateway", "route"],
      enabled: !daemonRestartInteractionBlocked,
      focusTerminalAfterRun: false,
      run: actions.showAddHost,
    },
    {
      id: COMMAND_IDS.addRoutedHost,
      category: message("command.category_host"),
      title: message("command.add_routed_host"),
      // Existing keybindings still open the unified host setup flow.
      visibleInPalette: false,
      keywords: ["remote", "ssh", "jump", "bastion", "gateway", "route"],
      enabled: !daemonRestartInteractionBlocked,
      focusTerminalAfterRun: false,
      run: actions.showAddRoutedHost,
    },
    {
      id: COMMAND_IDS.showPalette,
      category: message("command.category_view"),
      title: message("command.palette"),
      enabled: true,
      visibleInPalette: false,
      focusTerminalAfterRun: false,
      run: actions.showPalette,
    },
    {
      id: COMMAND_IDS.newShell,
      category: message("command.category_session"),
      title: message("command.new_shell"),
      keywords: ["create", "start"],
      enabled:
        !context.creating &&
        !context.newShellOpen &&
        !context.daemonRestartConfirmationPending &&
        !context.restartingDaemon,
      disabledReason: context.restartingDaemon
        ? t("command.daemon_restarting")
        : context.daemonRestartConfirmationPending
          ? t("command.confirm_daemon_restart")
          : context.creating
            ? t("command.shell_creating")
            : t("command.new_shell_open"),
      focusTerminalAfterRun: false,
      run: (args) => {
        if (args?.target_key) {
          const target = targetFor(args);
          if (!target || (target.kind === "ssh" && target.unavailable)) return;
          actions.showNewShell(target);
        } else actions.showNewShell();
      },
    },
    {
      id: COMMAND_IDS.newTab,
      category: message("command.category_terminal"),
      title: message("command.new_tab_here"),
      detail: context.currentWorkingDirectoryDisplay ?? undefined,
      keywords: ["shell", "terminal", "cwd", "folder"],
      enabled:
        context.currentWorkingDirectory !== null &&
        !context.creating &&
        !context.daemonRestartConfirmationPending &&
        !context.restartingDaemon,
      disabledReason: context.restartingDaemon
        ? t("command.daemon_restarting")
        : context.daemonRestartConfirmationPending
          ? t("command.confirm_daemon_restart")
          : context.creating
            ? t("command.tab_creating")
            : t("command.working_dir_unknown"),
      focusTerminalAfterRun: false,
      run: actions.openShellTab,
    },
    {
      id: COMMAND_IDS.refreshSessions,
      category: message("command.category_session"),
      title: message("command.refresh_sessions"),
      keywords: ["reload"],
      enabled: !context.listLoading && !daemonRestartInteractionBlocked,
      disabledReason: daemonRestartInteractionBlocked
        ? daemonRestartInteractionDisabledReason
        : t("command.refresh_busy"),
      run: actions.refreshSessions,
    },
    {
      id: COMMAND_IDS.nextTab,
      allow_concurrent: true,
      category: message("command.category_terminal"),
      title: message("command.next_tab"),
      enabled: nextTab !== null && !daemonRestartInteractionBlocked,
      disabledReason: daemonRestartInteractionBlocked
        ? daemonRestartInteractionDisabledReason
        : t("command.no_other_tabs"),
      run: () => {
        if (nextTab) {
          return actions.selectSession(nextTab);
        }
      },
    },
    {
      id: COMMAND_IDS.previousTab,
      allow_concurrent: true,
      category: message("command.category_terminal"),
      title: message("command.previous_tab"),
      enabled: previousTab !== null && !daemonRestartInteractionBlocked,
      disabledReason: daemonRestartInteractionBlocked
        ? daemonRestartInteractionDisabledReason
        : t("command.no_other_tabs"),
      run: () => {
        if (previousTab) {
          return actions.selectSession(previousTab);
        }
      },
    },
    {
      id: COMMAND_IDS.disconnect,
      // Closing may connect a neighboring tab after cleanup.
      // That connection must not block closing another tab.
      allow_concurrent: true,
      category: message("command.category_session"),
      title: message("command.detach_active"),
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
          ? t("command.cannot_detach")
          : t("command.no_active_session"),
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
      category: message("command.category_session"),
      title: closeConfirmationPending
        ? message("command.confirm_terminate")
        : message("command.terminate_active"),
      detail: closeSession?.name,
      keywords: ["exit", "kill", "terminate", "close"],
      enabled:
        !daemonRestartInteractionBlocked &&
        closeSession !== null &&
        !closeSessionBusy,
      disabledReason: daemonRestartInteractionBlocked
        ? daemonRestartInteractionDisabledReason
        : closeSession
          ? t("command.session_changing")
          : closeConfirmationPending
            ? t("command.confirmation_session_gone")
            : t("command.no_active_session"),
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
      category: message("command.category_terminal"),
      title: context.inputOwned ? message("command.release_input") : message("command.request_input"),
      keywords: ["lease", "ownership"],
      enabled: activeTabAttached && !daemonRestartInteractionBlocked,
      disabledReason: daemonRestartInteractionBlocked
        ? daemonRestartInteractionDisabledReason
        : t("command.attach_first"),
      run: actions.toggleInput,
    },
    {
      id: COMMAND_IDS.toggleResizeControl,
      category: message("command.category_terminal"),
      title: context.resizeControlStatus === "owned" ? message("command.release_resize") : message("command.take_resize"),
      detail: context.resizeControlStatus === "held_elsewhere" ? t("command.resize_held") : undefined,
      keywords: ["layout", "lease", "ownership", "panes", "zoom"],
      enabled: activeTabAttached && !daemonRestartInteractionBlocked &&
        context.resizeControlStatus !== "unavailable",
      disabledReason: daemonRestartInteractionBlocked ? daemonRestartInteractionDisabledReason
        : t("command.attach_first"),
      focusTerminalAfterRun: false,
      run: () => actions.requestResizeControl(context.resizeControlStatus !== "owned"),
    },
    {
      id: COMMAND_IDS.toggleResize,
      category: message("command.category_terminal"),
      title: context.resizeWithWindow
        ? message("command.stop_resize")
        : message("command.resize_with_window"),
      keywords: ["layout", "window", "automatic", "fixed"],
      enabled: activeTabAttached && !daemonRestartInteractionBlocked,
      disabledReason: daemonRestartInteractionBlocked
        ? daemonRestartInteractionDisabledReason
        : t("command.attach_first"),
      run: actions.toggleResizeWithWindow,
    },
    {
      id: COMMAND_IDS.reconnect,
      category: message("command.category_terminal"),
      title: message("command.reconnect_active"),
      detail: activeSession?.name,
      enabled: canReconnect && !daemonRestartInteractionBlocked,
      disabledReason: daemonRestartInteractionBlocked
        ? daemonRestartInteractionDisabledReason
        : activeSession
          ? t("command.active_not_disconnected")
          : t("command.no_active_session"),
      run: actions.reconnect,
    },
    {
      id: COMMAND_IDS.focus,
      category: message("command.category_terminal"),
      title: message("command.focus_terminal"),
      enabled: context.terminalReady,
      disabledReason: t("command.renderer_not_ready"),
      run: actions.focusTerminal,
    },
    {
      id: COMMAND_IDS.restartDaemon,
      category: message("command.category_daemon"),
      title: message("command.restart_daemon"),
      detail:
        t("command.restart_daemon_detail"),
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
      id: COMMAND_IDS.configureHost,
      category: message("command.category_host"),
      title: message("command.host_settings"),
      detail: t("command.host_settings_detail"),
      keywords: ["connection", "method", "preferred", "gateway", "rename"],
      enabled: targetFor()?.kind === "ssh" && !daemonRestartInteractionBlocked,
      isEnabled: (args) => targetFor(args)?.kind === "ssh" && !daemonRestartInteractionBlocked,
      focusTerminalAfterRun: false,
      run: (args) => {
        const target = targetFor(args);
        if (target?.kind === "ssh") actions.configureHost(target);
      },
    },
    {
      id: COMMAND_IDS.selectSession,
      allow_concurrent: true,
      category: message("command.category_session"),
      title: message("command.connect_active"),
      enabled: sessionAvailable(activeSession),
      isEnabled: (args) => sessionAvailable(sessionFor(args)),
      run: (args) => {
        const session = sessionFor(args);
        if (session) return actions.selectSession(session);
      },
    },
    {
      id: COMMAND_IDS.connectHost,
      category: message("command.category_host"),
      title: message("command.connect_host"),
      detail: t("command.connect_host_detail"),
      keywords: ["ssh", "remote", "alias"],
      enabled: hasConnectableHost,
      disabledReason: daemonRestartInteractionBlocked
        ? daemonRestartInteractionDisabledReason
        : t("command.no_ssh_hosts"),
      isEnabled: (args) => args.target_key === undefined
        ? hasConnectableHost
        : canConnectHost(targetFor(args)),
      focusTerminalAfterRun: false,
      run: (args) => {
        if (args?.target_key === undefined) {
          if (hasConnectableHost) actions.showConnectHost();
          return;
        }
        const target = targetFor(args);
        if (target && canConnectHost(target)) actions.connectHost(target);
      },
    },
    {
      id: COMMAND_IDS.removeHost,
      category: message("command.category_host"),
      title: message("command.remove_active_host"),
      detail: t("command.remove_active_host_detail"),
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
      id: COMMAND_IDS.managePortForwards,
      category: message("command.category_host"),
      title: message("command.manage_ports"),
      keywords: ["ssh", "local", "tunnel", "forward"],
      enabled: targetFor()?.kind === "ssh" && !daemonRestartInteractionBlocked,
      isEnabled: (args) =>
        targetFor(args)?.kind === "ssh" && !daemonRestartInteractionBlocked,
      focusTerminalAfterRun: false,
      run: (args) => {
        const target = targetFor(args);
        if (target?.kind === "ssh") actions.managePortForwards(target);
      },
    },
    {
      id: COMMAND_IDS.saveWorkspace,
      category: message("command.category_workspace"),
      title: message("command.retry_workspace"),
      enabled: !daemonRestartInteractionBlocked,
      run: actions.saveWorkspace,
    },
    {
      id: COMMAND_IDS.configureKeybindings,
      category: message("command.category_settings"),
      title: message("command.configure_shortcuts"),
      keywords: ["keybindings", "keys", "settings"],
      enabled: true,
      focusTerminalAfterRun: false,
      run: actions.configureKeybindings,
    },
    {
      id: COMMAND_IDS.reloadKeybindings,
      category: message("command.category_settings"),
      title: message("command.reload_shortcuts"),
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
        ? message("command.reconnect_session", { v1: session.name })
        : message("command.attach_session", { v1: session.name })
      : message("command.switch_session", { v1: session.name });
    const detail = selected
      ? attaching
        ? t("command.attaching")
        : sessionAttached
          ? t("command.current_session")
          : ended
            ? t("command.ended_session")
            : t("command.retry_attachment")
      : `${targetLabel(session.target, locale)} · ${intl.formatNumber(session.terminal_size.columns)}×${intl.formatNumber(session.terminal_size.rows)} · ${t(`command.session_status_${session.status}`)}`;
    commands.push({
      id: sessionSwitchCommandId(session),
      allow_concurrent: true,
      category: message("command.category_session"),
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
          ? t("command.session_closing")
          : attaching
            ? t("command.session_attaching")
            : sessionAttached
              ? t("command.session_active")
              : t("command.session_ended"),
      run: () => actions.selectSession(session),
    });
  }

  const defaults = defaultKeybindings(context.shortcutPlatform);
  return commands.map((command) => ({
    ...localizeCommand(locale, command),
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
