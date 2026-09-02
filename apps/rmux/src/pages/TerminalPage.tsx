import { useCallback, useEffect, useRef, useState } from "react";
import { QuickInput } from "../components/commands/QuickInput";
import { SshHostFlow } from "../components/sessions/SshHostFlow";
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
  sessionSwitchCommandId,
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
  mergeTargetSessionLists,
  prependSession,
  removeSession,
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
import {
  LOCAL_TARGET,
  appLocalSshTarget,
  browserStorage,
  configuredSshTarget,
  inactiveSshConfigDestinations,
  loadRemoteTargets,
  normalizeSshDestination,
  sameSession,
  sameTarget,
  saveRemoteTargets,
  sessionKey,
  targetKey,
  targetKeyFromSessionKey,
} from "../features/targets/targets";
import { useWindowTitle } from "../features/window/useWindowTitle";
import { errorCode, errorMessage } from "../lib/errors";
import { displayWorkingDirectory } from "../lib/shellState";
import {
  createSession,
  killSession,
  listSessions,
  listSshConfigHosts,
  restartLocalDaemon,
  saveSshConfigHost,
  forgetSshCredentials,
} from "../lib/tauri";
import type {
  ConnectionTarget,
  SessionSummary,
  ShellStateSummary,
  SshConfigHost,
  SshHostDefinition,
  SshHostStorage,
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
  const [targets, setTargets] = useState<ConnectionTarget[]>(() => [
    LOCAL_TARGET,
    ...loadRemoteTargets(browserStorage()),
  ]);
  const [sshConfigHosts, setSshConfigHosts] = useState<SshConfigHost[]>([]);
  const [sshConfigWarning, setSshConfigWarning] = useState<string | null>(null);
  const [targetErrors, setTargetErrors] = useState<ReadonlyMap<string, string>>(
    () => new Map(),
  );
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [tabs, setTabs] = useState<SessionSummary[]>([]);
  const [tabShellStates, setTabShellStates] = useState<
    ReadonlyMap<string, ShellStateSummary>
  >(() => new Map());
  const [sessionShellStates, setSessionShellStates] = useState<
    ReadonlyMap<string, ShellStateSummary>
  >(() => new Map());
  const [activeTabKey, setActiveTabKey] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [listError, setListError] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [createFormOpen, setCreateFormOpen] = useState(false);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [hostFlow, setHostFlow] = useState<ConnectionTarget | null | undefined>(
    undefined,
  );
  const [
    daemonRestartConfirmationPending,
    setDaemonRestartConfirmationPending,
  ] = useState(false);
  const [restartingDaemon, setRestartingDaemon] = useState(false);
  const [pendingCloseSessionKey, setPendingCloseSessionKey] = useState<
    string | null
  >(null);
  const [closingSessionKeys, setClosingSessionKeys] = useState<
    ReadonlySet<string>
  >(new Set());
  const [disconnectingSessionKey, setDisconnectingSessionKey] = useState<
    string | null
  >(null);
  const closingSessionKeysRef = useRef(new Set<string>());
  const closedSessionKeysRef = useRef(new Set<string>());
  const refreshGuardRef = useRef(new SessionListRefreshGuard());
  const sessionsRef = useRef<SessionSummary[]>([]);
  const tabsRef = useRef<SessionSummary[]>([]);
  const activeTabKeyRef = useRef<string | null>(null);
  const creatingRef = useRef(false);
  const daemonRestartConfirmationRef = useRef(false);
  const restartingDaemonRef = useRef(false);
  const daemonEpochRef = useRef(0);

  const daemonRestartBlocksInteractions = useCallback(
    () => daemonRestartConfirmationRef.current || restartingDaemonRef.current,
    [],
  );

  useEffect(() => {
    let cancelled = false;
    void listSshConfigHosts()
      .then((catalog) => {
        if (cancelled) {
          return;
        }
        setSshConfigHosts(catalog.hosts);
        setSshConfigWarning(catalog.warnings[0] ?? null);
      })
      .catch((error: unknown) => {
        if (!cancelled) {
          setSshConfigWarning(errorMessage(error));
        }
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const refresh = useCallback(
    async (allowDuringDaemonRestart = false) => {
      if (!allowDuringDaemonRestart && daemonRestartBlocksInteractions()) {
        return;
      }
      const daemonEpoch = daemonEpochRef.current;
      const token = refreshGuardRef.current.begin();
      setLoading(true);
      setListError(null);
      try {
        const results = await Promise.all(
          targets.map(async (target) => {
            try {
              return { target, response: await listSessions(target) } as const;
            } catch (error) {
              return { target, error } as const;
            }
          }),
        );
        if (
          daemonEpoch !== daemonEpochRef.current ||
          !refreshGuardRef.current.canApply(token)
        ) {
          return;
        }
        const refreshed = new Map<string, SessionSummary[]>();
        const inspections = new Map<string, ShellStateSummary>();
        const errors = new Map<string, string>();
        const successfulTargets = new Set<string>();
        const listedSessionKeys = new Set<string>();
        for (const result of results) {
          const key = targetKey(result.target);
          if ("error" in result) {
            errors.set(key, errorMessage(result.error));
            continue;
          }
          successfulTargets.add(key);
          refreshed.set(key, result.response.sessions);
          for (const session of result.response.sessions) {
            listedSessionKeys.add(sessionKey(session));
            const shellState = result.response.shell_states[session.session_id];
            if (shellState) {
              inspections.set(sessionKey(session), shellState);
            }
          }
        }
        const hidden = closedSessionKeysRef.current;
        const visible = mergeTargetSessionLists(
          sessionsRef.current,
          targets,
          refreshed,
        ).filter((session) => !hidden.has(sessionKey(session)));
        const visibleIds = new Set(visible.map(sessionKey));
        sessionsRef.current = visible;
        setSessions(visible);
        setTargetErrors(errors);
        setSessionShellStates((current) =>
          mergeShellStateInspections(current, inspections, visibleIds),
        );
        const nextTabs = reconcileTerminalTabs(
          tabsRef.current,
          visible,
          activeTabKeyRef.current,
        );
        tabsRef.current = nextTabs;
        setTabs(nextTabs);
        setTabShellStates((current) =>
          retainShellStates(current, new Set(nextTabs.map(sessionKey))),
        );
        for (const identity of hidden) {
          const hiddenTarget = targetKeyFromSessionKey(identity);
          if (
            hiddenTarget &&
            successfulTargets.has(hiddenTarget) &&
            !listedSessionKeys.has(identity)
          ) {
            hidden.delete(identity);
          }
        }
      } finally {
        if (
          daemonEpoch === daemonEpochRef.current &&
          refreshGuardRef.current.isLatest(token)
        ) {
          setLoading(false);
        }
      }
    },
    [daemonRestartBlocksInteractions, targets],
  );

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const addTarget = useCallback(
    (target: ConnectionTarget): boolean => {
      if (
        target.kind === "local" ||
        !normalizeSshDestination(target.destination) ||
        targets.some((candidate) => sameTarget(candidate, target))
      ) {
        return false;
      }
      const next = [...targets, target];
      saveRemoteTargets(browserStorage(), next);
      setTargets(next);
      return true;
    },
    [targets],
  );

  const activateConfiguredHost = useCallback(
    (destination: string): boolean => {
      const target = configuredSshTarget(destination);
      return target ? addTarget(target) : false;
    },
    [addTarget],
  );

  const saveHost = useCallback(
    async (
      definition: SshHostDefinition,
      storage: SshHostStorage,
    ): Promise<void> => {
      if (
        targets.some(
          (target) =>
            target.kind === "ssh" && target.destination === definition.alias,
        )
      ) {
        throw new Error(`Host ${definition.alias} is already active.`);
      }
      const target =
        storage === "ssh_config"
          ? configuredSshTarget(
              (await saveSshConfigHost(definition)).destination,
            )
          : appLocalSshTarget(definition);
      if (!target || !addTarget(target)) {
        throw new Error("The host settings could not be saved.");
      }
      if (storage === "ssh_config") {
        setSshConfigHosts((current) =>
          current.some((host) => host.destination === target.destination)
            ? current
            : [...current, { destination: target.destination }],
        );
      }
    },
    [addTarget, targets],
  );

  const hostSuggestions = inactiveSshConfigDestinations(
    sshConfigHosts,
    targets,
  );

  const activateTab = useCallback(
    async (session: SessionSummary, resizeWithWindow = false) => {
      const daemonEpoch = daemonEpochRef.current;
      if (daemonRestartBlocksInteractions()) {
        return;
      }
      const identity = sessionKey(session);
      const nextTabs = openTerminalTab(tabsRef.current, session);
      tabsRef.current = nextTabs;
      activeTabKeyRef.current = identity;
      setTabs(nextTabs);
      setActiveTabKey(identity);

      if (sameSession(attachment.state.session, session)) {
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
      const identity = sessionKey(session);
      const currentTabs = tabsRef.current;
      if (!currentTabs.some((tab) => sessionKey(tab) === identity)) {
        return;
      }

      const wasActive = activeTabKeyRef.current === identity;
      const closed = closeTerminalTab(currentTabs, identity);
      tabsRef.current = closed.tabs;
      setTabs(closed.tabs);
      setTabShellStates((current) => forgetShellState(current, identity));
      if (!wasActive) {
        return;
      }

      const nextTab = closed.nextTab;
      activeTabKeyRef.current = nextTab ? sessionKey(nextTab) : null;
      setActiveTabKey(nextTab ? sessionKey(nextTab) : null);
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

  const removeHost = useCallback(
    async (target: ConnectionTarget) => {
      if (target.kind === "local" || daemonRestartBlocksInteractions()) {
        return;
      }
      void forgetSshCredentials(target).catch(() => undefined);
      const removedTargetKey = targetKey(target);
      const activeTab = tabsRef.current.find(
        (tab) => sessionKey(tab) === activeTabKeyRef.current,
      );
      const removingActive = activeTab
        ? sameTarget(activeTab.target, target)
        : false;
      if (removingActive) {
        await attachment.detach();
      }

      const nextTargets = targets.filter(
        (candidate) => !sameTarget(candidate, target),
      );
      const nextSessions = sessionsRef.current.filter(
        (session) => !sameTarget(session.target, target),
      );
      const nextTabs = tabsRef.current.filter(
        (tab) => !sameTarget(tab.target, target),
      );
      const nextActive = removingActive
        ? (nextTabs[0] ?? null)
        : (activeTab ?? null);
      const nextActiveKey = nextActive ? sessionKey(nextActive) : null;

      refreshGuardRef.current.recordMutation();
      sessionsRef.current = nextSessions;
      tabsRef.current = nextTabs;
      activeTabKeyRef.current = nextActiveKey;
      setTargets(nextTargets);
      saveRemoteTargets(browserStorage(), nextTargets);
      setSessions(nextSessions);
      setTabs(nextTabs);
      setActiveTabKey(nextActiveKey);
      setSessionShellStates((current) =>
        retainShellStates(current, new Set(nextSessions.map(sessionKey))),
      );
      setTabShellStates((current) =>
        retainShellStates(current, new Set(nextTabs.map(sessionKey))),
      );
      setTargetErrors((current) => {
        const next = new Map(current);
        next.delete(removedTargetKey);
        return next;
      });
      if (removingActive && nextActive) {
        await attachment.connect(nextActive);
      }
    },
    [attachment, daemonRestartBlocksInteractions, targets],
  );

  const create = useCallback(
    async (
      target: ConnectionTarget,
      workingDirectory: string | null,
    ): Promise<boolean> => {
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
          target,
          working_directory: workingDirectory,
          terminal_size: measuredSize(renderer),
        });
        if (daemonEpoch !== daemonEpochRef.current) {
          return false;
        }
        refreshGuardRef.current.recordMutation();
        setSessions((current) => {
          const next = prependSession(current, session);
          sessionsRef.current = next;
          return next;
        });
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
      const identity = sessionKey(session);
      if (
        daemonRestartBlocksInteractions() ||
        !tabsRef.current.some((tab) => sessionKey(tab) === identity)
      ) {
        return;
      }
      setDisconnectingSessionKey(identity);
      setListError(null);
      try {
        await closeTab(session);
      } finally {
        if (daemonEpoch === daemonEpochRef.current) {
          setDisconnectingSessionKey((current) =>
            current === identity ? null : current,
          );
        }
      }
    },
    [closeTab, daemonRestartBlocksInteractions],
  );

  const close = useCallback(
    async (session: SessionSummary) => {
      const daemonEpoch = daemonEpochRef.current;
      const identity = sessionKey(session);
      if (
        daemonRestartBlocksInteractions() ||
        closingSessionKeysRef.current.has(identity)
      ) {
        return;
      }
      closingSessionKeysRef.current.add(identity);
      setClosingSessionKeys((current) => {
        const next = new Set(current);
        next.add(identity);
        return next;
      });
      setListError(null);
      try {
        attachment.cancelPendingConnection(session);
        try {
          await killSession({
            target: session.target,
            session_id: session.session_id,
          });
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
        closedSessionKeysRef.current.add(identity);
        setSessions((current) => {
          const next = removeSession(current, identity);
          sessionsRef.current = next;
          return next;
        });
        setTabShellStates((current) => forgetShellState(current, identity));
        setSessionShellStates((current) => forgetShellState(current, identity));
        await closeTab(session);
        // The session is hidden after either an accepted kill or a not-found
        // response, which means another actor already achieved the same result.
      } finally {
        closingSessionKeysRef.current.delete(identity);
        if (daemonEpoch === daemonEpochRef.current) {
          setClosingSessionKeys((current) => {
            const next = new Set(current);
            next.delete(identity);
            return next;
          });
        }
      }
    },
    [attachment, closeTab, daemonRestartBlocksInteractions],
  );

  const requestClose = useCallback(
    (session: SessionSummary) => {
      if (daemonRestartBlocksInteractions()) {
        return;
      }
      setPaletteOpen(false);
      setPendingCloseSessionKey(sessionKey(session));
    },
    [daemonRestartBlocksInteractions],
  );

  const cancelClose = useCallback(() => {
    setPendingCloseSessionKey(null);
    requestAnimationFrame(() => renderer?.focus());
  }, [renderer]);

  const confirmClose = useCallback(
    (session: SessionSummary) => {
      if (daemonRestartBlocksInteractions()) {
        return;
      }
      setPendingCloseSessionKey(null);
      void close(session);
    },
    [close, daemonRestartBlocksInteractions],
  );

  const setDaemonRestartConfirmation = useCallback((pending: boolean) => {
    daemonRestartConfirmationRef.current = pending;
    setDaemonRestartConfirmationPending(pending);
  }, []);

  const clearLocalDaemonState = useCallback((): SessionSummary | null => {
    daemonEpochRef.current += 1;
    refreshGuardRef.current.recordMutation();
    closedSessionKeysRef.current.clear();
    closingSessionKeysRef.current.clear();
    creatingRef.current = false;
    const retainedSessions = sessionsRef.current.filter(
      (session) => session.target.kind !== "local",
    );
    const activeTab = tabsRef.current.find(
      (tab) => sessionKey(tab) === activeTabKeyRef.current,
    );
    const activeWasLocal = activeTab?.target.kind === "local";
    const retainedTabs = tabsRef.current.filter(
      (tab) => tab.target.kind !== "local",
    );
    const nextActiveTab = activeWasLocal ? (retainedTabs[0] ?? null) : null;
    const nextActiveKey = activeWasLocal
      ? nextActiveTab
        ? sessionKey(nextActiveTab)
        : null
      : activeTabKeyRef.current;

    sessionsRef.current = retainedSessions;
    tabsRef.current = retainedTabs;
    activeTabKeyRef.current = nextActiveKey;
    if (attachment.state.session?.target.kind === "local") {
      attachment.resetAfterDaemonRestart();
    }
    setSessions(retainedSessions);
    setTabs(retainedTabs);
    setTabShellStates((current) =>
      retainShellStates(current, new Set(retainedTabs.map(sessionKey))),
    );
    setSessionShellStates((current) =>
      retainShellStates(current, new Set(retainedSessions.map(sessionKey))),
    );
    setActiveTabKey(nextActiveKey);
    setCreating(false);
    setCreateFormOpen(false);
    setPendingCloseSessionKey(null);
    setClosingSessionKeys(new Set());
    setDisconnectingSessionKey(null);
    setLoading(true);
    setListError(null);
    setTargetErrors((current) => {
      const next = new Map(current);
      next.delete("local");
      return next;
    });
    return nextActiveTab;
  }, [attachment]);

  const restartDaemon = useCallback(async () => {
    if (restartingDaemonRef.current) {
      return;
    }
    restartingDaemonRef.current = true;
    setDaemonRestartConfirmation(false);
    setRestartingDaemon(true);
    let nextActiveTab: SessionSummary | null = null;
    try {
      await restartLocalDaemon();
      nextActiveTab = clearLocalDaemonState();
      await refresh(true);
    } catch (error) {
      if (!restartFailurePreservesLocalState(errorCode(error))) {
        nextActiveTab = clearLocalDaemonState();
        await refresh(true);
      }
      setListError(`Could not restart rmuxd: ${errorMessage(error)}`);
    } finally {
      restartingDaemonRef.current = false;
      setRestartingDaemon(false);
      setLoading(false);
    }
    if (nextActiveTab) {
      await activateTab(nextActiveTab);
    }
  }, [
    activateTab,
    clearLocalDaemonState,
    refresh,
    setDaemonRestartConfirmation,
  ]);

  const requestDaemonRestart = useCallback(() => {
    if (restartingDaemonRef.current) {
      return;
    }
    setPaletteOpen(false);
    setDaemonRestartConfirmation(true);
  }, [setDaemonRestartConfirmation]);

  const cancelDaemonRestart = useCallback(() => {
    if (!daemonRestartConfirmationRef.current) {
      return;
    }
    setDaemonRestartConfirmation(false);
    requestAnimationFrame(() => renderer?.focus());
  }, [renderer, setDaemonRestartConfirmation]);

  const confirmDaemonRestart = useCallback(() => {
    if (!daemonRestartConfirmationRef.current || restartingDaemonRef.current) {
      return;
    }
    void restartDaemon();
  }, [restartDaemon]);

  const attachedSession = attachment.state.session;
  const attachedSessionKey = attachedSession
    ? sessionKey(attachedSession)
    : null;
  const attachedTerminalSize = attachedSession?.terminal_size;
  const attachedShellState = attachment.state.shell_state;
  useEffect(() => {
    if (!attachedSession || !attachedShellState) {
      return;
    }
    if (!tabsRef.current.some((tab) => sameSession(tab, attachedSession))) {
      return;
    }
    setTabShellStates((current) =>
      rememberShellState(
        current,
        sessionKey(attachedSession),
        attachedShellState,
        { replaceEqualRevision: true },
      ),
    );
    setSessionShellStates((current) =>
      rememberShellState(
        current,
        sessionKey(attachedSession),
        attachedShellState,
        { replaceEqualRevision: true },
      ),
    );
  }, [attachedSessionKey, attachedShellState]);

  useEffect(() => {
    if (!attachedSession || !attachedTerminalSize) {
      return;
    }
    refreshGuardRef.current.recordMutation();
    setSessions((current) => {
      const next = syncSessionTerminalSize(
        current,
        sessionKey(attachedSession),
        attachedTerminalSize,
      );
      sessionsRef.current = next;
      return next;
    });
    setTabs((current) => {
      const next = syncTabTerminalSize(
        current,
        sessionKey(attachedSession),
        attachedTerminalSize,
      );
      tabsRef.current = next;
      return next;
    });
  }, [
    attachedSessionKey,
    attachedTerminalSize?.columns,
    attachedTerminalSize?.rows,
    attachedTerminalSize?.pixel_width,
    attachedTerminalSize?.pixel_height,
  ]);

  useEffect(() => {
    if (attachment.state.phase !== "ended" || !attachedSession) {
      return;
    }
    const identity = sessionKey(attachedSession);
    refreshGuardRef.current.recordMutation();
    closedSessionKeysRef.current.add(identity);
    setSessions((current) => {
      const next = removeSession(current, identity);
      sessionsRef.current = next;
      return next;
    });
    setTabShellStates((current) => forgetShellState(current, identity));
    setSessionShellStates((current) => forgetShellState(current, identity));
    void closeTab(attachedSession);
  }, [attachment.state.phase, attachedSession, closeTab]);

  const openTabSessionKeys = new Set(tabs.map(sessionKey));

  const activeTab =
    tabs.find((tab) => sessionKey(tab) === activeTabKey) ?? null;
  const activeShellState =
    attachedSessionKey === activeTabKey && attachedShellState
      ? attachedShellState
      : activeTabKey
        ? (tabShellStates.get(activeTabKey) ?? null)
        : null;
  const displayedTabShellStates =
    attachedSession && attachedShellState
      ? rememberShellState(
          tabShellStates,
          sessionKey(attachedSession),
          attachedShellState,
          { replaceEqualRevision: true },
        )
      : tabShellStates;
  const displayedSessionShellStates = new Map(sessionShellStates);
  if (attachedSession && attachedShellState) {
    displayedSessionShellStates.set(
      sessionKey(attachedSession),
      attachedShellState,
    );
  }
  const activeTitle = formatTerminalTitle(activeTab, activeShellState);
  useWindowTitle(compactTerminalTitleParts(activeTitle));

  const commands = buildTerminalCommands(
    {
      sessions,
      tabs,
      activeSessionKey: activeTabKey,
      attachmentSessionKey: attachedSessionKey,
      phase: attachment.state.phase,
      inputOwned: attachment.state.input_lease.owned_by_client,
      resizeWithWindow: attachment.state.resize_with_window,
      listLoading: loading,
      creating,
      createFormOpen,
      pendingCloseSessionKey,
      closingSessionKeys,
      disconnectingSessionKey,
      terminalReady: renderer !== null,
      currentWorkingDirectory,
      currentWorkingDirectoryDisplay,
      daemonRestartConfirmationPending,
      restartingDaemon,
      shortcutPlatform,
    },
    {
      showPalette: () => setPaletteOpen(true),
      showAddHost: () => setHostFlow(null),
      showNewShellForm: () => {
        if (!daemonRestartBlocksInteractions()) {
          setCreateFormOpen(true);
        }
      },
      openShellTab: () => {
        if (currentWorkingDirectory && activeTab) {
          void create(activeTab.target, currentWorkingDirectory);
        }
      },
      refreshSessions: () => void refresh(),
      selectSession: (session) => void activateTab(session),
      disconnectSession: (session) => void disconnect(session),
      requestCloseSession: requestClose,
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
    },
  );
  const paletteShortcutLabel = formatKeybinding(
    SHOW_PALETTE_KEYBINDING,
    shortcutPlatform,
  );

  const executeCommand = useCallback(
    (command: AppCommand) => {
      if (
        hostFlow !== undefined ||
        pendingCloseSessionKey ||
        daemonRestartConfirmationPending
      )
        return;
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
    [
      cancelDaemonRestart,
      renderer,
      hostFlow,
      pendingCloseSessionKey,
      daemonRestartConfirmationPending,
    ],
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
          targets={targets}
          targetErrors={targetErrors}
          sessions={sessions}
          shellStates={displayedSessionShellStates}
          selectedSessionKey={activeTabKey}
          openTabSessionKeys={openTabSessionKeys}
          loading={loading}
          error={listError}
          creating={creating}
          createFormOpen={createFormOpen}
          closingSessionKeys={closingSessionKeys}
          disconnectingSessionKey={disconnectingSessionKey}
          onRefresh={() => executeCommandById(COMMAND_IDS.refreshSessions)}
          onSelect={(session) =>
            executeCommandById(sessionSwitchCommandId(session))
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
          onAddHost={() => executeCommandById(COMMAND_IDS.addHost)}
          onConnectHost={(target) => {
            if (target.kind === "ssh") {
              setPaletteOpen(false);
              setHostFlow(target);
            }
          }}
          onRemoveHost={(target) => void removeHost(target)}
        />
        <section className="terminal-workspace">
          <TerminalTabs
            tabs={tabs}
            shellStates={displayedTabShellStates}
            activeSessionKey={activeTabKey}
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
            onToggleInput={() => executeCommandById(COMMAND_IDS.toggleInput)}
            onToggleResizeWithWindow={() =>
              executeCommandById(COMMAND_IDS.toggleResize)
            }
            onReconnect={() => executeCommandById(COMMAND_IDS.reconnect)}
            onShowCommands={() => executeCommandById(COMMAND_IDS.showPalette)}
            commandShortcutLabel={paletteShortcutLabel}
          />
          <div className="terminal-notices">
            {attachment.state.history_gap ? (
              <div className="history-gap-banner" role="status">
                Earlier remote output is no longer contiguous. The live screen
                was restored from a checkpoint.
              </div>
            ) : null}
            {attachment.state.message ? (
              <div className="message-banner" role="status">
                {attachment.state.message}
              </div>
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
      {hostFlow !== undefined ? (
        <SshHostFlow
          suggestions={hostSuggestions}
          warning={sshConfigWarning}
          target={hostFlow ?? undefined}
          onActivateHost={activateConfiguredHost}
          onSaveHost={saveHost}
          onConnected={() => void refresh()}
          onClose={() => {
            setHostFlow(undefined);
            requestAnimationFrame(() => renderer?.focus());
          }}
        />
      ) : pendingCloseSessionKey ? (
        <QuickInput
          title="Close session"
          description={`Terminate ${sessions.find((session) => sessionKey(session) === pendingCloseSessionKey)?.name ?? "this session"} for all clients? This cannot be undone.`}
          mode={{
            kind: "confirm",
            confirm_label: "Close session",
            destructive: true,
          }}
          onCancel={cancelClose}
          onSubmit={() => {
            const session = sessions.find(
              (session) => sessionKey(session) === pendingCloseSessionKey,
            );
            if (session) confirmClose(session);
            else cancelClose();
          }}
        />
      ) : daemonRestartConfirmationPending ? (
        <QuickInput
          title="Restart rmuxd"
          description="Terminate every local rmux session and start a new daemon? This cannot be undone."
          mode={{
            kind: "confirm",
            confirm_label: "Restart rmuxd",
            destructive: true,
          }}
          onCancel={cancelDaemonRestart}
          onSubmit={confirmDaemonRestart}
        />
      ) : paletteOpen ? (
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
