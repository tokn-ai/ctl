import { useTaskWorkspace } from "../features/tasks/useTaskWorkspace";
import { TaskSidebar } from "../components/tasks/TaskSidebar";
import { TaskEditor } from "../components/tasks/TaskEditor";
import { TaskDetail } from "../components/tasks/TaskDetail";
import { taskState } from "../features/tasks/taskModel";
import { workspaceTabKey } from "../features/workspace/workspaceModel";
import "../components/tasks/tasks.css";
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { QuickInput } from "../components/commands/QuickInput";
import { SshHostFlow } from "../components/sessions/SshHostFlow";
import { AddExistingSessionFlow } from "../components/sessions/AddExistingSessionFlow";
import { NewShellFlow } from "../components/sessions/NewShellFlow";
import { useWorkspace } from "../features/workspace/useWorkspace";
import { useWorkspaceConnections } from "../features/workspace/useWorkspaceConnections";
import { withHostId } from "../features/workspace/workspaceModel";
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
} from "../features/commands/terminalCommands";
import type { AppCommand, CommandArguments } from "../features/commands/types";
import { CommandDispatcher } from "../features/commands/CommandDispatcher";
import { CommandProvider } from "../features/commands/CommandContext";
import { CommandBindings } from "../features/commands/CommandBindings";
import { useKeybindings } from "../features/commands/useKeybindings";
import { KeybindingsFlow } from "../components/commands/KeybindingsFlow";
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
  appLocalSshTarget,
  configuredSshTarget,
  inactiveSshConfigDestinations,
  normalizeSshDestination,
  sameSession,
  sameTarget,
  sessionKey,
  targetKey,
} from "../features/targets/targets";
import { useWindowTitle } from "../features/window/useWindowTitle";
import { errorCode, errorMessage } from "../lib/errors";
import { displayWorkingDirectory } from "../lib/shellState";
import {
  createSession,
  killSession,
  inspectKnownSessions,
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
  const workspace = useWorkspace();
  const {
    targets,
    setTargets,
    sessions,
    setSessions,
    tabs,
    setTabs,
    active_tab_key: activeTabKey,
    setActiveTabKey,
    shell_states: sessionShellStates,
    setShellStates: setSessionShellStates,
    persist: persistWorkspace,
  } = workspace;
  const [renderer, setRenderer] = useState<XtermRenderer | null>(null);
  const [shortcutPlatform] = useState(detectShortcutPlatform);
  const keybindings = useKeybindings(shortcutPlatform);
  const [keybindingsOpen, setKeybindingsOpen] = useState(false);
  const [dispatcher] = useState(() => new CommandDispatcher());
  const attachment = useAttachment(renderer);
  const taskWorkspace = useTaskWorkspace(workspace, async (session) => { await attachment.connect(session, { resize_with_window: true }); }, attachment.detach);
  const taskWorkspaceRef = useRef(taskWorkspace); taskWorkspaceRef.current = taskWorkspace;
  const currentShellState = attachment.state.shell_state;
  const currentWorkingDirectory = currentShellState?.cwd || null;
  const currentWorkingDirectoryDisplay = currentShellState
    ? displayWorkingDirectory(currentShellState)
    : null;
  const [sshConfigHosts, setSshConfigHosts] = useState<SshConfigHost[]>([]);
  const [sshConfigWarning, setSshConfigWarning] = useState<string | null>(null);
  const [targetErrors, setTargetErrors] = useState<ReadonlyMap<string, string>>(
    () => new Map(),
  );
  const [tabShellStates, setTabShellStates] = useState<
    ReadonlyMap<string, ShellStateSummary>
  >(() => new Map());
  const [loading, setLoading] = useState(false);
  const [listError, setListError] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [newShellOpen, setNewShellOpen] = useState(false);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [importOpen, setImportOpen] = useState(false);
  const [pendingForget, setPendingForget] = useState<SessionSummary | null>(
    null,
  );
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
  const pendingCloseSessionKeyRef = useRef<string | null>(null);
  const refreshGuardRef = useRef(new SessionListRefreshGuard());
  const sessionsRef = useRef<SessionSummary[]>([]);
  const tabsRef = useRef<SessionSummary[]>([]);
  const activeTabKeyRef = useRef<string | null>(null);
  sessionsRef.current = sessions;
  tabsRef.current = tabs;
  activeTabKeyRef.current = activeTabKey;
  const creatingRef = useRef(false);
  const daemonRestartConfirmationRef = useRef(false);
  const restartingDaemonRef = useRef(false);
  const daemonEpochRef = useRef(0);
  workspace.closeBlockedRef.current = () =>
    creatingRef.current || restartingDaemonRef.current || taskWorkspace.busy || taskWorkspace.hasDirtyDrafts;

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
    async (selectedTarget?: ConnectionTarget) => {
      if (!workspace.ready || daemonRestartBlocksInteractions()) {
        return;
      }
      const daemonEpoch = daemonEpochRef.current;
      const token = refreshGuardRef.current.begin();
      setLoading(true);
      setListError(null);
      try {
        const results = await Promise.all(
          targets
            .filter(
              (target) =>
                (!selectedTarget || sameTarget(target, selectedTarget)) &&
                sessionsRef.current.some((session) =>
                  sameTarget(session.target, target),
                ),
            )
            .map(async (target) => {
              try {
                const ids = sessionsRef.current
                  .filter((session) => sameTarget(session.target, target))
                  .map((session) => session.session_id);
                return {
                  target,
                  response: await inspectKnownSessions(target, ids),
                } as const;
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
        const refreshed = new Map<string, SessionSummary>();
        const inspections = new Map<string, ShellStateSummary>();
        const errors = new Map(targetErrors);
        for (const result of results) {
          const key = targetKey(result.target);
          errors.delete(key);
          if ("error" in result) {
            errors.set(key, errorMessage(result.error));
            for (const session of sessionsRef.current.filter((session) =>
              sameTarget(session.target, result.target),
            )) {
              refreshed.set(sessionKey(session), {
                ...session,
                status: "unreachable",
              });
            }
            continue;
          }
          for (const inspection of result.response) {
            const known = sessionsRef.current.find(
              (session) =>
                sameTarget(session.target, result.target) &&
                session.session_id === inspection.session_id,
            );
            if (!known) continue;
            const identity = sessionKey(known);
            refreshed.set(
              identity,
              inspection.session ?? {
                ...known,
                status:
                  inspection.error?.code === "session_not_found"
                    ? "missing"
                    : "unreachable",
              },
            );
            if (inspection.shell_state) {
              inspections.set(identity, inspection.shell_state);
            } else if (inspection.error?.code !== "session_not_found") {
              errors.set(
                key,
                inspection.error?.message ?? "Could not inspect session.",
              );
            }
          }
        }
        const visible = sessionsRef.current.map(
          (session) => refreshed.get(sessionKey(session)) ?? session,
        );
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
      } finally {
        if (
          daemonEpoch === daemonEpochRef.current &&
          refreshGuardRef.current.isLatest(token)
        ) {
          setLoading(false);
        }
      }
    },
    [
      daemonRestartBlocksInteractions,
      targets,
      targetErrors,
      workspace.ready,
      setSessions,
      setTabs,
      setSessionShellStates,
    ],
  );

  const addTarget = useCallback(
    async (target: ConnectionTarget): Promise<boolean> => {
      if (
        !workspace.ready ||
        target.kind === "local" ||
        !normalizeSshDestination(target.destination) ||
        targets.some(
          (candidate) =>
            candidate.kind === "ssh" &&
            candidate.destination === target.destination,
        )
      ) {
        return false;
      }
      const next = [...targets, withHostId(target)];
      setTargets(next);
      await persistWorkspace();
      return true;
    },
    [targets, workspace.ready, setTargets, persistWorkspace],
  );

  const activateConfiguredHost = useCallback(
    async (destination: string): Promise<boolean> => {
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
      if (!target || !(await addTarget(target))) {
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
    async (requestedSession: SessionSummary, resizeWithWindow = false) => {
      const daemonEpoch = daemonEpochRef.current;
      if (daemonRestartBlocksInteractions()) {
        return;
      }
      const managed = requestedSession.target.kind === "local" ? taskWorkspaceRef.current.tasks.find((task) => task.active_run?.interactive?.session_id === requestedSession.session_id) : undefined;
      if (managed) { taskWorkspaceRef.current.openTask(managed); return; }
      const identity = sessionKey(requestedSession);
      // Inspection may have just completed, before React publishes new props.
      const session =
        sessionsRef.current.find((known) => sessionKey(known) === identity) ??
        requestedSession;
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

  const resumeHost = useWorkspaceConnections({
    ready: workspace.ready,
    closing: workspace.closing,
    tabs,
    active_tab_key: activeTabKey,
    activateTab,
    refreshHost: refresh,
  });

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
      // Closing a restored, disconnected tab must not open a new SSH channel.
      if (nextTab && attachment.state.session !== null) {
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
      setSessions(nextSessions);
      setTabs(nextTabs);
      setActiveTabKey(nextActiveKey);
      await persistWorkspace();
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
      if (removingActive && nextActive && attachment.state.session !== null) {
        await attachment.connect(nextActive);
      }
    },
    [
      attachment,
      daemonRestartBlocksInteractions,
      targets,
      setTargets,
      setSessions,
      setTabs,
      setActiveTabKey,
      setSessionShellStates,
      persistWorkspace,
    ],
  );

  const create = useCallback(
    async (
      target: ConnectionTarget,
      workingDirectory: string | null,
    ): Promise<void> => {
      if (
        !workspace.ready ||
        creatingRef.current ||
        daemonRestartConfirmationRef.current ||
        restartingDaemonRef.current
      ) {
        throw new Error(
          "Shell creation is currently unavailable. Try again when the workspace is ready.",
        );
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
          return;
        }
        refreshGuardRef.current.recordMutation();
        setSessions((current) => {
          const next = prependSession(current, session);
          sessionsRef.current = next;
          return next;
        });
        try {
          await persistWorkspace();
        } catch (failure) {
          setListError(
            `Shell ${session.name} was created, but saving its workspace entry failed. Retry saving before closing the app. ${errorMessage(failure)}`,
          );
          return;
        }
        try {
          await activateTab(session, true);
        } catch (failure) {
          // Creation already succeeded: close the input flow rather than offer
          // a retry that would create a second persistent shell.
          if (daemonEpoch === daemonEpochRef.current) {
            setListError(
              `Shell ${session.name} was created, but opening its tab failed. Select the existing session to retry. ${errorMessage(failure)}`,
            );
          }
        }
      } finally {
        if (daemonEpoch === daemonEpochRef.current) {
          creatingRef.current = false;
          setCreating(false);
        }
      }
    },
    [activateTab, renderer, workspace.ready, setSessions, persistWorkspace],
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

  const importSession = useCallback(
    async (session: SessionSummary, shell_state: ShellStateSummary | null) => {
      if (!workspace.ready || daemonRestartBlocksInteractions()) return;
      refreshGuardRef.current.recordMutation();
      setSessions((current) => prependSession(current, session));
      if (shell_state) {
        setSessionShellStates((current) =>
          rememberShellState(current, sessionKey(session), shell_state),
        );
      }
      await persistWorkspace();
    },
    [
      workspace.ready,
      daemonRestartBlocksInteractions,
      setSessions,
      setSessionShellStates,
      persistWorkspace,
    ],
  );

  const forgetSession = useCallback(
    async (session: SessionSummary) => {
      if (daemonRestartBlocksInteractions()) return;
      attachment.cancelPendingConnection(session);
      refreshGuardRef.current.recordMutation();
      await closeTab(session);
      setSessions((current) => removeSession(current, sessionKey(session)));
      setSessionShellStates((current) =>
        forgetShellState(current, sessionKey(session)),
      );
      await persistWorkspace();
    },
    [
      attachment,
      closeTab,
      daemonRestartBlocksInteractions,
      setSessions,
      setSessionShellStates,
      persistWorkspace,
    ],
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
      const identity = sessionKey(session);
      if (
        daemonRestartBlocksInteractions() ||
        pendingCloseSessionKeyRef.current !== null ||
        closingSessionKeysRef.current.has(identity)
      ) {
        return;
      }
      setPaletteOpen(false);
      pendingCloseSessionKeyRef.current = identity;
      setPendingCloseSessionKey(identity);
    },
    [daemonRestartBlocksInteractions],
  );

  const cancelClose = useCallback(() => {
    pendingCloseSessionKeyRef.current = null;
    setPendingCloseSessionKey(null);
    requestAnimationFrame(() => renderer?.focus());
  }, [renderer]);

  const confirmClose = useCallback(
    (session: SessionSummary) => {
      if (
        daemonRestartBlocksInteractions() ||
        pendingCloseSessionKeyRef.current !== sessionKey(session)
      ) {
        return;
      }
      // Consume the confirmation before starting async work or rerendering.
      pendingCloseSessionKeyRef.current = null;
      setPendingCloseSessionKey(null);
      return close(session);
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
    closingSessionKeysRef.current.clear();
    creatingRef.current = false;
    if (attachment.state.session?.target.kind === "local") {
      attachment.resetAfterDaemonRestart();
    }
    const markLocalMissing = (current: SessionSummary[]): SessionSummary[] =>
      current.map((session) =>
        session.target.kind === "local"
          ? { ...session, status: "missing" }
          : session,
      );
    setSessions(markLocalMissing);
    setTabs(markLocalMissing);
    setCreating(false);
    setNewShellOpen(false);
    pendingCloseSessionKeyRef.current = null;
    setPendingCloseSessionKey(null);
    setClosingSessionKeys(new Set());
    setDisconnectingSessionKey(null);
    setLoading(false);
    setListError(null);
    setTargetErrors((current) => {
      const next = new Map(current);
      next.delete("local");
      return next;
    });
  }, [attachment, setSessions, setTabs]);

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
    } catch (error) {
      if (!restartFailurePreservesLocalState(errorCode(error))) {
        clearLocalDaemonState();
      }
      setListError(`Could not restart rmuxd: ${errorMessage(error)}`);
    } finally {
      restartingDaemonRef.current = false;
      setRestartingDaemon(false);
      setLoading(false);
    }
  }, [clearLocalDaemonState, setDaemonRestartConfirmation]);

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
    if (!attachedSession) return;
    const phase = attachment.state.phase;
    const status =
      phase === "ended"
        ? "exited"
        : phase === "error"
          ? attachment.state.error_code === "session_not_found"
            ? "missing"
            : "unreachable"
          : phase === "attached"
            ? "running"
            : null;
    if (!status) return;
    const updateStatus = (current: SessionSummary[]): SessionSummary[] => {
      if (
        !current.some(
          (session) =>
            sameSession(session, attachedSession) && session.status !== status,
        )
      )
        return current;
      refreshGuardRef.current.recordMutation();
      return current.map((session) =>
        sameSession(session, attachedSession)
          ? { ...session, status }
          : session,
      );
    };
    setSessions(updateStatus);
    setTabs(updateStatus);
  }, [
    attachment.state.phase,
    attachment.state.error_code,
    attachedSessionKey,
    setSessions,
    setTabs,
  ]);

  const openTabSessionKeys = new Set(tabs.map(sessionKey));

  const activeTab =
    tabs.find((tab) => sessionKey(tab) === activeTabKey) ?? null;
  const activeShellState =
    attachedSessionKey === activeTabKey && attachedShellState
      ? attachedShellState
      : activeTabKey
        ? (tabShellStates.get(activeTabKey) ??
          sessionShellStates.get(activeTabKey) ??
          null)
        : null;
  const displayedTabShellStates =
    attachedSession && attachedShellState
      ? rememberShellState(
          new Map([...sessionShellStates, ...tabShellStates]),
          sessionKey(attachedSession),
          attachedShellState,
          { replaceEqualRevision: true },
        )
      : new Map([...sessionShellStates, ...tabShellStates]);
  const displayedSessionShellStates = new Map(sessionShellStates);
  if (attachedSession && attachedShellState) {
    displayedSessionShellStates.set(
      sessionKey(attachedSession),
      attachedShellState,
    );
  }
  const activeTitle = formatTerminalTitle(activeTab, activeShellState);
  useWindowTitle(taskWorkspace.active ? taskWorkspace.activeTask?.definition.name ?? taskWorkspace.saved?.definition.name ?? "Task definition" : compactTerminalTitleParts(activeTitle));

  const commands: AppCommand[] = buildTerminalCommands(
    {
      targets,
      sessions,
      tabs,
      activeSessionKey: activeTabKey,
      attachmentSessionKey: attachedSessionKey,
      phase: attachment.state.phase,
      inputOwned: attachment.state.input_lease.owned_by_client,
      resizeWithWindow: attachment.state.resize_with_window,
      listLoading: loading,
      creating,
      newShellOpen,
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
      showAddExistingSession: () => setImportOpen(true),
      forgetSession: (session) => setPendingForget(session),
      showNewShell: () => {
        if (!daemonRestartBlocksInteractions()) {
          setNewShellOpen(true);
        }
      },
      openShellTab: () => {
        if (currentWorkingDirectory && activeTab) {
          void create(activeTab.target, currentWorkingDirectory).catch(
            (failure) => setListError(errorMessage(failure)),
          );
        }
      },
      refreshSessions: () => void refresh(),
      selectSession: activateTab,
      disconnectSession: disconnect,
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
      connectHost: (target) => {
        setPaletteOpen(false);
        setHostFlow(target);
      },
      removeHost,
      saveWorkspace: () => persistWorkspace(true),
      configureKeybindings: () => setKeybindingsOpen(true),
      reloadKeybindings: keybindings.reload,
    },
  ).map((command) => {
    const base = { ...command, keybinding: keybindings.bindings.get(command.id) };
    if (command.id === COMMAND_IDS.disconnect && taskWorkspace.active) return { ...base, title: "Close tab", enabled: true, isEnabled: () => true, focusTerminalAfterRun: false, run: () => taskWorkspace.close(workspaceTabKey(taskWorkspace.active!)) };
    if (command.id === COMMAND_IDS.nextTab || command.id === COMMAND_IDS.previousTab) return { ...base, enabled: workspace.tab_order.length > 1, focusTerminalAfterRun: false, run: () => {
      const offset = command.id === COMMAND_IDS.nextTab ? 1 : -1;
      const order = workspace.tab_order;
      const key = order[(order.indexOf(activeTabKey ?? "") + offset + order.length) % order.length];
      const taskTab = workspace.task_tabs.find((tab) => workspaceTabKey(tab) === key);
      if (taskTab) taskWorkspace.open(taskTab); else { const terminal = tabs.find((tab) => sessionKey(tab) === key); if (terminal) void activateTab(terminal); }
    } };
    return base;
  });
  commands.push({ id: "task.new", category: "Tasks", title: "New task definition", enabled: workspace.ready && !taskWorkspace.busy, focusTerminalAfterRun: false, run: taskWorkspace.newDefinition });
  for (const action of ["start_task", "stop_task", "restart_task"] as const) {
    commands.push({ id: `task.${action}`, category: "Tasks", title: action === "start_task" ? "Start task" : action === "stop_task" ? "Stop task" : "Restart task", enabled: !!taskWorkspace.activeTask && !taskWorkspace.busy && (action === "start_task" ? !taskWorkspace.activeTask.active_run : action === "stop_task" ? !!taskWorkspace.activeTask.active_run : true), focusTerminalAfterRun: false, run: () => { if (taskWorkspace.activeTask) void taskWorkspace.action(taskWorkspace.activeTask, action); } });
  }
  const shortcutLabel = (id: string) => {
    const binding = keybindings.bindings.get(id);
    return binding ? formatKeybinding(binding, shortcutPlatform) : "";
  };
  const paletteShortcutLabel = shortcutLabel(COMMAND_IDS.showPalette);
  const closeShortcutLabel = shortcutLabel(COMMAND_IDS.close);
  const dialogOpen =
    keybindingsOpen ||
    newShellOpen ||
    importOpen ||
    pendingForget !== null ||
    hostFlow !== undefined ||
    pendingCloseSessionKey !== null ||
    daemonRestartConfirmationPending;

  useLayoutEffect(() => {
    dispatcher.update(
      commands.map((command) => ({
        ...command,
        run: (args) => {
          if (!command.keepPaletteOpen) setPaletteOpen(false);
          if (command.id !== COMMAND_IDS.restartDaemon) cancelDaemonRestart();
          const result = command.run(args);
          if (command.focusTerminalAfterRun !== false)
            requestAnimationFrame(() => renderer?.focus());
          return result;
        },
      })),
      workspace.ready && !workspace.closing && keybindings.ready && !dialogOpen,
      (error) => setListError(errorMessage(error)),
    );
  });

  function executeCommandById(commandId: string, args?: CommandArguments) {
    dispatcher.execute(commandId, args);
  }
  const executeCommand = (command: AppCommand) =>
    executeCommandById(command.id);

  const handleTerminalInput = useCallback(
    (data: Uint8Array) => {
      if (!dialogOpen && !paletteOpen && !daemonRestartBlocksInteractions()) {
        attachment.handleInput(data);
      }
    },
    [attachment, daemonRestartBlocksInteractions, dialogOpen, paletteOpen],
  );

  function dismissPalette() {
    setPaletteOpen(false);
    cancelDaemonRestart();
    requestAnimationFrame(() => renderer?.focus());
  }

  return (
    <CommandProvider
      value={{ dispatcher, keybinding: (id) => keybindings.bindings.get(id) }}
    >
      <CommandBindings platform={shortcutPlatform} />
      <main
        className="app-shell"
        inert={
          !workspace.ready || workspace.closing || dialogOpen || paletteOpen
        }
      >
        <SessionSidebar
          targets={targets}
          targetErrors={targetErrors}
          sessions={sessions}
          shellStates={displayedSessionShellStates}
          selectedSessionKey={activeTabKey}
          openTabSessionKeys={openTabSessionKeys}
          loading={loading || !workspace.ready}
          error={listError ?? workspace.error}
          creating={creating}
          closingSessionKeys={closingSessionKeys}
          disconnectingSessionKey={disconnectingSessionKey}
          onRefresh={() => executeCommandById(COMMAND_IDS.refreshSessions)}
          onSelect={(session) =>
            executeCommandById(COMMAND_IDS.selectSession, {
              session_key: sessionKey(session),
            })
          }
          onNewShell={() => executeCommandById(COMMAND_IDS.newShell)}
          onDisconnect={(session) =>
            executeCommandById(COMMAND_IDS.disconnect, {
              session_key: sessionKey(session),
            })
          }
          onRequestClose={(session) =>
            executeCommandById(COMMAND_IDS.close, {
              session_key: sessionKey(session),
            })
          }
          onForget={(session) =>
            executeCommandById(COMMAND_IDS.forgetSession, {
              session_key: sessionKey(session),
            })
          }
          onAddExisting={() =>
            executeCommandById(COMMAND_IDS.addExistingSession)
          }
          onAddHost={() => executeCommandById(COMMAND_IDS.addHost)}
          onConnectHost={(target) =>
            executeCommandById(COMMAND_IDS.connectHost, {
              target_key: targetKey(target),
            })
          }
          onRemoveHost={(target) =>
            executeCommandById(COMMAND_IDS.removeHost, {
              target_key: targetKey(target),
            })
          }
        ><TaskSidebar model={taskWorkspace} definitions={workspace.task_definitions} references={workspace.task_references} /></SessionSidebar>
        <section className="terminal-workspace">
          <TerminalTabs
            tabs={tabs}
            extra_tabs={workspace.task_tabs.map((tab) => {
              const saved = tab.kind === "task_definition" ? workspace.task_definitions.find((item) => item.definition_id === tab.definition_id) : undefined;
              const task = tab.kind === "task" ? taskWorkspace.tasks.find((item) => item.task_id === tab.task_id) : undefined;
              return { tab_key: workspaceTabKey(tab), title: tab.kind === "task_definition" ? `${saved?.definition.name ?? "New task"}${taskWorkspace.drafts[tab.definition_id]?.dirty ? " *" : ""}` : task?.definition.name ?? "Saved task", host: tab.kind === "task_definition" ? "Definition" : tab.host_id === "local" ? "Local" : tab.host_id, status: task ? taskState(task) : "unknown" };
            })}
            tab_order={workspace.tab_order}
            on_select_extra={(key) => { const tab = workspace.task_tabs.find((item) => workspaceTabKey(item) === key); if (tab) taskWorkspace.open(tab); }}
            on_close_extra={taskWorkspace.close}
            shellStates={displayedTabShellStates}
            activeSessionKey={activeTabKey}
            canCreate={
              currentWorkingDirectory !== null &&
              !creating &&
              !daemonRestartConfirmationPending &&
              !restartingDaemon
            }
            onSelect={(session) =>
              executeCommandById(COMMAND_IDS.selectSession, {
                session_key: sessionKey(session),
              })
            }
            onClose={(session) =>
              executeCommandById(COMMAND_IDS.disconnect, {
                session_key: sessionKey(session),
              })
            }
            onCreate={() => executeCommandById(COMMAND_IDS.newTab)}
          />
          {taskWorkspace.active?.kind === "task_definition" ? <TaskEditor key={taskWorkspace.active.definition_id} model={taskWorkspace} saved={taskWorkspace.saved} /> : null}
          {taskWorkspace.active?.kind === "task" ? <TaskDetail key={`${taskWorkspace.active.host_id}:${taskWorkspace.active.task_id}`} model={taskWorkspace} saved={workspace.task_definitions.find((definition) => definition.definition_id === workspace.task_references.find((reference) => reference.task_id === taskWorkspace.activeTask?.task_id || (taskWorkspace.active?.kind === "task" && reference.task_id === taskWorkspace.active.task_id))?.definition_id)} /> : null}
          {taskWorkspace.pendingClose ? <div className="task-confirm" role="dialog" aria-label="Unsaved task definition"><p>Save changes before closing this definition?</p><button onClick={() => void taskWorkspace.saveBeforeClose()}>Save and close</button><button onClick={() => taskWorkspace.closeNow(taskWorkspace.pendingClose!)}>Discard changes</button><button onClick={taskWorkspace.cancelClose}>Cancel</button></div> : null}
          <div className="terminal-pane" hidden={!!taskWorkspace.active && !(taskWorkspace.activeTask?.definition.execution_mode === "interactive" && taskWorkspace.activeTask.active_run)}>
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
            {keybindings.error ? (
              <div className="message-banner" role="alert">
                Keyboard shortcuts: {keybindings.error} Last valid bindings
                remain active.
                <button
                  type="button"
                  onClick={() =>
                    executeCommandById(COMMAND_IDS.reloadKeybindings)
                  }
                >
                  Reload shortcuts
                </button>
              </div>
            ) : null}
            {workspace.error ? (
              <div className="message-banner" role="alert">
                Workspace: {workspace.error}
                {workspace.ready ? (
                  <button
                    type="button"
                    onClick={() =>
                      executeCommandById(COMMAND_IDS.saveWorkspace)
                    }
                  >
                    Retry saving
                  </button>
                ) : null}
              </div>
            ) : null}
            {activeTab && attachment.state.session === null ? (
              <div className="message-banner" role="status">
                {activeTab.target.kind === "ssh"
                  ? "Connect this host to resume its saved tab. Cached paths are last-known."
                  : "Local session is not connected. Cached paths are last-known."}
                <button
                  type="button"
                  onClick={() => {
                    if (activeTab.target.kind === "ssh") {
                      executeCommandById(COMMAND_IDS.connectHost, {
                        target_key: targetKey(activeTab.target),
                      });
                    } else {
                      executeCommandById(COMMAND_IDS.selectSession, {
                        session_key: sessionKey(activeTab),
                      });
                    }
                  }}
                >
                  {activeTab.target.kind === "ssh"
                    ? "Connect host"
                    : "Connect session"}
                </button>
              </div>
            ) : null}
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
          </div>
        </section>
      </main>
      {keybindingsOpen ? (
        <KeybindingsFlow
          commands={commands}
          document={keybindings.document}
          path={keybindings.path}
          error={keybindings.error}
          platform={shortcutPlatform}
          onSave={keybindings.save}
          onClose={() => {
            setKeybindingsOpen(false);
            requestAnimationFrame(() => renderer?.focus());
          }}
        />
      ) : newShellOpen ? (
        <NewShellFlow
          targets={targets}
          onCreate={create}
          onClose={() => {
            setNewShellOpen(false);
            requestAnimationFrame(() => renderer?.focus());
          }}
        />
      ) : importOpen ? (
        <AddExistingSessionFlow
          targets={targets}
          known={sessions}
          onAdd={importSession}
          onClose={() => setImportOpen(false)}
        />
      ) : pendingForget ? (
        <QuickInput
          title="Remove from workspace"
          description={`Forget ${pendingForget.name} and close its tab? Its shell will keep running. You can add it again through discovery.`}
          mode={{ kind: "confirm", confirm_label: "Remove from workspace" }}
          onCancel={() => setPendingForget(null)}
          onSubmit={() => {
            const session = pendingForget;
            setPendingForget(null);
            return forgetSession(session).catch((failure) =>
              setListError(errorMessage(failure)),
            );
          }}
        />
      ) : hostFlow !== undefined ? (
        <SshHostFlow
          suggestions={hostSuggestions}
          warning={sshConfigWarning}
          target={hostFlow ?? undefined}
          onActivateHost={activateConfiguredHost}
          onSaveHost={saveHost}
          onConnected={() => {
            if (hostFlow)
              void resumeHost(hostFlow).catch((failure) =>
                setListError(errorMessage(failure)),
              );
          }}
          onClose={() => {
            setHostFlow(undefined);
            requestAnimationFrame(() => renderer?.focus());
          }}
        />
      ) : pendingCloseSessionKey ? (
        <QuickInput
          title="Close session"
          description={`Terminate ${sessions.find((session) => sessionKey(session) === pendingCloseSessionKey)?.name ?? "this session"} for all clients? This cannot be undone.${closeShortcutLabel ? ` Press ${closeShortcutLabel} to confirm.` : ""}`}
          confirm_command_id={COMMAND_IDS.close}
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
            if (session) return confirmClose(session);
            else cancelClose();
          }}
        />
      ) : daemonRestartConfirmationPending ? (
        <QuickInput
          title="Restart rmuxd"
          description="Terminate every local rmux session, including sessions opened by other apps, and start a new daemon? This cannot be undone."
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
    </CommandProvider>
  );
}
