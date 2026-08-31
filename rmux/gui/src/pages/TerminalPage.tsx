import { useCallback, useEffect, useRef, useState } from "react";
import { CommandPalette } from "../components/commands/CommandPalette";
import { SessionSidebar } from "../components/sessions/SessionSidebar";
import { StatusBar } from "../components/status/StatusBar";
import { TerminalTabs } from "../components/tabs/TerminalTabs";
import { TerminalSurface } from "../components/terminal/TerminalSurface";
import { TerminalToolbar } from "../components/terminal/TerminalToolbar";
import { useAttachment } from "../features/attachment/useAttachment";
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
import { errorCode, errorMessage } from "../lib/errors";
import {
  createSession,
  killSession,
  listSessions,
} from "../lib/tauri";
import type { SessionSummary, TerminalSize } from "../lib/types";

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
  const currentWorkingDirectory = attachment.state.shell_state?.cwd || null;
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [tabs, setTabs] = useState<SessionSummary[]>([]);
  const [activeTabId, setActiveTabId] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [listError, setListError] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [createFormOpen, setCreateFormOpen] = useState(false);
  const [paletteOpen, setPaletteOpen] = useState(false);
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

  const refresh = useCallback(async () => {
    const token = refreshGuardRef.current.begin();
    setLoading(true);
    setListError(null);
    try {
      const listed = await listSessions();
      if (!refreshGuardRef.current.canApply(token)) {
        return;
      }
      const listedIds = new Set(listed.map((session) => session.session_id));
      const hidden = closedSessionIdsRef.current;
      const visible = replaceSessionList(
        listed.filter((session) => !hidden.has(session.session_id)),
      );
      setSessions(visible);
      setTabs((current) => {
        const next = reconcileTerminalTabs(
          current,
          visible,
          activeTabIdRef.current,
        );
        tabsRef.current = next;
        return next;
      });
      for (const sessionId of hidden) {
        if (!listedIds.has(sessionId)) {
          hidden.delete(sessionId);
        }
      }
    } catch (error) {
      if (refreshGuardRef.current.canApply(token)) {
        setListError(errorMessage(error));
      }
    } finally {
      if (refreshGuardRef.current.isLatest(token)) {
        setLoading(false);
      }
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const activateTab = useCallback(
    async (session: SessionSummary, resizeWithWindow = false) => {
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
          await attachment.reconnect();
          return;
        }
      }
      await attachment.connect(session, {
        resize_with_window: resizeWithWindow,
      });
    },
    [attachment, renderer],
  );

  const closeTab = useCallback(
    async (session: SessionSummary) => {
      const currentTabs = tabsRef.current;
      if (!currentTabs.some((tab) => tab.session_id === session.session_id)) {
        return;
      }

      const wasActive = activeTabIdRef.current === session.session_id;
      const closed = closeTerminalTab(currentTabs, session.session_id);
      tabsRef.current = closed.tabs;
      setTabs(closed.tabs);
      if (!wasActive) {
        return;
      }

      const nextTab = closed.nextTab;
      activeTabIdRef.current = nextTab?.session_id ?? null;
      setActiveTabId(nextTab?.session_id ?? null);
      if (nextTab) {
        await attachment.connect(nextTab);
      } else {
        await attachment.detach();
      }
    },
    [attachment],
  );

  const create = useCallback(
    async (workingDirectory: string | null): Promise<boolean> => {
      if (creatingRef.current) {
        return false;
      }
      creatingRef.current = true;
      setCreating(true);
      setListError(null);
      try {
        const session = await createSession({
          working_directory: workingDirectory,
          terminal_size: measuredSize(renderer),
        });
        refreshGuardRef.current.recordMutation();
        setSessions((current) => prependSession(current, session));
        await activateTab(session, true);
        return true;
      } catch (error) {
        setListError(errorMessage(error));
        return false;
      } finally {
        creatingRef.current = false;
        setCreating(false);
      }
    },
    [activateTab, renderer],
  );

  const disconnect = useCallback(
    async (session: SessionSummary) => {
      if (activeTabIdRef.current !== session.session_id) {
        return;
      }
      setDisconnectingSessionId(session.session_id);
      setListError(null);
      try {
        await closeTab(session);
      } finally {
        setDisconnectingSessionId((current) =>
          current === session.session_id ? null : current,
        );
      }
    },
    [closeTab],
  );

  const close = useCallback(
    async (session: SessionSummary) => {
      if (closingSessionIdsRef.current.has(session.session_id)) {
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
          if (errorCode(error) !== "session_not_found") {
            setListError(errorMessage(error));
            return;
          }
        }
        refreshGuardRef.current.recordMutation();
        closedSessionIdsRef.current.add(session.session_id);
        setSessions((current) => removeSession(current, session.session_id));
        await closeTab(session);
        // The session is hidden after either an accepted kill or a not-found
        // response, which means another actor already achieved the same result.
      } finally {
        closingSessionIdsRef.current.delete(session.session_id);
        setClosingSessionIds((current) => {
          const next = new Set(current);
          next.delete(session.session_id);
          return next;
        });
      }
    },
    [attachment, closeTab],
  );

  const requestClose = useCallback((session: SessionSummary) => {
    setPendingCloseSessionId(session.session_id);
  }, []);

  const cancelClose = useCallback(() => {
    setPendingCloseSessionId(null);
  }, []);

  const confirmClose = useCallback(
    (session: SessionSummary) => {
      setPendingCloseSessionId(null);
      void close(session);
    },
    [close],
  );

  const attachedSession = attachment.state.session;
  const attachedTerminalSize = attachedSession?.terminal_size;
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
    void closeTab(attachedSession);
  }, [attachment.state.phase, attachedSession, closeTab]);

  const disconnectableSessionId =
    attachedSession && attachment.state.phase !== "ended"
      ? attachedSession.session_id
      : null;

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
      closingSessionIds,
      disconnectingSessionId,
      terminalReady: renderer !== null,
      currentWorkingDirectory,
      shortcutPlatform,
    },
    {
      showPalette: () => setPaletteOpen(true),
      showNewShellForm: () => setCreateFormOpen(true),
      openShellTab: () => {
        if (currentWorkingDirectory) {
          void create(currentWorkingDirectory);
        }
      },
      refreshSessions: () => void refresh(),
      selectSession: (session) => void activateTab(session),
      disconnectSession: (session) => void disconnect(session),
      requestCloseSession: requestClose,
      toggleInput: () => void attachment.toggleInputLease(),
      toggleResizeWithWindow: () =>
        void attachment.toggleResizeWithWindow(),
      reconnect: () => void attachment.reconnect(),
      focusTerminal: () => renderer?.focus(),
    },
  );
  const paletteShortcutLabel = formatKeybinding(
    SHOW_PALETTE_KEYBINDING,
    shortcutPlatform,
  );

  const executeCommand = useCallback(
    (command: AppCommand) => {
      setPaletteOpen(false);
      command.run();
      if (command.focusTerminalAfterRun !== false) {
        requestAnimationFrame(() => renderer?.focus());
      }
    },
    [renderer],
  );

  useCommandShortcuts(commands, shortcutPlatform, executeCommand);
  useNativeCommandEvents(commands, executeCommand);

  function executeCommandById(commandId: string) {
    const command = commands.find((candidate) => candidate.id === commandId);
    if (command?.enabled) {
      executeCommand(command);
    }
  }

  function dismissPalette() {
    setPaletteOpen(false);
    requestAnimationFrame(() => renderer?.focus());
  }

  return (
    <>
      <main className="app-shell">
        <SessionSidebar
          sessions={sessions}
          selectedSessionId={activeTabId}
          disconnectableSessionId={disconnectableSessionId}
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
              executeCommandById(COMMAND_IDS.newShell);
            } else {
              setCreateFormOpen(false);
            }
          }}
          onDisconnect={() => executeCommandById(COMMAND_IDS.disconnect)}
          onRequestClose={requestClose}
          onCancelClose={cancelClose}
          onConfirmClose={confirmClose}
        />
        <section className="terminal-workspace">
          <TerminalTabs
            tabs={tabs}
            activeSessionId={activeTabId}
            canCreate={currentWorkingDirectory !== null && !creating}
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
            onInput={attachment.handleInput}
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
