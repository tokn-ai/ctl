import { useCallback, useEffect, useRef, useState } from "react";
import { CommandPalette } from "../components/commands/CommandPalette";
import { SessionSidebar } from "../components/sessions/SessionSidebar";
import { StatusBar } from "../components/status/StatusBar";
import { TerminalTabs } from "../components/tabs/TerminalTabs";
import { TerminalSurface } from "../components/terminal/TerminalSurface";
import { TerminalToolbar } from "../components/terminal/TerminalToolbar";
import { useAttachment } from "../features/attachment/useAttachment";
import { restartFailurePreservesLocalState } from "../features/daemon/restartFailurePolicy";
import {
  detectShortcutPlatform,
  formatKeybinding,
} from "../features/commands/keybindings";
import {
  buildTerminalCommands,
  COMMAND_IDS,
  SHOW_PALETTE_KEYBINDING,
} from "../features/commands/terminalCommands";
import type { AppCommand } from "../features/commands/types";
import { useCommandShortcuts } from "../features/commands/useCommandShortcuts";
import { useNativeCommandEvents } from "../features/commands/useNativeCommandEvents";
import {
  forgetShellState,
  mergeShellStateInspections,
  rememberShellState,
  retainShellStates,
} from "../features/shell/shellStateCache";
import {
  SessionListRefreshGuard,
  prependSession,
  removeSession,
  replaceSessionList,
  syncSessionTerminalSize,
} from "../features/sessions/sessionListState";
import type { XtermRenderer } from "../features/terminal/XtermRenderer";
import {
  closeTerminalTab,
  openTerminalTab,
  reconcileTerminalTabs,
  syncTabTerminalSize,
} from "../features/tabs/tabState";
import {
  compactTerminalTitleParts,
  formatTerminalTitle,
} from "../features/tabs/terminalTitle";
import { useWindowTitle } from "../features/window/useWindowTitle";
import { errorCode, errorMessage } from "../lib/errors";
import { displayWorkingDirectory } from "../lib/shellState";
import {
  createSession,
  killSession,
  listSessions,
  restartLocalDaemon,
} from "../lib/tauri";
import type {
  SessionSummary,
  ShellStateSummary,
  TerminalSize,
} from "../lib/types";

function measuredSize(renderer: XtermRenderer | null): TerminalSize {
  const proposed = renderer?.proposeDimensions();
  return {
    columns: proposed?.columns ?? 80,
    rows: proposed?.rows ?? 24,
    pixel_width: null,
    pixel_height: null,
  };
}

