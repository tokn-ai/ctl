import { WorkspaceSidebar } from "../components/workspace/WorkspaceSidebar";
import { AboutPage } from "./AboutPage";
import { CredentialsPage } from "./CredentialsPage";
import { credentialTargets } from "../features/credentials/credentialTargets";
import { useComponentActionEvents } from "../features/about/useComponentActionEvents";
import { componentResetMatches } from "../features/attachment/componentActions";
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
  useMemo,
  useRef,
  useState,
} from "react";
import { QuickInput } from "../components/commands/QuickInput";
import { HostSettingsDialog } from "../components/sessions/HostSettingsDialog";
import { SshHostFlow } from "../components/sessions/SshHostFlow";
import { ConnectHostFlow } from "../components/sessions/ConnectHostFlow";
import { PortForwardingDialog } from "../components/sessions/PortForwardingDialog";
import { PortForwardingSidebar } from "../components/portForwarding/PortForwardingSidebar";
import { AddExistingSessionFlow } from "../components/sessions/AddExistingSessionFlow";
import { NewShellFlow } from "../components/sessions/NewShellFlow";
import { hostSelectorChoices } from "../components/sessions/hostChoices";
import { useWorkspace } from "../features/workspace/useWorkspace";
import { useWorkspaceConnections } from "../features/workspace/useWorkspaceConnections";
import { useHostConnections } from "../features/workspace/useHostConnections";
import { usePortForwarding } from "../features/portForwarding/usePortForwarding";
import { useVpn } from "../features/vpn/useVpn";
import { vpnAggregateState } from "../features/vpn/status";
import { VpnSidebar } from "../components/vpn/VpnSidebar";
import {
  recoverRemoteHost,
  remapStateKeys,
  sameSshEndpoint,
} from "../features/workspace/remoteRecovery";
import { connectionMethodOptions, connectionSettings, expectedHostIdentity, hostFromTarget, hostTarget, isVirtualHost, projectedHostId, tailscaleHostId, promoteHost, updateHostSettings, workspaceSidebarTargets } from "../features/workspace/workspaceModel";
import { removableHostCredentials } from "../features/workspace/hostCredentials";
import { CommandPalette } from "../components/commands/CommandPalette";
import { ArchiveBrowser } from "../components/sessions/ArchiveBrowser";
import { SessionSidebar } from "../components/sessions/SessionSidebar";
import { StatusBar } from "../components/status/StatusBar";
import { NotificationBell, Notifications } from "../components/notifications/Notifications";
import { NotificationStore } from "../features/notifications/NotificationStore";
import { NotificationProvider, useNotificationEnvironment } from "../features/notifications/NotificationContext";
import { useWorkbenchNotifications } from "../features/notifications/useWorkbenchNotifications";
import { TerminalTabs } from "../components/tabs/TerminalTabs";
import { SessionViewSurface } from "../components/terminal/SessionViewSurface";
import { TerminalToolbar } from "../components/terminal/TerminalToolbar";
import { useSessionAttachments } from "../features/attachment/useSessionAttachments";
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
import { XtermRenderer } from "../features/terminal/XtermRenderer";
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
  sameSession,
  sameTarget,
  sessionKey,
  targetKey,
  targetKeyFromSessionKey,
} from "../features/targets/targets";
import { useWindowTitle } from "../features/window/useWindowTitle";
import { errorCode, errorDetails, errorMessage, type ErrorDetails } from "../lib/errors";
import { displayWorkingDirectory } from "../lib/shellState";
import {
  sessionCache,
  createSession,
  killSession,
  inspectKnownSessions,
  probeSshHost,
  cancelSshProbe,
  restartLocalDaemon,
  forgetSshCredentials,
  sshConnectionStatus,
  executeComponentAction,
} from "../lib/tauri";
import type {
  ConnectionTarget,
  RemoteIdentity,
  SessionSummary,
  ShellStateSummary,
  TerminalSize,
  SshConnectionTarget,
  WorkspaceSshGateway,
  WorkspacePortForward,
  WorkspaceHost,
  WorkspaceConnectionMethod,
  ComponentActionPreflight,
} from "../lib/types";

interface MethodDraft {
  host_id: string | null;
  host_name: string;
  method_id: string | null;
  method_name: string;
  initial_target?: SshConnectionTarget;
}

function measuredSize(renderer: XtermRenderer | null): TerminalSize {
  const proposed = renderer?.proposeDimensions();
  return {
    columns: proposed?.columns ?? 80,
    rows: proposed?.rows ?? 24,
    pixel_width: null,
    pixel_height: null,
  };
}

async function verifyHostConnectionStatus(target: SshConnectionTarget): Promise<void> {
  let connection;
  try {
    connection = await sshConnectionStatus(target);
  } catch (failure) {
    // Platforms without the Unix broker still support verified batch SSH.
    if (errorCode(failure) === "ssh_broker_unsupported") return;
    throw failure;
  }
  if (connection.manually_disconnected) {
    throw new Error("This host was disconnected. Connect again to resume.");
  }
  if (!connection.connected) {
    throw new Error("The SSH connection ended before verification completed. Try connecting again.");
  }
}

export function TerminalPage() {
  const [notifications] = useState(() => new NotificationStore());
  return <NotificationProvider store={notifications}><TerminalWorkbench /></NotificationProvider>;
}

function TerminalWorkbench() {
  const { store: notifications, attachments: attachmentNotifications } = useNotificationEnvironment()!;
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
  const [pane_commands, setPaneCommands] = useState<AppCommand[]>([]);
  const attachment = useSessionAttachments(renderer);
  const taskWorkspace = useTaskWorkspace(
    workspace,
    async (session) => {
      await attachment.connect(session, { resize_with_window: true });
    },
    attachment.detach,
  );
  const taskWorkspaceRef = useRef(taskWorkspace);
  taskWorkspaceRef.current = taskWorkspace;
  const open_session_keys = useMemo(() => {
    const session_keys = new Set(tabs.map(sessionKey));
    for (const tab of workspace.task_tabs) {
      if (tab.kind !== "task" || tab.host_id !== "local") continue;
      const session_id = taskWorkspace.tasks.find(
        (task) => task.task_id === tab.task_id,
      )?.active_run?.interactive?.session_id;
      if (session_id) {
        session_keys.add(sessionKey({ target: { kind: "local" }, session_id }));
      }
    }
    return session_keys;
  }, [tabs, workspace.task_tabs, taskWorkspace.tasks]);
  useEffect(() => {
    attachment.retainSessions(open_session_keys);
    renderer?.retainSessions(open_session_keys);
  }, [renderer, open_session_keys, attachment.retainSessions]);
  const currentShellState = attachment.state.shell_state;
  const currentWorkingDirectory = currentShellState?.cwd || null;
  const currentWorkingDirectoryDisplay = currentShellState
    ? displayWorkingDirectory(currentShellState)
    : null;
  const { sshConfigHosts, sshConfigWarning, tailscaleDevices, tailscaleWarning } = workspace;
  const discoveryWarning = [sshConfigWarning, tailscaleWarning].filter(Boolean).join("\n") || null;
  const [targetErrors, setTargetErrors] = useState<ReadonlyMap<string, ErrorDetails>>(
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
  const [hostFlow, setHostFlow] = useState<{
    target: SshConnectionTarget;
    selected_method_id?: string;
    update_required?: boolean;
  } | null>(null);
  const [addHostOpen, setAddHostOpen] = useState(false);
  const openAddHost = () => {
    void workspace.refreshHostDiscovery();
    setAddHostOpen(true);
  };
  const [connectHostOpen, setConnectHostOpen] = useState(false);
  const [methodNameOpen, setMethodNameOpen] = useState(false);
  const [methodDraft, setMethodDraft] = useState<MethodDraft | null>(null);
  const [hostSettingsId, setHostSettingsId] = useState<string | null>(null);
  const [portForwardTarget, setPortForwardTarget] = useState<SshConnectionTarget | null>(null);
  const [portForwardUpdateTarget, setPortForwardUpdateTarget] = useState<SshConnectionTarget | null>(null);
  const hostVerificationRef = useRef(new Map<string, Promise<ConnectionTarget>>());
  const [
    daemonRestartConfirmationPending,
    setDaemonRestartConfirmationPending,
  ] = useState(false);
  const [restartingDaemon, setRestartingDaemon] = useState(false);
  const [utility_page, setUtilityPage] = useState<"about" | "credentials" | null>(null);
  const [about_dialog_open, setAboutDialogOpen] = useState(false);
  const [credentials_dialog_open, setCredentialsDialogOpen] = useState(false);
  useEffect(() => { setUtilityPage(null); }, [activeTabKey]);
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
  const creatingTargetRef = useRef<ConnectionTarget | null>(null);
  const daemonRestartConfirmationRef = useRef(false);
  const restartingDaemonRef = useRef(false);
  const daemonEpochRef = useRef(0);
  useWorkbenchNotifications(notifications, {
    workspace_error: workspace.error,
    workspace_ready: workspace.ready,
    keybindings_error: keybindings.error,
    session_error: listError,
    targets,
    target_errors: targetErrors,
    storage_error: attachment.storage_error,
    task_error: taskWorkspace.error,
    definitions_error: taskWorkspace.definitions_error,
    task_status: taskWorkspace.daemonStatus,
    tasks: taskWorkspace.tasks,
    tasks_loaded: taskWorkspace.hasLoaded,
  });
  const captureDaemonOperation = useCallback((target: ConnectionTarget) => {
    const epoch = daemonEpochRef.current;
    return () => target.kind !== "local" || epoch === daemonEpochRef.current;
  }, []);
  workspace.closeBlockedRef.current = () =>
    creatingRef.current || restartingDaemonRef.current || taskWorkspace.busy;

  const updatePortForwards = useCallback(
    (
      update: (
        current: typeof workspace.port_forwards,
      ) => typeof workspace.port_forwards,
    ) => workspace.update("port_forwards", update),
    [workspace.update],
  );
  const portForwarding = usePortForwarding(
    workspace.ready,
    targets,
    workspace.port_forwards,
    updatePortForwards,
  );
  const sidebarTargets = workspaceSidebarTargets(workspace);
  const vpn = useVpn(workspace.ready && !workspace.closing);
  const credential_targets = useMemo(
    () => credentialTargets(workspace.hosts, workspace.ssh_gateways),
    [workspace.hosts, workspace.ssh_gateways],
  );
  const hostConnections = useHostConnections({
    ready: workspace.ready,
    closing: workspace.closing,
    hosts: workspace.hosts.filter((host) => sidebarTargets.some((target) =>
      (target.kind === "local" ? "local" : target.host_id) === host.host_id)),
    targets: [...targets, ...sessions.map((session) => session.target), ...tabs.map((session) => session.target)],
    gateways: workspace.ssh_gateways,
    vpn_statuses: vpn.statuses,
    vpn_status_stale: vpn.status_stale,
    onPause: async (host_id) => {
      refreshGuardRef.current.recordMutation();
      for (const session of sessionsRef.current) {
        if (session.target.kind === "ssh" && session.target.host_id === host_id) attachment.cancelPendingConnection(session);
      }
      await Promise.all([
        portForwarding.pauseHost(host_id),
        attachment.disconnectHost(host_id),
      ]);
    },
    onResume: portForwarding.resumeHost,
  });

  const daemonRestartBlocksInteractions = useCallback(
    () => daemonRestartConfirmationRef.current || restartingDaemonRef.current,
    [],
  );


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
          workspace.viewRef.current.targets
            .filter(
              (target) =>
                (!selectedTarget || sameTarget(target, selectedTarget)) &&
                !hostConnections.isPaused(target) &&
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
        const errors = new Map<string, ErrorDetails>();
        for (const result of results) {
          const key = targetKey(result.target);
          if ("error" in result) {
            errors.set(key, errorDetails(result.error));
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
                inspection.error ?? errorDetails("Could not inspect session."),
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
        for (const { target } of results) {
          const key = targetKey(target);
          if (!errors.has(key)) notifications.resolve(`host:${key}`);
        }
        setTargetErrors((current) => {
          const next = new Map(current);
          for (const { target } of results) next.delete(targetKey(target));
          for (const [key, message] of errors) next.set(key, message);
          return next;
        });
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
      workspace.ready,
      setSessions,
      setTabs,
      setSessionShellStates,
      hostConnections.isPaused,
      notifications,
    ],
  );

  const hostSuggestions = sshConfigHosts.map((host) => host.destination);
  // New connections use saved settings; live sessions retain their transport snapshots.
  const connectionTargets = useMemo(() => workspace.hosts.map((host) =>
    hostTarget(host, workspace.ssh_gateways)), [workspace.hosts, workspace.ssh_gateways]);
  // A missing preferred route must not hide the host's working alternatives.
  const connectableHostKeys = new Set(workspace.hosts
    .filter((host) => host.host_id !== "local" && host.source !== "unavailable" &&
      host.connection_methods.some((method) => !method.target.unavailable))
    .map((host) => targetKey(hostTarget(host, workspace.ssh_gateways))));
  const connectableTargets = connectionTargets.filter((target): target is SshConnectionTarget =>
    target.kind === "ssh" && connectableHostKeys.has(targetKey(target)));
  const settingsHost = workspace.hosts.find((host) => host.host_id === hostSettingsId);
  const methodHost = workspace.hosts.find((host) => host.host_id === methodDraft?.host_id);

  function editMethod(host: WorkspaceHost, method?: WorkspaceConnectionMethod) {
    setHostSettingsId(null);
    setMethodDraft({
      host_id: host.host_id,
      host_name: host.name,
      method_id: method?.method_id ?? null,
      method_name: method?.name ?? "SSH",
      initial_target: method ? { ...method.target, ...connectionMethodOptions(method) } : undefined,
    });
    setMethodNameOpen(!method);
  }

  async function saveNewHost(name: string, target: SshConnectionTarget, remote_info: RemoteIdentity) {
    const projected_id = target.tailscale_node_id
      ? tailscaleHostId(target.tailscale_node_id)
      : target.ssh_config_alias ? projectedHostId(target.ssh_config_alias) : null;
    const projected = workspace.viewRef.current.hosts.find((host) =>
      host.host_id === projected_id && isVirtualHost(host));
    const expected = projected ? expectedHostIdentity(projected) : undefined;
    if (expected && expected.remote_id !== remote_info.remote_id)
      throw new Error("This discovered host now reaches a different remote environment. Restore its original connection before saving.");
    const host = promoteHost(projected ? {
      ...projected,
      name,
      remote_info,
      ...(projected.expected_remote_info ? { expected_remote_info: remote_info } : {}),
      // Promotion keeps method references held by existing sessions valid.
      connection_methods: projected.connection_methods.map((method) =>
        method.method_id === projected.preferred_method_id
          ? { ...method, ...connectionMethodOptions(target), target: connectionSettings(target) }
          : method),
    } : hostFromTarget({ ...target, host_id: undefined, remote_info }, name));
    await workspace.replaceView((current) => projected
      ? updateHostSettings(current, host)
      : {
        ...current,
        hosts: [...current.hosts, host],
        targets: [...current.targets, hostTarget(host, current.ssh_gateways)],
      });
  }

  async function saveConnection(
    target: SshConnectionTarget,
    gateways: WorkspaceSshGateway[],
    remote_info: RemoteIdentity,
  ) {
    if (!methodDraft || !workspace.ready) throw new Error("Workspace is not available.");
    const current = workspace.viewRef.current;
    const existing = current.hosts.find((host) => host.host_id === methodDraft.host_id);
    if (methodDraft.host_id && !existing) throw new Error("This host was removed while connecting.");
    const expected = existing ? expectedHostIdentity(existing) : undefined;
    if (expected && expected.remote_id !== remote_info.remote_id)
      throw new Error("This method reaches a different account or remote environment. Add a separate host for it.");
    if (methodDraft.method_id && !existing?.connection_methods.some((method) => method.method_id === methodDraft.method_id))
      throw new Error("This connection method was removed while connecting.");
    const method: WorkspaceConnectionMethod = {
      method_id: methodDraft.method_id ?? crypto.randomUUID(),
      name: methodDraft.method_name,
      target: connectionSettings(target),
      ...connectionMethodOptions(target),
    };
    const host: WorkspaceHost = existing ? {
      ...promoteHost(existing),
      remote_info,
      connection_methods: methodDraft.method_id
        ? existing.connection_methods.map((item) => item.method_id === method.method_id ? method : item)
        : [...existing.connection_methods, method],
    } : {
      host_id: crypto.randomUUID(),
      name: methodDraft.host_name,
      remote_info,
      connection_methods: [method],
      preferred_method_id: method.method_id,
    };
    await workspace.replaceView((latest) => {
      const next = existing ? updateHostSettings(latest, host) : {
        ...latest,
        hosts: [...latest.hosts, host],
        targets: [...latest.targets, hostTarget(host, gateways)],
      };
      return { ...next, ssh_gateways: gateways };
    });
    void workspace.refreshHostDiscovery();
    setHostSettingsId(host.host_id);
  }

  async function saveHostSettings(host: WorkspaceHost) {
    await workspace.replaceView((current) => updateHostSettings(current, promoteHost(host)));
  }

  function connectHostMethod(target: ConnectionTarget, method_id?: string) {
    if (target.kind !== "ssh") return;
    setHostSettingsId(null);
    setHostFlow({ target, selected_method_id: method_id });
  }

  const activateTab = useCallback(
    async (requestedSession: SessionSummary, resizeWithWindow = false) => {
      const isCurrent = captureDaemonOperation(requestedSession.target);
      if (daemonRestartBlocksInteractions()) {
        return;
      }
      if (hostConnections.isPaused(requestedSession.target)) {
        setListError("This host is disconnected. Connect the host to resume its sessions.");
        return;
      }
      const managed =
        requestedSession.target.kind === "local"
          ? taskWorkspaceRef.current.tasks.find(
              (task) =>
                task.active_run?.interactive?.session_id ===
                requestedSession.session_id,
            )
          : undefined;
      if (managed) {
        taskWorkspaceRef.current.openTask(managed);
        return;
      }
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

      if (
        sameSession(attachment.state.session, session) &&
        sameSshEndpoint(attachment.state.session!.target, session.target)
      ) {
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
            !isCurrent() ||
            daemonRestartBlocksInteractions()
          ) {
            return;
          }
          await attachment.reconnect();
          return;
        }
      }
      if (
        !isCurrent() ||
        daemonRestartBlocksInteractions()
      ) {
        return;
      }
      await attachment.connect(session, {
        resize_with_window: resizeWithWindow,
      });
    },
    [captureDaemonOperation, attachment, daemonRestartBlocksInteractions, renderer, hostConnections.isPaused],
  );

  const recoverHost = async (
    candidate: ConnectionTarget,
    remote_info: RemoteIdentity,
  ) => {
    if (candidate.kind !== "ssh") return null;
    await verifyHostConnectionStatus(candidate);
    const recovered = recoverRemoteHost(
      workspace.viewRef.current,
      candidate,
      remote_info,
    );
    if (!recovered) return null;
    await workspace.replaceView((current) => recoverRemoteHost(current, candidate, remote_info)!.view);
    if (workspace.isClosing()) throw new Error("Workspace is closing.");
    refreshGuardRef.current.recordMutation();
    renderer?.remapSessions(recovered.key_changes);
    setTabShellStates((current) => remapStateKeys(current, recovered.key_changes));
    const host_keys = new Set(recovered.view.targets.map(targetKey));
    notifications.resolve(`host:${targetKey(recovered.target)}`);
    setTargetErrors((current) => new Map(
      [...current].filter(([key]) => host_keys.has(key) && key !== targetKey(recovered.target)),
    ));
    sessionsRef.current = workspace.viewRef.current.sessions;
    tabsRef.current = workspace.viewRef.current.tabs;
    activeTabKeyRef.current = workspace.viewRef.current.active_tab_key;
    portForwarding.resumeHost(candidate.host_id!);
    await portForwarding.refreshTarget(recovered.target);
    if (workspace.isClosing()) throw new Error("Workspace is closing.");
    await verifyHostConnectionStatus(recovered.target);
    return recovered.target;
  };

  // Projected aliases have not passed through Add host's verification. Pin the
  // environment before giving them durable session or forward ownership.
  const prepareHostTarget = useCallback(async (target: ConnectionTarget): Promise<ConnectionTarget> => {
    if (target.kind !== "ssh") return target;
    if (target.unavailable) throw new Error(target.unavailable);
    const host = workspace.viewRef.current.hosts.find((item) => item.host_id === target.host_id);
    if (!host || (!isVirtualHost(host) && !host.host_id.startsWith("ssh-config:") && !host.host_id.startsWith("tailscale:"))) return target;
    const expected = expectedHostIdentity(host);
    if (expected) return { ...target, remote_info: expected };
    const pending = hostVerificationRef.current.get(host.host_id);
    if (pending) return pending;
    const verification = (async () => {
      const attempt_id = crypto.randomUUID();
      let promptRequired!: (failure: Error) => void;
      const prompted = new Promise<never>((_resolve, reject) => { promptRequired = reject; });
      const identity = await Promise.race([
        probeSshHost(target, attempt_id, () => {
          void cancelSshProbe(attempt_id).catch(() => undefined);
          promptRequired(new Error("Authentication is required. Use Connect host to authenticate, then try again."));
        }),
        prompted,
      ]);
      let verified: ConnectionTarget = target;
      await workspace.replaceView((current) => {
        const recovered = recoverRemoteHost(current, target, identity);
        if (!recovered) throw new Error("This host is no longer available.");
        verified = recovered.target;
        return recovered.view;
      });
      return verified;
    })();
    hostVerificationRef.current.set(host.host_id, verification);
    try {
      return await verification;
    } finally {
      hostVerificationRef.current.delete(host.host_id);
    }
  }, [workspace.replaceView, workspace.viewRef]);

  async function managePortForwards(target: ConnectionTarget) {
    if (target.kind !== "ssh") return;
    try {
      const verified = await prepareHostTarget(target);
      if (verified.kind !== "ssh") return;
      await portForwarding.refreshTarget(verified);
      setPortForwardTarget(verified);
    } catch (failure) {
      setListError(errorMessage(failure));
    }
  }

  async function setPortForwardEnabled(target: SshConnectionTarget, forward: WorkspacePortForward, enabled: boolean) {
    const verified = enabled ? await prepareHostTarget(target) : target;
    if (verified.kind !== "ssh") return;
    if (enabled && !target.remote_info && verified.remote_info) await portForwarding.refreshTarget(verified);
    await portForwarding.setEnabled(verified, forward, enabled);
  }

  const resumeHost = useWorkspaceConnections({
    getView: () => workspace.viewRef.current,
    ready: workspace.ready,
    closing: workspace.closing,
    tabs,
    active_tab_key: activeTabKey,
    activateTab,
    refreshHost: refresh,
    canConnect: (target) => !hostConnections.isPaused(target),
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

      await attachment.closeSession(session);
      await sessionCache({ kind: "archive", host_key: targetKey(session.target), session_id: session.session_id, reason: "Tab closed" });
      const wasActive = activeTabKeyRef.current === identity;
      const closed = closeTerminalTab(tabsRef.current, identity);
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
        (nextTab?.target.kind === "local" && daemonEpoch !== daemonEpochRef.current) ||
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

  const archiveSession = useCallback(async (session: SessionSummary, reason: string) => {
    await sessionCache({ kind: "archive", host_key: targetKey(session.target), session_id: session.session_id, reason });
  }, []);

  const dismissingSessions = useRef(new Set<string>());
  const dismissSession = useCallback(async (session: SessionSummary) => {
    const key = sessionKey(session);
    if (dismissingSessions.current.has(key)) return;
    dismissingSessions.current.add(key);
    try {
      await archiveSession(session, attachment.state.message ?? "Session no longer exists");
      refreshGuardRef.current.recordMutation();
      setSessions((current) => removeSession(current, sessionKey(session)));
      setSessionShellStates((current) => forgetShellState(current, sessionKey(session)));
      await closeTab(session);
      await persistWorkspace();
    } catch (failure) { setListError(errorMessage(failure)); }
    finally { dismissingSessions.current.delete(key); }
  }, [archiveSession, attachment.state.message, closeTab, setSessions, setSessionShellStates, persistWorkspace]);

  const removeHost = useCallback(
    async (target: ConnectionTarget) => {
      if (target.kind === "local" || daemonRestartBlocksInteractions()) {
        return;
      }
      for (const forward of workspace.port_forwards.filter(
        (item) => item.host_id === target.host_id && item.enabled,
      )) {
        await portForwarding.setEnabled(target, forward, false);
      }
      for (const credentialTarget of removableHostCredentials(
        workspace.viewRef.current, target.host_id!, attachment.state.session?.target,
      )) {
        await forgetSshCredentials(credentialTarget);
      }
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
      workspace.update("port_forwards", (current) =>
        current.filter((forward) => forward.host_id !== target.host_id),
      );
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
      portForwarding.setEnabled,
      workspace.port_forwards,
      workspace.update,
    ],
  );

  const create = useCallback(
    async (
      target: ConnectionTarget,
      workingDirectory: string | null,
    ): Promise<void> => {
      if (
        !workspace.ready ||
        workspace.isClosing() ||
        creatingRef.current ||
        daemonRestartConfirmationRef.current ||
        restartingDaemonRef.current
      ) {
        throw new Error(
          "Shell creation is currently unavailable. Try again when the workspace is ready.",
        );
      }
      const isCurrent = captureDaemonOperation(target);
      creatingRef.current = true;
      creatingTargetRef.current = target;
      setCreating(true);
      setListError(null);
      try {
        const session = await createSession({
          target,
          working_directory: workingDirectory,
          terminal_size: measuredSize(renderer),
        });
        if (!isCurrent()) {
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
          if (isCurrent()) {
            setListError(
              `Shell ${session.name} was created, but opening its tab failed. Select the existing session to retry. ${errorMessage(failure)}`,
            );
          }
        }
      } finally {
        if (isCurrent()) {
          creatingRef.current = false;
          creatingTargetRef.current = null;
          setCreating(false);
        }
      }
    },
    [captureDaemonOperation, activateTab, renderer, workspace.ready, workspace.isClosing, setSessions, persistWorkspace],
  );

  const disconnect = useCallback(
    async (session: SessionSummary) => {
      const isCurrent = captureDaemonOperation(session.target);
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
        if (isCurrent()) {
          setDisconnectingSessionKey((current) =>
            current === identity ? null : current,
          );
        }
      }
    },
    [captureDaemonOperation, closeTab, daemonRestartBlocksInteractions],
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
      await archiveSession(session, "Removed from this client");
      await closeTab(session);
      setSessions((current) => removeSession(current, sessionKey(session)));
      setSessionShellStates((current) =>
        forgetShellState(current, sessionKey(session)),
      );
      await persistWorkspace();
    },
    [
      attachment,
      archiveSession,
      closeTab,
      daemonRestartBlocksInteractions,
      setSessions,
      setSessionShellStates,
      persistWorkspace,
    ],
  );

  const close = useCallback(
    async (session: SessionSummary) => {
      const isCurrent = captureDaemonOperation(session.target);
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
          if (!isCurrent()) {
            return;
          }
          if (errorCode(error) !== "session_not_found") {
            setListError(errorMessage(error));
            return;
          }
        }
        if (!isCurrent()) {
          return;
        }
        await archiveSession(session, "Session terminated");
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
      } catch (failure) {
        setListError(errorMessage(failure));
      } finally {
        closingSessionKeysRef.current.delete(identity);
        if (isCurrent()) {
          setClosingSessionKeys((current) => {
            const next = new Set(current);
            next.delete(identity);
            return next;
          });
        }
      }
    },
    [captureDaemonOperation, attachment, archiveSession, closeTab, daemonRestartBlocksInteractions],
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
    const isLocalKey = (key: string | null) => key !== null && targetKeyFromSessionKey(key) === "local";
    closingSessionKeysRef.current = new Set([...closingSessionKeysRef.current].filter((key) => !isLocalKey(key)));
    if (creatingTargetRef.current?.kind === "local") {
      creatingRef.current = false;
      creatingTargetRef.current = null;
      setCreating(false);
      setNewShellOpen(false);
    }
    renderer?.forgetLocalSessions();
    attachment.resetAfterDaemonRestart();
    const markLocalMissing = (current: SessionSummary[]): SessionSummary[] =>
      current.map((session) =>
        session.target.kind === "local"
          ? { ...session, status: "missing" }
          : session,
      );
    setSessions(markLocalMissing);
    setTabs(markLocalMissing);
    if (isLocalKey(pendingCloseSessionKeyRef.current)) {
      pendingCloseSessionKeyRef.current = null;
      setPendingCloseSessionKey(null);
    }
    setClosingSessionKeys(new Set(closingSessionKeysRef.current));
    setDisconnectingSessionKey((current) => isLocalKey(current) ? null : current);
    setLoading(false);
    setListError(null);
    setTargetErrors((current) => {
      const next = new Map(current);
      next.delete("local");
      return next;
    });
  }, [attachment, renderer, setSessions, setTabs]);

  const component_event_error = useComponentActionEvents((event, affected) => {
    const affected_keys = new Set(affected.map(sessionKey));
    for (const session of [...sessionsRef.current, ...tabsRef.current]) {
      if (componentResetMatches(event, session, null)) affected_keys.add(sessionKey(session));
    }
    XtermRenderer.forgetRestartedSessions(affected_keys);
    if (event.scope === "local") {
      clearLocalDaemonState();
      return;
    }
    refreshGuardRef.current.recordMutation();
    attachment.forgetRestartedSessions(affected_keys);
    const markMissing = (current: SessionSummary[]) => current.map((session) => affected_keys.has(sessionKey(session)) ? { ...session, status: "missing" as const } : session);
    setSessions(markMissing);
    setTabs(markMissing);
    setTabShellStates((current) => new Map([...current].filter(([key]) => !affected_keys.has(key))));
    setSessionShellStates((current) => new Map([...current].filter(([key]) => !affected_keys.has(key))));
  });
  useEffect(() => { if (component_event_error) setListError(component_event_error); }, [component_event_error]);

  const executeAboutAction = async (preflight: ComponentActionPreflight) => {
    const execute = () => executeComponentAction(preflight.action_token);
    if (preflight.component === "taskd" && preflight.location === "local") return taskWorkspace.performComponentAction(execute);
    if (preflight.component !== "rmuxd" || preflight.location !== "local") return execute();
    if (restartingDaemonRef.current || creatingRef.current) throw new Error("Wait for the current local session operation to finish.");
    restartingDaemonRef.current = true;
    setRestartingDaemon(true);
    const before = daemonEpochRef.current;
    try {
      const result = await execute();
      // Native events also update other windows. A successful result is a
      // fallback if this window did not receive the committed reset event.
      if (daemonEpochRef.current === before) clearLocalDaemonState();
      return result;
    } finally {
      restartingDaemonRef.current = false;
      setRestartingDaemon(false);
    }
  };

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

  useEffect(() => {
    for (const state of attachment.states) {
      const session = state.session;
      if (!session || sameSession(session, attachment.state.session) || !tabsRef.current.some((tab) => sameSession(tab, session))) continue;
      const key = sessionKey(session);
      if (state.shell_state) {
        const shell_state = state.shell_state;
        setTabShellStates((current) => rememberShellState(current, key, shell_state, { replaceEqualRevision: true }));
        setSessionShellStates((current) => rememberShellState(current, key, shell_state, { replaceEqualRevision: true }));
      }
      const status: SessionSummary["status"] | null = state.phase === "ended" ? "exited"
        : state.phase === "error" ? state.error_code === "session_not_found" ? "missing" : "unreachable"
        : state.phase === "attached" ? "running" : null;
      const update = (current: SessionSummary[]) => {
        const resized = syncSessionTerminalSize(current, key, session.terminal_size);
        return status && resized.some((candidate) => sameSession(candidate, session) && candidate.status !== status)
          ? resized.map((candidate) => sameSession(candidate, session) ? { ...candidate, status } : candidate)
          : resized;
      };
      refreshGuardRef.current.recordMutation();
      setSessions((current) => {
        const next = update(current);
        sessionsRef.current = next;
        return next;
      });
      setTabs((current) => {
        const next = update(current);
        tabsRef.current = next;
        return next;
      });
    }
  }, [attachment.states, setSessions, setTabs, setSessionShellStates]);

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
  useWindowTitle(
    utility_page ? (utility_page === "about" ? "About rmux" : "Credentials") : taskWorkspace.active
      ? (taskWorkspace.activeTask?.definition.name ??
          taskWorkspace.saved?.definition.name ??
          "Task definition")
      : compactTerminalTitleParts(activeTitle),
  );

  const commands: AppCommand[] = buildTerminalCommands(
    {
      targets,
      connectableHostKeys,
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
      showAddHost: openAddHost,
      showAddRoutedHost: openAddHost,
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
      refreshSessions: () => void workspace.refreshHostDiscovery().then(() =>
        Promise.all([refresh(), hostConnections.refresh()])),
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
        connectHostMethod(target);
      },
      showConnectHost: () => {
        setPaletteOpen(false);
        setConnectHostOpen(true);
      },
      configureHost: (target) => {
        if (target.kind === "ssh") setHostSettingsId(target.host_id!);
      },
      removeHost,
      managePortForwards: (target) => { void managePortForwards(target); },
      saveWorkspace: () => persistWorkspace(true),
      configureKeybindings: () => setKeybindingsOpen(true),
      reloadKeybindings: keybindings.reload,
    },
  ).map((command) => {
    const base = {
      ...command,
      keybinding: keybindings.bindings.get(command.id),
    };
    if (command.id === COMMAND_IDS.disconnect && taskWorkspace.active)
      return {
        ...base,
        title: "Close tab",
        enabled: true,
        isEnabled: () => true,
        focusTerminalAfterRun: false,
        run: () => taskWorkspace.close(workspaceTabKey(taskWorkspace.active!)),
      };
    if (
      command.id === COMMAND_IDS.nextTab ||
      command.id === COMMAND_IDS.previousTab
    )
      return {
        ...base,
        enabled: workspace.tab_order.length > 1,
        focusTerminalAfterRun: false,
        run: () => {
          const offset = command.id === COMMAND_IDS.nextTab ? 1 : -1;
          const order = workspace.tab_order;
          const key =
            order[
              (order.indexOf(activeTabKey ?? "") + offset + order.length) %
                order.length
            ];
          const taskTab = workspace.task_tabs.find(
            (tab) => workspaceTabKey(tab) === key,
          );
          if (taskTab) taskWorkspace.open(taskTab);
          else {
            const terminal = tabs.find((tab) => sessionKey(tab) === key);
            if (terminal) void activateTab(terminal);
          }
        },
      };
    return base;
  });
  for (const command of pane_commands) {
    const index = commands.findIndex((candidate) => candidate.id === command.id);
    if (index >= 0) commands[index] = command;
    else commands.push(command);
  }
  commands.push({
    id: COMMAND_IDS.about,
    category: "App",
    title: "About rmux",
    detail: "Check app, daemon, and connected host versions.",
    keywords: ["version", "protocol", "ctld", "rmuxd", "taskd", "update", "restart"],
    enabled: workspace.ready,
    keybinding: keybindings.bindings.get(COMMAND_IDS.about),
    focusTerminalAfterRun: false,
    run: () => setUtilityPage("about"),
  });
  commands.push({
    id: COMMAND_IDS.credentials,
    category: "App",
    title: "Credentials",
    detail: "Manage saved credential names and metadata.",
    keywords: ["keychain", "password", "passphrase", "ssh", "vpn", "forget"],
    enabled: workspace.ready,
    keybinding: keybindings.bindings.get(COMMAND_IDS.credentials),
    focusTerminalAfterRun: false,
    run: () => setUtilityPage("credentials"),
  });
  commands.push({
    id: COMMAND_IDS.showNotifications,
    category: "View",
    title: "Show Notifications",
    enabled: true,
    keybinding: keybindings.bindings.get(COMMAND_IDS.showNotifications),
    focusTerminalAfterRun: false,
    run: () => notifications.setCenterOpen(true),
  }, {
    id: COMMAND_IDS.recoverSessionComponents,
    category: "Session",
    title: "Recover Session Components",
    enabled: false,
    visibleInPalette: false,
    focusTerminalAfterRun: false,
    isEnabled: (args) => !daemonRestartBlocksInteractions() && !!attachmentNotifications.recoverySession(args.value),
    run: (args) => {
      const session = attachmentNotifications.recoverySession(args?.value);
      if (!session) return;
      if (session.target.kind === "ssh") setHostFlow({ target: session.target, update_required: true });
      else requestDaemonRestart();
    },
  }, {
    id: COMMAND_IDS.reconnectNotificationAttachment,
    allow_concurrent: true,
    category: "Session",
    title: "Reconnect Notification Attachment",
    enabled: false,
    visibleInPalette: false,
    focusTerminalAfterRun: false,
    isEnabled: (args) => !daemonRestartBlocksInteractions() && attachmentNotifications.canReconnect(args.value),
    run: (args) => attachmentNotifications.reconnect(args?.value),
  }, {
    id: COMMAND_IDS.refreshTasks,
    category: "Tasks",
    title: "Refresh Tasks and Definitions",
    enabled: workspace.ready && !taskWorkspace.busy && !taskWorkspace.loading,
    keybinding: keybindings.bindings.get(COMMAND_IDS.refreshTasks),
    focusTerminalAfterRun: false,
    run: taskWorkspace.refresh,
  }, {
    id: COMMAND_IDS.openTask,
    category: "Tasks",
    title: "View Task",
    enabled: false,
    visibleInPalette: false,
    focusTerminalAfterRun: false,
    isEnabled: (args) => workspace.ready && taskWorkspace.tasks.some((task) => task.task_id === args.value),
    run: (args) => {
      const task = taskWorkspace.tasks.find((task) => task.task_id === args?.value);
      if (task) { setUtilityPage(null); taskWorkspace.openTask(task); }
    },
  });
  commands.push({
    id: COMMAND_IDS.restartTaskDaemon,
    category: "Tasks",
    title: "Restart taskd",
    detail: "Restart the local task daemon. Stop active tasks first.",
    enabled: workspace.ready && !taskWorkspace.busy,
    keybinding: keybindings.bindings.get(COMMAND_IDS.restartTaskDaemon),
    focusTerminalAfterRun: false,
    run: () => {
      workspace.update("sidebar_view", "tasks");
      return taskWorkspace.restartDaemon();
    },
  });
  commands.push({
    id: "task.new",
    category: "Tasks",
    title: "New task definition",
    enabled: workspace.ready && !taskWorkspace.busy,
    focusTerminalAfterRun: false,
    run: taskWorkspace.newDefinition,
  });
  for (const action of ["start_task", "stop_task", "restart_task"] as const) {
    commands.push({
      id: `task.${action}`,
      category: "Tasks",
      title:
        action === "start_task"
          ? "Start task"
          : action === "stop_task"
            ? "Stop task"
            : "Restart task",
      enabled:
        !!taskWorkspace.activeTask &&
        !taskWorkspace.busy &&
        (action === "start_task"
          ? !taskWorkspace.activeTask.active_run
          : action === "stop_task"
            ? !!taskWorkspace.activeTask.active_run
            : true),
      focusTerminalAfterRun: false,
      run: () => {
        if (taskWorkspace.activeTask)
          void taskWorkspace.action(taskWorkspace.activeTask, action);
      },
    });
  }
  const shortcutLabel = (id: string) => {
    const binding = keybindings.bindings.get(id);
    return binding ? formatKeybinding(binding, shortcutPlatform) : "";
  };
  const paletteShortcutLabel = shortcutLabel(COMMAND_IDS.showPalette);
  const closeShortcutLabel = shortcutLabel(COMMAND_IDS.close);
  const [archives_open, setArchivesOpen] = useState(false);
  const dialogOpen = archives_open ||
    about_dialog_open || credentials_dialog_open ||
    vpn.editor !== null ||
    taskWorkspace.editorId !== null ||
    portForwardTarget !== null ||
    keybindingsOpen ||
    newShellOpen ||
    importOpen ||
    pendingForget !== null ||
    hostFlow !== null ||
    addHostOpen || connectHostOpen || methodDraft !== null || hostSettingsId !== null ||
    pendingCloseSessionKey !== null ||
    daemonRestartConfirmationPending;

  useLayoutEffect(() => {
    dispatcher.update(
      commands.map((command) => ({
        ...command,
        run: (args) => {
          if (!command.keepPaletteOpen) setPaletteOpen(false);
          if (command.id !== COMMAND_IDS.restartDaemon) cancelDaemonRestart();
          if (utility_page && command.focusTerminalAfterRun !== false) setUtilityPage(null);
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
      if (!utility_page && !dialogOpen && !paletteOpen && !daemonRestartBlocksInteractions()) {
        attachment.handleInput(data);
      }
    },
    [attachment, utility_page, daemonRestartBlocksInteractions, dialogOpen, paletteOpen],
  );

  function dismissPalette() {
    setPaletteOpen(false);
    cancelDaemonRestart();
    if (!utility_page) requestAnimationFrame(() => renderer?.focus());
  }

  return (
      <CommandProvider
        value={{ dispatcher, keybinding: (id) => keybindings.bindings.get(id) }}
      >
        <CommandBindings platform={shortcutPlatform} />
        <div className="workbench">
          <main
            className="app-shell"
            inert={
              !workspace.ready || workspace.closing || dialogOpen || paletteOpen
            }
          >
            <WorkspaceSidebar
              on_about={() => executeCommandById(COMMAND_IDS.about)}
              about_open={utility_page === "about"}
              on_credentials={() => executeCommandById(COMMAND_IDS.credentials)}
              credentials_open={utility_page === "credentials"}
              on_keybindings={() => executeCommandById(COMMAND_IDS.configureKeybindings)}
              selected={workspace.sidebar_view}
              onSelect={(view) => {
                setUtilityPage(null);
                workspace.update("sidebar_view", view);
                if (view === "ports") void portForwarding.refreshAll();
              }}
              vpn={<VpnSidebar model={vpn} />}
              vpn_state={vpn.status_loaded ? vpnAggregateState(vpn.statuses) : undefined}
              vpn_active_count={vpn.statuses.length}
              vpn_status_stale={vpn.status_stale}
              tasks={
                <TaskSidebar
                  model={taskWorkspace}
                  definitions={taskWorkspace.definitions}
                  references={workspace.task_references}
                />
              }
              sessions={
                <SessionSidebar
                  on_archives={() => setArchivesOpen(true)}
                  targets={sidebarTargets}
                  hosts={workspace.hosts}
                  connectableHostKeys={connectableHostKeys}
                  hostConnections={hostConnections.statuses}
                  attachmentStates={new Map(attachment.states.flatMap((state) => state.session ? [[sessionKey(state.session), state] as const] : []))}
                  onDisconnectHost={(target) => {
                    void hostConnections.disconnect(target).catch((failure) => setListError(errorMessage(failure)));
                  }}
                  targetErrors={targetErrors}
                  sessions={sessions}
                  interactiveTasks={taskWorkspace.tasks}
                  shellStates={displayedSessionShellStates}
                  selectedSessionKey={activeTabKey}
                  openTabSessionKeys={openTabSessionKeys}
                  loading={loading || !workspace.ready}
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
                  onSelectTask={taskWorkspace.openTask}
                  onStopTask={(task) =>
                    void taskWorkspace.action(task, "stop_task")
                  }
                  onAddExisting={() =>
                    executeCommandById(COMMAND_IDS.addExistingSession)
                  }
                  onAddHost={() => executeCommandById(COMMAND_IDS.addHost)}
                  onChooseHost={connectableTargets.length > 0
                    ? () => executeCommandById(COMMAND_IDS.connectHost)
                    : undefined}
                  onHostSettings={(target) => {
                    executeCommandById(COMMAND_IDS.configureHost, { target_key: targetKey(target) });
                  }}
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
                  onPortForward={(target) =>
                    executeCommandById(COMMAND_IDS.managePortForwards, {
                      target_key: targetKey(target),
                    })
                  }
                />
              }
              ports={
                <PortForwardingSidebar
                  targets={targets.filter(
                    (target): target is SshConnectionTarget =>
                      target.kind === "ssh",
                  )}
                  forwards={workspace.port_forwards}
                  statuses={portForwarding.statuses}
                  busy={portForwarding.busy}
                  hostErrors={portForwarding.hostErrors}
                  refreshing={portForwarding.refreshing}
                  lastRefreshedAt={portForwarding.lastRefreshedAt}
                  onRefresh={() => void portForwarding.refreshAll()}
                  onSetEnabled={(target, forward, enabled) => {
                    void setPortForwardEnabled(target, forward, enabled).catch((failure) => setListError(errorMessage(failure)));
                  }}
                  onManage={(target) => { void managePortForwards(target); }}
                />
              }
            />
            <section className="terminal-workspace" hidden={utility_page !== null}>
              <TerminalTabs
                tabs={tabs}
                extra_tabs={workspace.task_tabs.map((tab) => {
                  const task = taskWorkspace.tasks.find(
                    (item) => item.task_id === tab.task_id,
                  );
                  return {
                    tab_key: workspaceTabKey(tab),
                    title: task?.definition.name ?? "Saved task",
                    host: tab.host_id === "local" ? "Local" : tab.host_id,
                    status: task ? taskState(task) : "unknown",
                  };
                })}
                tab_order={workspace.tab_order}
                on_select_extra={(key) => {
                  const tab = workspace.task_tabs.find(
                    (item) => workspaceTabKey(item) === key,
                  );
                  if (tab) taskWorkspace.open(tab);
                }}
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
              {taskWorkspace.active?.kind === "task" ? (
                <TaskDetail
                  key={`${taskWorkspace.active.host_id}:${taskWorkspace.active.task_id}`}
                  model={taskWorkspace}
                  saved={taskWorkspace.activeSaved}
                />
              ) : null}
              <div
                className="terminal-pane"
                hidden={
                  !!taskWorkspace.active &&
                  !(
                    taskWorkspace.activeTask?.definition.execution_mode ===
                      "interactive" && taskWorkspace.activeTask.active_run
                  )
                }
              >
                <TerminalToolbar
                  showInputControl={false}
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
                </div>
                <SessionViewSurface
                  prefix_settings={{ document: keybindings.document, bindings: keybindings.bindings, platform: shortcutPlatform }}
                  shortcuts_enabled={!utility_page && !dialogOpen && !paletteOpen && keybindings.ready && !workspace.closing}
                  input_enabled={!utility_page && !dialogOpen && !paletteOpen && !workspace.closing && !daemonRestartBlocksInteractions()}
                  on_command={executeCommandById}
                  on_pane_commands={setPaneCommands}
                  open_session_keys={attachment.session_keys}
                  session={attachment.state.session}
                  shell_state={attachment.state.shell_state}
                  renderer={renderer}
                  input_owned={attachment.state.input_lease.owned_by_client}
                  on_toggle_input={attachment.toggleInputLease}
                  on_promoted={(session) => importSession(session, null)}
                  on_select_terminal={(session) => attachment.connect(session, { resize_with_window: true, terminal_id: session.terminal_id })}
                  ended_message={attachment.state.phase === "ended" ? attachment.state.message ?? "Session exited" : attachment.state.error_code === "session_not_found" ? "Session no longer exists" : null}
                  on_dismiss={() => { if (attachment.state.session) void dismissSession(attachment.state.session); }}
                  phase={attachment.state.phase}
                  hasSession={attachment.state.session !== null}
                  has_cached_content={attachment.state.applied_sequence !== null}
                  onInput={handleTerminalInput}
                  onReady={setRenderer}
                />
                {attachment.controllers}
              </div>
            </section>
            <CredentialsPage
              visible={utility_page === "credentials"}
              targets={credential_targets}
              on_close={() => { setUtilityPage(null); requestAnimationFrame(() => renderer?.focus()); }}
              on_dialog_change={setCredentialsDialogOpen}
              on_manage_vpn={(connection_id) => {
                setUtilityPage(null);
                workspace.update("sidebar_view", "vpn");
                const connection = vpn.connections.find((item) => item.connection_id === connection_id);
                if (connection && connection.provider !== "tailscale") vpn.editConnection(connection);
                else void vpn.refresh();
              }}
            />
            <AboutPage
              visible={utility_page === "about"}
              on_close={() => { setUtilityPage(null); requestAnimationFrame(() => renderer?.focus()); }}
              on_dialog_change={setAboutDialogOpen}
              execute_action={executeAboutAction}
              on_restarted={(preflight) => {
                if (preflight.component === "taskd") void taskWorkspace.refresh();
                else { void vpn.refresh(); void hostConnections.refresh(); void portForwarding.refreshAll(); }
              }}
            />
          </main>
          <StatusBar
            state={attachment.state}
            show_terminal={!utility_page && (!taskWorkspace.active || (
              taskWorkspace.activeTask?.definition.execution_mode === "interactive" && !!taskWorkspace.activeTask.active_run
            ))}
            context_label={utility_page === "about" ? "About rmux" : utility_page === "credentials" ? "Credentials" : "Tasks"}
            inert={dialogOpen || paletteOpen || workspace.closing}
          >
            <NotificationBell store={notifications} />
          </StatusBar>
        </div>
        <Notifications store={notifications} blocked={dialogOpen || paletteOpen || workspace.closing} />
        {archives_open && <ArchiveBrowser targets={sidebarTargets} on_close={() => setArchivesOpen(false)} />}
        {taskWorkspace.editorId ? (
          <TaskEditor
            key={taskWorkspace.editorKey}
            model={taskWorkspace}
            saved={taskWorkspace.saved}
          />
        ) : null}
        {portForwardTarget ? (
          <PortForwardingDialog
            target={portForwardTarget}
            forwards={workspace.port_forwards.filter(
              (forward) => forward.host_id === portForwardTarget.host_id,
            )}
            statuses={portForwarding.statuses}
            busy={portForwarding.busy}
            onSetEnabled={(forward, enabled) =>
              setPortForwardEnabled(portForwardTarget, forward, enabled)
            }
            onChange={(forwards) => {
              workspace.update("port_forwards", (current) => [
                ...current.filter(
                  (forward) => forward.host_id !== portForwardTarget.host_id,
                ),
                ...forwards,
              ]);
            }}
            onUpdateAgent={() => {
              setPortForwardUpdateTarget(portForwardTarget);
              setPortForwardTarget(null);
              connectHostMethod(portForwardTarget);
            }}
            onClose={() => {
              setPortForwardTarget(null);
              requestAnimationFrame(() => renderer?.focus());
            }}
          />
        ) : keybindingsOpen ? (
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
            targets={connectionTargets}
            hosts={workspace.hosts}
            gateways={workspace.ssh_gateways}
            discoveryMessage={discoveryWarning ?? (workspace.discoveryLoading ? "Discovering Tailscale devices…" : null)}
            onVerifyHost={recoverHost}
            onConnectionChange={hostConnections.connectionChanged}
            onCreate={create}
            onClose={() => {
              setNewShellOpen(false);
              requestAnimationFrame(() => renderer?.focus());
            }}
          />
        ) : importOpen ? (
          <AddExistingSessionFlow
            targets={connectionTargets}
            hosts={workspace.hosts}
            gateways={workspace.ssh_gateways}
            discoveryMessage={discoveryWarning ?? (workspace.discoveryLoading ? "Discovering Tailscale devices…" : null)}
            known={sessions}
            onVerifyHost={recoverHost}
            onConnectionChange={hostConnections.connectionChanged}
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
        ) : connectHostOpen ? (
          <QuickInput
            title="Connect host"
            description={`Choose a saved host or a discovered SSH/Tailscale device.${discoveryWarning ? `\n${discoveryWarning}` : workspace.discoveryLoading ? "\nDiscovering Tailscale devices…" : ""}`}
            mode={{
              kind: "pick",
              choices: hostSelectorChoices(connectableTargets, workspace.hosts),
            }}
            onCancel={() => setConnectHostOpen(false)}
            onSubmit={(key) => {
              const target = connectableTargets.find((candidate) => targetKey(candidate) === key);
              if (!target) return;
              setConnectHostOpen(false);
              connectHostMethod(target);
            }}
          />
        ) : addHostOpen ? (
          <SshHostFlow
            suggestions={hostSuggestions}
            tailscaleDevices={tailscaleDevices}
            discoveryLoading={workspace.discoveryLoading}
            warning={discoveryWarning}
            gateways={workspace.ssh_gateways}
            vpn_connections={vpn.connections}
            vpn_statuses={vpn.statuses}
            vpn_loading={!vpn.catalog_loaded && vpn.catalog_loading}
            vpn_error={vpn.catalog_error ?? vpn.status_error}
            onSaveNewHost={saveNewHost}
            onClose={() => setAddHostOpen(false)}
          />
        ) : methodDraft && methodNameOpen ? (
          <QuickInput
            key="method-name"
            title={`Connection method · ${methodDraft.host_name}`}
            description="Give this method a name, such as Office network or Via gateway."
            mode={{ kind: "input", label: "Method name", initial_value: methodDraft.method_name }}
            onCancel={() => { setMethodDraft(null); setMethodNameOpen(false); }}
            onSubmit={(value) => {
              const name = value.trim();
              if (!name || name.length > 4096 || /[\x00-\x1f\x7f]/u.test(name)) return;
              setMethodDraft({ ...methodDraft, method_name: name });
              setMethodNameOpen(false);
            }}
          />
        ) : methodDraft ? (
          <SshHostFlow
            suggestions={hostSuggestions}
            warning={discoveryWarning}
            gateways={workspace.ssh_gateways}
            vpn_connections={vpn.connections}
            vpn_statuses={vpn.statuses}
            vpn_loading={!vpn.catalog_loaded && vpn.catalog_loading}
            vpn_error={vpn.catalog_error ?? vpn.status_error}
            initialTarget={methodDraft.initial_target}
            expectedIdentity={methodHost ? expectedHostIdentity(methodHost) : undefined}
            onSaveConnection={saveConnection}
            onConnectionChange={(target, state, message) => {
              if (target.kind === "ssh" && methodDraft.host_id) {
                hostConnections.connectionChanged({ ...target, host_id: methodDraft.host_id }, state, message);
              }
            }}
            onClose={() => { setMethodDraft(null); setMethodNameOpen(false); }}
          />
        ) : settingsHost ? (
          <HostSettingsDialog
            key={settingsHost.host_id}
            host={settingsHost}
            vpn_connections={vpn.connections}
            onSave={saveHostSettings}
            onAddMethod={() => editMethod(settingsHost)}
            onEditMethod={(method) => editMethod(settingsHost, method)}
            onConnect={(method) => connectHostMethod(hostTarget(settingsHost, workspace.ssh_gateways, method.method_id), method.method_id)}
            onClose={() => setHostSettingsId(null)}
          />
        ) : hostFlow !== null ? (
          <ConnectHostFlow
            suggestions={hostSuggestions}
            warning={discoveryWarning}
            target={hostFlow.target}
            host={workspace.hosts.find((host) => host.host_id === hostFlow.target.host_id)}
            selected_method_id={hostFlow.selected_method_id}
            gateways={workspace.ssh_gateways}
            updateRequired={portForwardUpdateTarget !== null || hostFlow.update_required === true}
            onVerified={recoverHost}
            onConnectionChange={hostConnections.connectionChanged}
            onConnected={(target) => {
              void resumeHost(target).catch((failure) =>
                setListError(errorMessage(failure)),
              );
              if (portForwardUpdateTarget && target.kind === "ssh") {
                setPortForwardTarget(target);
              }
            }}
            onClose={() => {
              setHostFlow(null);
              setPortForwardUpdateTarget(null);
              requestAnimationFrame(() => renderer?.focus());
            }}
          />
        ) : pendingCloseSessionKey ? (
          <QuickInput
            title="Terminate session"
            description={`Terminate ${sessions.find((session) => sessionKey(session) === pendingCloseSessionKey)?.name ?? "this session"} for all clients? This cannot be undone.${closeShortcutLabel ? ` Press ${closeShortcutLabel} to confirm.` : ""}`}
            confirm_command_id={COMMAND_IDS.close}
            mode={{
              kind: "confirm",
              confirm_label: "Terminate session",
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