export function TerminalPage() {
  const [renderer, setRenderer] = useState<XtermRenderer | null>(null);
  const [shortcutPlatform] = useState(detectShortcutPlatform);
  const attachment = useAttachment(renderer);
  const currentShellState = attachment.state.shell_state;
  const currentWorkingDirectory = currentShellState?.cwd || null;
  const currentWorkingDirectoryDisplay = currentShellState
    ? displayWorkingDirectory(currentShellState)
    : null;
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [tabs, setTabs] = useState<SessionSummary[]>([]);
  const [tabShellStates, setTabShellStates] = useState<
    ReadonlyMap<string, ShellStateSummary>
  >(() => new Map());
  const [sessionShellStates, setSessionShellStates] = useState<
    ReadonlyMap<string, ShellStateSummary>
  >(() => new Map());
  const [activeTabId, setActiveTabId] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [listError, setListError] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [createFormOpen, setCreateFormOpen] = useState(false);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [daemonRestartConfirmationPending, setDaemonRestartConfirmationPending] =
    useState(false);
  const [restartingDaemon, setRestartingDaemon] = useState(false);
  const [pendingCloseSessionId, setPendingCloseSessionId] = useState<
    string | null
  >(null);
  const [closingSessionIds, setClosingSessionIds] = useState<ReadonlySet<string>>(
    new Set(),
  );
  const [disconnectingSessionId, setDisconnectingSessionId] = useState<
    string | null
  >(null);
  const closingSessionIdsRef = useRef(new Set<string>());
  const closedSessionIdsRef = useRef(new Set<string>());
  const refreshGuardRef = useRef(new SessionListRefreshGuard());
  const tabsRef = useRef<SessionSummary[]>([]);
  const activeTabIdRef = useRef<string | null>(null);
  const creatingRef = useRef(false);
  const daemonRestartConfirmationRef = useRef(false);
  const restartingDaemonRef = useRef(false);
  const daemonEpochRef = useRef(0);

  const daemonRestartBlocksInteractions = useCallback(
    () =>
      daemonRestartConfirmationRef.current || restartingDaemonRef.current,
    [],
  );

  const refresh = useCallback(async (allowDuringDaemonRestart = false) => {
    if (!allowDuringDaemonRestart && daemonRestartBlocksInteractions()) {
      return;
    }
    const daemonEpoch = daemonEpochRef.current;
    const token = refreshGuardRef.current.begin();
    setLoading(true);
    setListError(null);
    try {
      const listedResponse = await listSessions();
      if (
        daemonEpoch !== daemonEpochRef.current ||
        !refreshGuardRef.current.canApply(token)
      ) {
        return;
      }
      const listed = listedResponse.sessions;
      const listedIds = new Set(listed.map((session) => session.session_id));
      const hidden = closedSessionIdsRef.current;
      const visible = replaceSessionList(
        listed.filter((session) => !hidden.has(session.session_id)),
      );
      const visibleIds = new Set(visible.map((session) => session.session_id));
      setSessions(visible);
      setSessionShellStates((current) =>
        mergeShellStateInspections(
          current,
          new Map(Object.entries(listedResponse.shell_states)),
          visibleIds,
        ),
      );
      const nextTabs = reconcileTerminalTabs(
        tabsRef.current,
        visible,
        activeTabIdRef.current,
      );
      tabsRef.current = nextTabs;
      setTabs(nextTabs);
      setTabShellStates((current) =>
        retainShellStates(
          current,
          new Set(nextTabs.map((session) => session.session_id)),
        ),
      );
      for (const sessionId of hidden) {
        if (!listedIds.has(sessionId)) {
          hidden.delete(sessionId);
        }
      }
    } catch (error) {
      if (
        daemonEpoch === daemonEpochRef.current &&
        refreshGuardRef.current.canApply(token)
      ) {
        setListError(errorMessage(error));
      }
    } finally {
      if (
        daemonEpoch === daemonEpochRef.current &&
        refreshGuardRef.current.isLatest(token)
      ) {
        setLoading(false);
      }
    }
  }, [daemonRestartBlocksInteractions]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const activateTab = useCallback(
    async (session: SessionSummary, resizeWithWindow = false) => {
      const daemonEpoch = daemonEpochRef.current;
      if (daemonRestartBlocksInteractions()) {
        return;
      }
      const nextTabs = openTerminalTab(tabsRef.current, session);
      tabsRef.current = nextTabs;
      activeTabIdRef.current = session.session_id;
      setTabs(nextTabs);
      setActiveTabId(session.session_id);

      if (attachment.state.session?.session_id === session.session_id) {
        if (
          attachment.state.phase === "attached" ||
          attachment.state.phase === "connecting" ||
          attachment.state.phase === "reconnecting"
        ) {
          renderer?.focus();
          return;
        }
        if (
          attachment.state.phase === "disconnected" ||
          attachment.state.phase === "error"
        ) {
          if (
            daemonEpoch !== daemonEpochRef.current ||
            daemonRestartBlocksInteractions()
          ) {
            return;
          }
          await attachment.reconnect();
          return;
        }
      }
      if (
        daemonEpoch !== daemonEpochRef.current ||
        daemonRestartBlocksInteractions()
      ) {
        return;
      }
      await attachment.connect(session, {
        resize_with_window: resizeWithWindow,
      });
    },
    [attachment, daemonRestartBlocksInteractions, renderer],
  );

  const closeTab = useCallback(
    async (session: SessionSummary) => {
      const daemonEpoch = daemonEpochRef.current;
      if (daemonRestartBlocksInteractions()) {
        return;
      }
      const currentTabs = tabsRef.current;
      if (!currentTabs.some((tab) => tab.session_id === session.session_id)) {
        return;
      }

      const wasActive = activeTabIdRef.current === session.session_id;
      const closed = closeTerminalTab(currentTabs, session.session_id);
      tabsRef.current = closed.tabs;
      setTabs(closed.tabs);
      setTabShellStates((current) =>
        forgetShellState(current, session.session_id),
      );
      if (!wasActive) {
        return;
      }

      const nextTab = closed.nextTab;
      activeTabIdRef.current = nextTab?.session_id ?? null;
      setActiveTabId(nextTab?.session_id ?? null);
      if (
        daemonEpoch !== daemonEpochRef.current ||
        daemonRestartBlocksInteractions()
      ) {
        return;
      }
      if (nextTab) {
        await attachment.connect(nextTab);
      } else {
        await attachment.detach();
      }
    },
    [attachment, daemonRestartBlocksInteractions],
  );

  const create = useCallback(
    async (workingDirectory: string | null): Promise<boolean> => {
      if (
        creatingRef.current ||
        daemonRestartConfirmationRef.current ||
        restartingDaemonRef.current
      ) {
        return false;
      }
      const daemonEpoch = daemonEpochRef.current;
      creatingRef.current = true;
      setCreating(true);
      setListError(null);
      try {
        const session = await createSession({
          working_directory: workingDirectory,
          terminal_size: measuredSize(renderer),
        });
        if (daemonEpoch !== daemonEpochRef.current) {
          return false;
        }
        refreshGuardRef.current.recordMutation();
        setSessions((current) => prependSession(current, session));
        await activateTab(session, true);
        return daemonEpoch === daemonEpochRef.current;
      } catch (error) {
        if (daemonEpoch === daemonEpochRef.current) {
          setListError(errorMessage(error));
        }
        return false;
      } finally {
        if (daemonEpoch === daemonEpochRef.current) {
          creatingRef.current = false;
          setCreating(false);
        }
      }
    },
    [activateTab, renderer],
  );

  const disconnect = useCallback(
    async (session: SessionSummary) => {
      const daemonEpoch = daemonEpochRef.current;
      if (
        daemonRestartBlocksInteractions() ||
        !tabsRef.current.some((tab) => tab.session_id === session.session_id)
      ) {
        return;
      }
      setDisconnectingSessionId(session.session_id);
      setListError(null);
      try {
        await closeTab(session);
      } finally {
        if (daemonEpoch === daemonEpochRef.current) {
          setDisconnectingSessionId((current) =>
            current === session.session_id ? null : current,
          );
        }
      }
    },
    [closeTab, daemonRestartBlocksInteractions],
  );

  const close = useCallback(
    async (session: SessionSummary) => {
      const daemonEpoch = daemonEpochRef.current;
      if (
        daemonRestartBlocksInteractions() ||
        closingSessionIdsRef.current.has(session.session_id)
      ) {
        return;
      }
      closingSessionIdsRef.current.add(session.session_id);
      setClosingSessionIds((current) => {
        const next = new Set(current);
        next.add(session.session_id);
        return next;
      });
      setListError(null);
      try {
        attachment.cancelPendingConnection(session.session_id);
        try {
          await killSession({ session_id: session.session_id });
        } catch (error) {
          if (daemonEpoch !== daemonEpochRef.current) {
            return;
          }
          if (errorCode(error) !== "session_not_found") {
            setListError(errorMessage(error));
            return;
          }
        }
        if (daemonEpoch !== daemonEpochRef.current) {
          return;
        }
        refreshGuardRef.current.recordMutation();
        closedSessionIdsRef.current.add(session.session_id);
        setSessions((current) => removeSession(current, session.session_id));
        setTabShellStates((current) =>
          forgetShellState(current, session.session_id),
        );
        setSessionShellStates((current) =>
          forgetShellState(current, session.session_id),
        );
        await closeTab(session);
        // The session is hidden after either an accepted kill or a not-found
        // response, which means another actor already achieved the same result.
      } finally {
        closingSessionIdsRef.current.delete(session.session_id);
        if (daemonEpoch === daemonEpochRef.current) {
          setClosingSessionIds((current) => {
            const next = new Set(current);
            next.delete(session.session_id);
            return next;
          });
        }
      }
    },
    [attachment, closeTab, daemonRestartBlocksInteractions],
  );

  const requestClose = useCallback((session: SessionSummary) => {
    if (daemonRestartBlocksInteractions()) {
      return;
    }
    setPendingCloseSessionId(session.session_id);
  }, [daemonRestartBlocksInteractions]);

  const cancelClose = useCallback(() => {
    setPendingCloseSessionId(null);
  }, []);

  const confirmClose = useCallback(
    (session: SessionSummary) => {
      if (daemonRestartBlocksInteractions()) {
        return;
      }
      setPendingCloseSessionId(null);
      void close(session);
    },
    [close, daemonRestartBlocksInteractions],
  );

  const setDaemonRestartConfirmation = useCallback((pending: boolean) => {
    daemonRestartConfirmationRef.current = pending;
    setDaemonRestartConfirmationPending(pending);
  }, []);

  const clearLocalDaemonState = useCallback(() => {
    daemonEpochRef.current += 1;
    refreshGuardRef.current.recordMutation();
    closedSessionIdsRef.current.clear();
    closingSessionIdsRef.current.clear();
    creatingRef.current = false;
    tabsRef.current = [];
    activeTabIdRef.current = null;
    attachment.resetAfterDaemonRestart();
    setSessions([]);
    setTabs([]);
    setTabShellStates(new Map<string, ShellStateSummary>());
    setSessionShellStates(new Map<string, ShellStateSummary>());
    setActiveTabId(null);
    setCreating(false);
    setCreateFormOpen(false);
    setPendingCloseSessionId(null);
    setClosingSessionIds(new Set());
    setDisconnectingSessionId(null);
    setLoading(true);
    setListError(null);
  }, [attachment]);

  const restartDaemon = useCallback(async () => {
    if (restartingDaemonRef.current) {
      return;
    }
    restartingDaemonRef.current = true;
    setDaemonRestartConfirmation(false);
    setRestartingDaemon(true);
    try {
      await restartLocalDaemon();
      clearLocalDaemonState();
      await refresh(true);
    } catch (error) {
      if (!restartFailurePreservesLocalState(errorCode(error))) {
        clearLocalDaemonState();
        await refresh(true);
      }
      setListError(`Could not restart rmuxd: ${errorMessage(error)}`);
    } finally {
      restartingDaemonRef.current = false;
      setRestartingDaemon(false);
      setLoading(false);
    }
  }, [clearLocalDaemonState, refresh, setDaemonRestartConfirmation]);

  const requestDaemonRestart = useCallback(() => {
    if (restartingDaemonRef.current) {
      return;
    }
    setDaemonRestartConfirmation(true);
  }, [setDaemonRestartConfirmation]);

  const cancelDaemonRestart = useCallback(() => {
    if (!daemonRestartConfirmationRef.current) {
      return;
    }
    setDaemonRestartConfirmation(false);
  }, [setDaemonRestartConfirmation]);

  const confirmDaemonRestart = useCallback(() => {
    if (
      !daemonRestartConfirmationRef.current ||
      restartingDaemonRef.current
    ) {
      return;
    }
    void restartDaemon();
  }, [restartDaemon]);

  const attachedSession = attachment.state.session;
  const attachedTerminalSize = attachedSession?.terminal_size;
  const attachedShellState = attachment.state.shell_state;
  useEffect(() => {
    if (!attachedSession || !attachedShellState) {
      return;
    }
    if (
      !tabsRef.current.some(
        (tab) => tab.session_id === attachedSession.session_id,
      )
    ) {
      return;
    }
    setTabShellStates((current) =>
      rememberShellState(
        current,
        attachedSession.session_id,
        attachedShellState,
        { replaceEqualRevision: true },
      ),
    );
    setSessionShellStates((current) =>
      rememberShellState(
        current,
        attachedSession.session_id,
        attachedShellState,
        { replaceEqualRevision: true },
      ),
    );
  }, [attachedSession?.session_id, attachedShellState]);

  useEffect(() => {
    if (!attachedSession || !attachedTerminalSize) {
      return;
    }
    refreshGuardRef.current.recordMutation();
    setSessions((current) =>
      syncSessionTerminalSize(
        current,
        attachedSession.session_id,
        attachedTerminalSize,
      ),
    );
    setTabs((current) => {
      const next = syncTabTerminalSize(
        current,
        attachedSession.session_id,
        attachedTerminalSize,
      );
      tabsRef.current = next;
      return next;
    });
  }, [
    attachedSession?.session_id,
    attachedTerminalSize?.columns,
    attachedTerminalSize?.rows,
    attachedTerminalSize?.pixel_width,
    attachedTerminalSize?.pixel_height,
  ]);

  useEffect(() => {
    if (attachment.state.phase !== "ended" || !attachedSession) {
      return;
    }
    refreshGuardRef.current.recordMutation();
    closedSessionIdsRef.current.add(attachedSession.session_id);
    setSessions((current) => removeSession(current, attachedSession.session_id));
    setTabShellStates((current) =>
      forgetShellState(current, attachedSession.session_id),
    );
    setSessionShellStates((current) =>
      forgetShellState(current, attachedSession.session_id),
    );
    void closeTab(attachedSession);
  }, [attachment.state.phase, attachedSession, closeTab]);

  const openTabSessionIds = new Set(
    tabs.map((tab) => tab.session_id),
  );

  const activeTab = tabs.find((tab) => tab.session_id === activeTabId) ?? null;
  const activeShellState =
    attachedSession?.session_id === activeTabId && attachedShellState
      ? attachedShellState
      : activeTabId
        ? tabShellStates.get(activeTabId) ?? null
        : null;
  const displayedTabShellStates =
    attachedSession && attachedShellState
      ? rememberShellState(
          tabShellStates,
          attachedSession.session_id,
          attachedShellState,
          { replaceEqualRevision: true },
      )
      : tabShellStates;
  const displayedSessionShellStates = new Map(sessionShellStates);
  if (attachedSession && attachedShellState) {
    displayedSessionShellStates.set(
      attachedSession.session_id,
      attachedShellState,
    );
  }
  const activeTitle = formatTerminalTitle(activeTab, activeShellState);
  useWindowTitle(compactTerminalTitleParts(activeTitle));

  const commands = buildTerminalCommands(
    {
      sessions,
      tabs,
      activeSessionId: activeTabId,
      attachmentSessionId: attachment.state.session?.session_id ?? null,
      phase: attachment.state.phase,
      inputOwned: attachment.state.input_lease.owned_by_client,
      resizeWithWindow: attachment.state.resize_with_window,
      listLoading: loading,
      creating,
      createFormOpen,
      pendingCloseSessionId,
      closingSessionIds,
      disconnectingSessionId,
      terminalReady: renderer !== null,
      currentWorkingDirectory,
      currentWorkingDirectoryDisplay,
      daemonRestartConfirmationPending,
      restartingDaemon,
      shortcutPlatform,
    },
    {
      showPalette: () => setPaletteOpen(true),
      showNewShellForm: () => {
        if (!daemonRestartBlocksInteractions()) {
          setCreateFormOpen(true);
        }
      },
      openShellTab: () => {
        if (currentWorkingDirectory) {
          void create(currentWorkingDirectory);
        }
      },
      refreshSessions: () => void refresh(),
      selectSession: (session) => void activateTab(session),
      disconnectSession: (session) => void disconnect(session),
      requestCloseSession: requestClose,
      confirmCloseSession: confirmClose,
      toggleInput: () => {
        if (!daemonRestartBlocksInteractions()) {
          void attachment.toggleInputLease();
        }
      },
      toggleResizeWithWindow: () => {
        if (!daemonRestartBlocksInteractions()) {
          void attachment.toggleResizeWithWindow();
        }
      },
      reconnect: () => {
        if (!daemonRestartBlocksInteractions()) {
          void attachment.reconnect();
        }
      },
      focusTerminal: () => renderer?.focus(),
      requestDaemonRestart,
      confirmDaemonRestart,
    },
  );
  const paletteShortcutLabel = formatKeybinding(
    SHOW_PALETTE_KEYBINDING,
    shortcutPlatform,
  );

  const executeCommand = useCallback(
    (command: AppCommand) => {
      if (!command.keepPaletteOpen) {
        setPaletteOpen(false);
      }
      if (command.id !== COMMAND_IDS.restartDaemon) {
        cancelDaemonRestart();
      }
      command.run();
      if (command.focusTerminalAfterRun !== false) {
        requestAnimationFrame(() => renderer?.focus());
      }
    },
    [cancelDaemonRestart, renderer],
  );

  useCommandShortcuts(commands, shortcutPlatform, executeCommand);
  useNativeCommandEvents(commands, executeCommand);

  function executeCommandById(commandId: string) {
    const command = commands.find((candidate) => candidate.id === commandId);
    if (command?.enabled) {
      executeCommand(command);
    }
  }

  const handleTerminalInput = useCallback(
    (data: Uint8Array) => {
      if (!daemonRestartBlocksInteractions()) {
        attachment.handleInput(data);
      }
    },
    [attachment, daemonRestartBlocksInteractions],
  );

  function dismissPalette() {
    setPaletteOpen(false);
    cancelDaemonRestart();
    requestAnimationFrame(() => renderer?.focus());
  }

  return (
    <>
      <main className="app-shell">
        <SessionSidebar
          sessions={sessions}
          shellStates={displayedSessionShellStates}
          selectedSessionId={activeTabId}
          openTabSessionIds={openTabSessionIds}
          loading={loading}
          error={listError}
          creating={creating}
          createFormOpen={createFormOpen}
          pendingCloseSessionId={pendingCloseSessionId}
          closingSessionIds={closingSessionIds}
          disconnectingSessionId={disconnectingSessionId}
          onRefresh={() => executeCommandById(COMMAND_IDS.refreshSessions)}
          onSelect={(session) =>
            executeCommandById(`session.switch.${session.session_id}`)
          }
          onCreate={create}
          onCreateFormOpenChange={(open) => {
            if (open) {
              if (!daemonRestartBlocksInteractions()) {
                executeCommandById(COMMAND_IDS.newShell);
              }
            } else {
              setCreateFormOpen(false);
            }
          }}
          onDisconnect={(session) => void disconnect(session)}
          onRequestClose={requestClose}
          onCancelClose={cancelClose}
          onConfirmClose={confirmClose}
        />
        <section className="terminal-workspace">
          <TerminalTabs
            tabs={tabs}
            shellStates={displayedTabShellStates}
            activeSessionId={activeTabId}
            canCreate={
              currentWorkingDirectory !== null &&
              !creating &&
              !daemonRestartConfirmationPending &&
              !restartingDaemon
            }
            onSelect={(session) => void activateTab(session)}
            onClose={(session) => void closeTab(session)}
            onCreate={() => executeCommandById(COMMAND_IDS.newTab)}
          />
          <TerminalToolbar
            state={attachment.state}
            onToggleInput={() =>
              executeCommandById(COMMAND_IDS.toggleInput)
            }
            onToggleResizeWithWindow={() =>
              executeCommandById(COMMAND_IDS.toggleResize)
            }
            onReconnect={() => executeCommandById(COMMAND_IDS.reconnect)}
            onShowCommands={() =>
              executeCommandById(COMMAND_IDS.showPalette)
            }
            commandShortcutLabel={paletteShortcutLabel}
          />
          <div className="terminal-notices">
            {attachment.state.history_gap ? (
              <div className="history-gap-banner" role="status">
                Earlier remote output is no longer contiguous. The live screen was restored from a
                checkpoint.
              </div>
            ) : null}
            {attachment.state.message ? (
              <div className="message-banner" role="status">{attachment.state.message}</div>
            ) : null}
          </div>
          <TerminalSurface
            phase={attachment.state.phase}
            hasSession={attachment.state.session !== null}
            onInput={handleTerminalInput}
            onReady={setRenderer}
          />
          <StatusBar state={attachment.state} />
        </section>
      </main>
      {paletteOpen ? (
        <CommandPalette
          commands={commands}
          platform={shortcutPlatform}
          onDismiss={dismissPalette}
          onExecute={executeCommand}
        />
      ) : null}
    </>
  );
}
