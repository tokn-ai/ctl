import { useEffect, useRef, useState } from "react";
import { QuickInput, type QuickInputMode } from "../commands/QuickInput";
import { remoteInstallProgressMode } from "./remoteInstallProgress";
import { GatewayRouteDialog } from "./GatewayRouteDialog";
import { tailscaleDeviceDetail, VIRTUAL_SSH_GROUP, VIRTUAL_TAILSCALE_GROUP } from "./hostChoices";
import { resolveSshGateways, tailscaleTarget } from "../../features/workspace/workspaceModel";
import { vpnRouteDetail } from "../../features/vpn/status";
import { hasVpnRoute, localVpnConnectionId } from "../../features/workspace/sshRoute";
import { sameSshEndpoint } from "../../features/workspace/remoteRecovery";
import { remoteVpnUpdateOwner } from "../../features/workspace/remoteVpnRecovery";
import { parseHostAddress } from "../../features/targets/hostAddress";
import { useSshIdentityFiles } from "../../features/targets/useSshIdentityFiles";
import {
  appLocalSshTarget,
  configuredSshTarget,
} from "../../features/targets/targets";
import { errorCode, errorMessage } from "../../lib/errors";
import {
  cancelSshProbe,
  forgetSshCredentials,
  installRemoteAgent,
  openVpnSignIn,
  restartRemoteCtmux,
  checkRemoteCtmuxRestart,
  probeSshHost,
  respondSshPrompt,
  saveSshConfigHost,
} from "../../lib/tauri";
import type {
  ConnectionTarget,
  RemoteIdentity,
  SshHostDefinition,
  SshHostStorage,
  SshPrompt,
  RemoteAgentInstallProgress,
  SshConnectionTarget,
  SshGatewayRouteStep,
  WorkspaceSshGateway,
  WorkspaceHost,
  HostConnectionChange,
  TailscaleDevice,
  VpnConnection,
  VpnStatus,
} from "../../lib/types";

export interface SshHostFlowProps {
  suggestions: readonly string[];
  tailscaleDevices?: readonly TailscaleDevice[];
  discoveryLoading?: boolean;
  warning: string | null;
  target?: ConnectionTarget;
  /** Begin verification on mount when a target was already selected. */
  autoConnect?: boolean;
  onConnectionChange?: HostConnectionChange;
  updateRequired?: boolean;
  complex?: boolean;
  /** Edit connection settings without entering the reconnect flow. */
  initialTarget?: SshConnectionTarget;
  editing_host_id?: string | null;
  expectedIdentity?: RemoteIdentity;
  onSaveNewHost?(
    name: string,
    target: SshConnectionTarget,
    remote_info: RemoteIdentity,
    gateways?: WorkspaceSshGateway[],
  ): Promise<void>;
  onSaveConnection?(
    target: SshConnectionTarget,
    gateways: WorkspaceSshGateway[],
    remote_info: RemoteIdentity,
  ): Promise<void>;
  gateways?: readonly WorkspaceSshGateway[];
  hosts?: readonly WorkspaceHost[];
  vpn_connections?: readonly VpnConnection[];
  vpn_statuses?: readonly VpnStatus[];
  vpn_loading?: boolean;
  vpn_error?: string | null;
  onSaveRoutedHost?(
    target: SshConnectionTarget,
    gateways: WorkspaceSshGateway[],
    remote_info: RemoteIdentity,
  ): Promise<void>;
  onVerified?(
    target: ConnectionTarget,
    remote_info: RemoteIdentity,
  ): Promise<ConnectionTarget | null>;
  onActivateHost?(
    destination: string,
    remote_info: RemoteIdentity,
  ): boolean | Promise<boolean>;
  onSaveHost?(
    definition: SshHostDefinition,
    storage: SshHostStorage,
    remote_info: RemoteIdentity,
  ): Promise<void>;
  onConnected?(target: ConnectionTarget): void;
  onClose(): void;
}

type Step =
  | "host"
  | "name"
  | "ssh_user"
  | "route"
  | "connect_through"
  | "auth"
  | "identity"
  | "restart_confirm"
  | "restarting"
  | "installing"
  | "progress"
  | "storage"
  | "save_retry"
  | "retry"
  | "update"
  | "reconnect";

export function SshHostFlow({
  suggestions,
  tailscaleDevices = [],
  discoveryLoading = false,
  warning,
  target,
  autoConnect = false,
  onConnectionChange,
  updateRequired = false,
  complex = false,
  initialTarget,
  editing_host_id,
  expectedIdentity,
  onSaveNewHost,
  onSaveConnection,
  gateways = [],
  hosts = [],
  vpn_connections = [],
  vpn_statuses = [],
  vpn_loading = false,
  vpn_error,
  onSaveRoutedHost,
  onActivateHost,
  onVerified,
  onSaveHost,
  onConnected,
  onClose,
}: SshHostFlowProps) {
  const editingConnection = Boolean(onSaveConnection);
  const availableTailscaleDevices = discoveryLoading ? [] : tailscaleDevices.filter((device) => device.online === true);
  const needsSshUser = target?.kind === "ssh" && Boolean(target.tailscale_node_id) && !target.remote_info && !target.user;
  const [step, setStep] = useState<Step>(() => {
    if (updateRequired) return "update";
    if (needsSshUser) return "ssh_user";
    if (target) return autoConnect ? "progress" : "reconnect";
    return complex || editingConnection ? "route" : "host";
  });
  const identityFiles = useSshIdentityFiles(step === "identity" || (editingConnection && step === "route"));
  const [address, setAddress] = useState(() => initialTarget ? targetAddress(initialTarget) : "");
  const [hostName, setHostName] = useState("");
  const [hostAlias, setHostAlias] = useState(initialTarget?.hostname ? initialTarget.destination : "");
  const [hostIdentityFile, setHostIdentityFile] = useState(initialTarget?.identity_file ?? "");
  const [sshConfigMaster, setSshConfigMaster] = useState(initialTarget?.use_ssh_config_master);
  const [exportToSshConfig, setExportToSshConfig] = useState(false);
  const [definition, setDefinition] = useState<SshHostDefinition>({
    alias: "",
    hostname: "",
    user: null,
    port: null,
    identity_file: null,
  });
  const [draftGateways, setDraftGateways] = useState<WorkspaceSshGateway[]>(
    () => gateways.map((gateway) => ({ ...gateway })),
  );
  const draftGatewaysRef = useRef(draftGateways);
  const originalGatewayIds = useRef(gateways.map((gateway) => gateway.gateway_id));
  const [vpnConnectionId, setVpnConnectionId] = useState(initialTarget?.vpn_connection_id);
  const [gatewayRoute, setGatewayRoute] = useState<SshGatewayRouteStep[]>(initialTarget?.gateway_route ?? []);
  const [error, setError] = useState<string | null>(null);
  const [needs_vpn_sign_in, setNeedsVpnSignIn] = useState(false);
  const [needs_remote_vpn_sign_in, setNeedsRemoteVpnSignIn] = useState(false);
  const [vpn_update_owner, setVpnUpdateOwner] = useState<ReturnType<typeof remoteVpnUpdateOwner>>(null);
  const [opening_vpn_sign_in, setOpeningVpnSignIn] = useState(false);
  const [prompt, setPrompt] = useState<SshPrompt | null>(null);
  const [saving, setSaving] = useState(false);
  const [canInstallAgent, setCanInstallAgent] = useState(updateRequired);
  const [install_progress, setInstallProgress] = useState<RemoteAgentInstallProgress | null>(null);
  const attemptRef = useRef<string | null>(null);
  const identityRef = useRef<RemoteIdentity | null>(null);
  const [needsUpdate, setNeedsUpdate] = useState(updateRequired);
  const restartingRef = useRef(false);
  const componentsUpdatedRef = useRef<ConnectionTarget | null>(null);
  const [needsDaemonRestart, setNeedsDaemonRestart] = useState(false);
  const candidateRef = useRef<ConnectionTarget | null>(target ?? null);
  const configuredRef = useRef(false);
  const selectedProviderTargetRef = useRef<SshConnectionTarget | null>(null);
  const closedRef = useRef(false);
  const uncommittedTargetRef = useRef<ConnectionTarget | null>(null);

  function forgetUncommitted() {
    const candidate = uncommittedTargetRef.current;
    uncommittedTargetRef.current = null;
    if (candidate) void forgetSshCredentials(candidate).catch(() => undefined);
  }

  function cancelAttempt() {
    const attempt = attemptRef.current;
    attemptRef.current = null;
    if (attempt) void cancelSshProbe(attempt).catch(() => undefined);
    if (attempt && candidateRef.current) onConnectionChange?.(candidateRef.current, "cancelled");
  }

  useEffect(() => {
    closedRef.current = false;
    let mounted = true;
    if (autoConnect && target && !updateRequired && !needsSshUser) {
      // Wait for StrictMode's setup/cleanup replay before starting one probe.
      void Promise.resolve().then(() => {
        if (mounted && !closedRef.current) void connect(target);
      });
    }
    return () => {
      mounted = false;
      closedRef.current = true;
      cancelAttempt();
      forgetUncommitted();
    };
  }, []);

  function close() {
    if (saving) return;
    closedRef.current = true;
    cancelAttempt();
    forgetUncommitted();
    onClose();
  }

  async function connect(candidate: ConnectionTarget) {
    if (candidate.kind === "ssh" && expectedIdentity) {
      candidate = { ...candidate, remote_info: expectedIdentity };
    }
    cancelAttempt();
    if (uncommittedTargetRef.current !== candidate) forgetUncommitted();
    // Methods may share credentials with another saved route. Only the legacy
    // standalone-host flow owns credentials it can safely discard on cancel.
    if (!target && !initialTarget && !onSaveConnection && !onSaveNewHost) uncommittedTargetRef.current = candidate;
    candidateRef.current = candidate;
    onConnectionChange?.(candidate, "connecting");
    const attempt = crypto.randomUUID();
    attemptRef.current = attempt;
    setError(null);
    setCanInstallAgent(false);
    setNeedsVpnSignIn(false);
    setNeedsRemoteVpnSignIn(false);
    setVpnUpdateOwner(null);
    setNeedsDaemonRestart(false);
    setPrompt(null);
    setStep("progress");
    try {
      const remote_info = await probeSshHost(candidate, attempt, (next) => {
        if (attemptRef.current === attempt && !closedRef.current)
          setPrompt(next);
      });
      if (attemptRef.current !== attempt || closedRef.current) return;
      setPrompt(null);
      if (expectedIdentity && expectedIdentity.remote_id !== remote_info.remote_id) {
        throw new Error("This connection reaches a different remote environment. Choose a connection for this host and account.");
      }
      identityRef.current = remote_info;
      setSaving(true);
      if (onSaveNewHost && candidate.kind === "ssh") {
        attemptRef.current = null;
        await saveNewHost(candidate, remote_info);
        return;
      }
      if (onSaveConnection && candidate.kind === "ssh") {
        if (exportToSshConfig && !initialTarget && candidate.hostname &&
          !candidate.gateway_route?.length && !candidate.vpn_connection_id && !suggestions.includes(candidate.destination)) {
          await saveSshConfigHost({
            alias: candidate.destination,
            hostname: candidate.hostname,
            user: candidate.user ?? null,
            port: candidate.port ?? null,
            identity_file: candidate.identity_file ?? null,
          });
          if (attemptRef.current !== attempt || closedRef.current) return;
        }
        await onSaveConnection(candidate, draftGatewaysRef.current, remote_info);
        if (attemptRef.current !== attempt || closedRef.current) return;
        onConnectionChange?.(candidate, "connected");
        uncommittedTargetRef.current = null;
        attemptRef.current = null;
        onClose();
        return;
      }
      const recovered = await onVerified?.(candidate, remote_info);
      if (attemptRef.current !== attempt || closedRef.current) return;
      onConnectionChange?.(recovered ?? candidate, "connected");
      if (complex && !target && candidate.kind === "ssh") {
        if (!onSaveRoutedHost) throw new Error("Routed host saving is unavailable.");
        await onSaveRoutedHost(candidate, draftGatewaysRef.current, remote_info);
        if (closedRef.current) return;
        uncommittedTargetRef.current = null;
        onClose();
      } else if (recovered || target) {
        uncommittedTargetRef.current = null;
        onConnected?.(recovered ?? candidate);
        onClose();
      } else if (configuredRef.current && candidate.kind === "ssh") {
        if (!(await onActivateHost?.(candidate.destination, remote_info)))
          throw new Error("That SSH host is already active.");
        if (closedRef.current) return;
        uncommittedTargetRef.current = null;
        onClose();
      } else {
        setStep("storage");
      }
      attemptRef.current = null;
    } catch (failure) {
      if (attemptRef.current !== attempt || closedRef.current) return;
      attemptRef.current = null;
      setPrompt(null);
      setError(errorMessage(failure));
      setNeedsVpnSignIn(errorCode(failure) === "vpn_sign_in_required");
      setNeedsRemoteVpnSignIn(errorCode(failure) === "remote_vpn_sign_in_required");
      setVpnUpdateOwner(remoteVpnUpdateOwner(candidate, failure));
      onConnectionChange?.(candidate, "error", errorMessage(failure));
      const code = errorCode(failure);
      const update = code === "ctl_agent_identity_unsupported" || code === "protocol_version_mismatch";
      const restart = code === "protocol_version_mismatch" && componentsUpdatedRef.current !== null
        && sameSshEndpoint(candidate, componentsUpdatedRef.current);
      setNeedsUpdate(update);
      setNeedsDaemonRestart(restart);
      setCanInstallAgent(!restart && (update || code === "ctl_agent_not_found"));
      if (restart) await checkRestart(candidate);
      else setStep("retry");
    } finally {
      if (!closedRef.current) setSaving(false);
    }
  }

  async function installAgent(candidate: ConnectionTarget, reconnect_candidate = candidate) {
    cancelAttempt();
    candidateRef.current = reconnect_candidate;
    onConnectionChange?.(reconnect_candidate, "connecting");
    const attempt = crypto.randomUUID();
    attemptRef.current = attempt;
    setError(null);
    setPrompt(null);
    setInstallProgress(null);
    setStep("installing");
    try {
      await installRemoteAgent(
        candidate,
        attempt,
        (next) => {
          if (attemptRef.current === attempt && !closedRef.current) setPrompt(next);
        },
        (next) => {
          if (attemptRef.current === attempt && !closedRef.current) setInstallProgress(next);
        },
      );
      if (attemptRef.current !== attempt || closedRef.current) return;
      attemptRef.current = null;
      componentsUpdatedRef.current = candidate;
      await connect(reconnect_candidate);
    } catch (failure) {
      if (attemptRef.current !== attempt || closedRef.current) return;
      attemptRef.current = null;
      setPrompt(null);
      setError(errorMessage(failure));
      setNeedsVpnSignIn(errorCode(failure) === "vpn_sign_in_required");
      setNeedsRemoteVpnSignIn(errorCode(failure) === "remote_vpn_sign_in_required");
      if (candidate === reconnect_candidate) {
        setCanInstallAgent(errorCode(failure) !== "vpn_sign_in_required" && errorCode(failure) !== "remote_vpn_sign_in_required");
      }
      onConnectionChange?.(reconnect_candidate, "error", errorMessage(failure));
      setStep("retry");
    }
  }

  async function checkRestart(candidate: ConnectionTarget) {
    const attempt = crypto.randomUUID();
    attemptRef.current = attempt;
    setStep("progress");
    try {
      await checkRemoteCtmuxRestart(candidate, attempt, (next) => {
        if (attemptRef.current === attempt && !closedRef.current) setPrompt(next);
      });
      if (attemptRef.current !== attempt || closedRef.current) return;
      setStep("restart_confirm");
    } catch (failure) {
      if (attemptRef.current !== attempt || closedRef.current) return;
      setNeedsDaemonRestart(false);
      setError(errorMessage(failure));
      setNeedsVpnSignIn(errorCode(failure) === "vpn_sign_in_required");
      setNeedsRemoteVpnSignIn(errorCode(failure) === "remote_vpn_sign_in_required");
      setStep("retry");
    } finally {
      if (attemptRef.current === attempt) {
        attemptRef.current = null;
        setPrompt(null);
      }
    }
  }

  async function restartDaemon(candidate: ConnectionTarget) {
    if (restartingRef.current) return;
    restartingRef.current = true;
    cancelAttempt();
    const attempt = crypto.randomUUID();
    attemptRef.current = attempt;
    setError(null);
    setPrompt(null);
    setStep("restarting");
    onConnectionChange?.(candidate, "connecting");
    try {
      await restartRemoteCtmux(candidate, attempt, (next) => {
        if (attemptRef.current === attempt && !closedRef.current) setPrompt(next);
      });
      if (attemptRef.current !== attempt || closedRef.current) return;
      attemptRef.current = null;
      await connect(candidate);
    } catch (failure) {
      if (attemptRef.current !== attempt || closedRef.current) return;
      attemptRef.current = null;
      setPrompt(null);
      setError(errorMessage(failure));
      setNeedsVpnSignIn(errorCode(failure) === "vpn_sign_in_required");
      setNeedsRemoteVpnSignIn(errorCode(failure) === "remote_vpn_sign_in_required");
      onConnectionChange?.(candidate, "error", errorMessage(failure));
      setStep("retry");
    } finally {
      restartingRef.current = false;
    }
  }

  function connectDefinition(next = definition) {
    const selected = selectedProviderTargetRef.current;
    const candidate = selected
      ? { ...selected }
      : onSaveNewHost && configuredRef.current
        ? configuredSshTarget(address)
        : appLocalSshTarget(next);
    if (candidate && (selected || onSaveNewHost && configuredRef.current) && next.identity_file) {
      candidate.identity_file = next.identity_file;
    }
    if (candidate) {
      try {
        void connect(resolveSshGateways({
          ...candidate,
          ...(vpnConnectionId ? { vpn_connection_id: vpnConnectionId, use_ssh_config_master: false } : {}),
          ...(gatewayRoute.length ? { gateway_route: gatewayRoute } : {}),
        }, draftGatewaysRef.current, hosts));
      } catch (failure) {
        setError(errorMessage(failure));
      }
    }
  }

  function selectedRouteChoice(): string {
    if (vpnConnectionId) return `vpn:${vpnConnectionId}`;
    if (!gatewayRoute.length) return "direct";
    if (gatewayRoute.length !== 1) return "custom_route";
    const step = gatewayRoute[0];
    if ("gateway_id" in step) return `gateway:${step.gateway_id}`;
    if ("host_id" in step) return `host:${JSON.stringify([step.host_id, step.method_id])}`;
    return "custom_route";
  }

  async function saveNewHost(candidate: SshConnectionTarget, remote_info: RemoteIdentity) {
    setSaving(true);
    setError(null);
    try {
      if (draftGatewaysRef.current.some((gateway) => !originalGatewayIds.current.includes(gateway.gateway_id))) {
        await onSaveNewHost?.(hostName, candidate, remote_info, draftGatewaysRef.current);
      } else await onSaveNewHost?.(hostName, candidate, remote_info);
      if (!closedRef.current) onClose();
    } catch (failure) {
      if (!closedRef.current) {
        setError(errorMessage(failure));
        setStep("save_retry");
      }
    } finally {
      if (!closedRef.current) setSaving(false);
    }
  }

  function routedHostCandidate(): SshConnectionTarget {
    return {
      ...routedHostSettings(),
      ...(editing_host_id ? { host_id: editing_host_id } : {}),
      ...(sshConfigMaster !== undefined ? { use_ssh_config_master: sshConfigMaster } : {}),
    };
  }

  function routedHostSettings(): SshConnectionTarget {
    const source = initialTarget ?? selectedProviderTargetRef.current;
    const destination = address.trim();
    const alias = hostAlias.trim();
    const identity_file = hostIdentityFile.trim();
    if (!destination) throw new Error("Enter the SSH host or config alias.");
    if (identity_file && /[\x00-\x1f\x7f]/u.test(identity_file)) {
      throw new Error("Enter a valid identity-file path.");
    }
    const parsed = parseHostAddress(destination);
    const unchangedAlias = source && !source.hostname && destination === source.destination;
    const enteredAlias = parsed?.hostname ?? destination;
    const configAlias = !source?.tailscale_node_id &&
      (source?.ssh_config_alias === enteredAlias ||
        !unchangedAlias && suggestions.includes(enteredAlias)) ? enteredAlias : null;
    if (configAlias || unchangedAlias) {
      const selectedAlias = configAlias ?? destination;
      if (alias && alias !== selectedAlias) {
        throw new Error("A saved SSH config host must keep its existing alias.");
      }
      const target = configAlias ? configuredSshTarget(configAlias) : {
        kind: "ssh" as const,
        destination,
        ...(source?.ssh_config_alias ? { ssh_config_alias: source.ssh_config_alias } : {}),
      };
      if (!target) throw new Error("Enter a valid SSH config host.");
      const sameAlias = source && !source.hostname && selectedAlias === source.destination;
      return {
        ...target,
        ...(sameAlias
          ? { user: source.user, port: source.port,
            ...(source.tailscale_node_id ? { tailscale_node_id: source.tailscale_node_id } : {}) }
          : {}),
        ...(parsed?.user ? { user: parsed.user } : {}),
        ...(parsed?.port ? { port: parsed.port } : {}),
        ...(identity_file ? { identity_file } : {}),
      };
    }
    if (!parsed) {
      throw new Error("Use [user@]hostname[:port], with IPv6 addresses in brackets. SSH flags are not accepted.");
    }
    const name = alias || parsed.hostname;
    if (!/^[a-zA-Z0-9_.:-]+$/u.test(name) || name.startsWith("-")) {
      throw new Error("Enter a name without spaces or SSH patterns.");
    }
    const target = appLocalSshTarget({
      alias: name,
      hostname: parsed.hostname,
      user: parsed.user,
      port: parsed.port,
      identity_file: identity_file || null,
    });
    if (!target) throw new Error("Enter valid SSH host settings.");
    if (source?.tailscale_node_id && parsed.hostname === (source.hostname ?? source.destination)) {
      target.tailscale_node_id = source.tailscale_node_id;
    }
    if (source?.ssh_config_alias === name && parsed.hostname === source.hostname) {
      target.ssh_config_alias = source.ssh_config_alias;
    }
    return target;
  }

  function answer(value: string) {
    const attempt = attemptRef.current;
    if (!attempt || !prompt) return;
    const promptId = prompt.prompt_id;
    const response = prompt.kind === "confirm" ? "yes" : value;
    setPrompt(null);
    void respondSshPrompt(attempt, promptId, response).catch((failure) => {
      if (attemptRef.current === attempt) {
        cancelAttempt();
        setError(errorMessage(failure));
        if (candidateRef.current) onConnectionChange?.(candidateRef.current, "error", errorMessage(failure));
        setStep("retry");
      }
    });
  }

  async function save(storage: SshHostStorage) {
    setSaving(true);
    setError(null);
    try {
      if (!identityRef.current) throw new Error("Connect to the host before saving it.");
      if (!onSaveHost) throw new Error("Host saving is unavailable.");
      await onSaveHost(definition, storage, identityRef.current);
      uncommittedTargetRef.current = null;
      if (!closedRef.current) onClose();
    } catch (failure) {
      if (!closedRef.current) setError(errorMessage(failure));
    } finally {
      if (!closedRef.current) setSaving(false);
    }
  }

  if (prompt)
    return (
      <QuickInput
        key={prompt.prompt_id}
        title={promptTitle(prompt)}
        description={prompt.message}
        warning={prompt.warning}
        mode={promptMode(prompt)}
        onSubmit={answer}
        onCancel={close}
      />
    );

  if (step === "route") {
    let defaultSshConfigMaster = false;
    try { defaultSshConfigMaster = Boolean(routedHostSettings().ssh_config_alias); }
    catch { /* Incomplete address entry has no provider default yet. */ }
    return (
      <GatewayRouteDialog
        title={editingConnection ? initialTarget ? "Edit connection method" : "Add connection method" : undefined}
        submitLabel={editingConnection ? "Verify and save" : undefined}
        target={{ kind: "ssh", host_id: editing_host_id ?? initialTarget?.host_id, destination: address.trim() || "New host", gateway_route: gatewayRoute, vpn_connection_id: vpnConnectionId }}
        vpn_connections={vpn_connections}
        vpn_statuses={vpn_statuses}
        vpn_loading={vpn_loading}
        vpn_error={vpn_error}
        gateways={draftGateways}
        hosts={hosts}
        targets={[]}
        hostSetup={{
          address,
          alias: hostAlias,
          identity_file: hostIdentityFile,
          suggestions,
          warning,
          identity_files: editingConnection ? identityFiles.identity_files : undefined,
          identity_loading: editingConnection && identityFiles.loading,
          identity_warning: editingConnection ? identityFiles.warnings.join("\n") : undefined,
          ssh_config_master: /Win/i.test(navigator.platform) ? undefined : {
            checked: sshConfigMaster ?? defaultSshConfigMaster,
            onChange: setSshConfigMaster,
          },
          export_to_ssh_config: editingConnection && !initialTarget ? {
            checked: exportToSshConfig,
            allowed: !suggestions.includes(address.trim()) && !suggestions.includes(hostAlias.trim()),
            onChange: setExportToSshConfig,
          } : undefined,
          onAddressChange: setAddress,
          onAliasChange: setHostAlias,
          onIdentityFileChange: setHostIdentityFile,
        }}
        readonlyExisting
        readonlyGatewayIds={originalGatewayIds.current}
        requireGateway={!editingConnection}
        closeLabel="Cancel"
        onSave={async (nextGateways, nextRoute, vpn_connection_id) => {
          const candidate = routedHostCandidate();
          draftGatewaysRef.current = nextGateways;
          setDraftGateways(nextGateways);
          setGatewayRoute(nextRoute);
          setVpnConnectionId(vpn_connection_id);
          void connect(resolveSshGateways(
            { ...candidate, gateway_route: nextRoute, ...(vpn_connection_id ? { vpn_connection_id, use_ssh_config_master: false } : {}) },
            nextGateways,
            hosts,
          ));
        }}
        onClose={close}
      />
    );
  }

  let title = "Add host";
  let description: string | undefined;
  let mode: QuickInputMode;
  let onBack: (() => void) | undefined;
  const back = (previous: Step) => () => {
    setError(null);
    setStep(previous);
  };
  switch (step) {
    case "host":
      title = "Add host · 1/4";
      description =
        "Enter [user@]hostname[:port], or choose a discovered host." +
        (warning ? `\n${warning}` : "");
      mode = {
        kind: "input",
        label: "SSH host",
        placeholder: "ctmux@127.0.0.1:2222",
        initial_value: address,
        suggestions: suggestions.length || availableTailscaleDevices.length || discoveryLoading
          ? {
              label: "Discovered hosts",
              items: [
                ...suggestions.map((host) => ({
                  id: `ssh-config:${host}`,
                  label: host,
                  group: VIRTUAL_SSH_GROUP,
                })),
                ...availableTailscaleDevices.map((device) => ({
                  id: `tailscale:${device.node_id}`,
                  label: device.name,
                  detail: tailscaleDeviceDetail(device),
                  group: VIRTUAL_TAILSCALE_GROUP,
                })),
              ],
              loading: discoveryLoading,
              loading_message: "Discovering hosts…",
              empty_message: "Enter a hostname to add a new host.",
              no_match_message:
                "No matching discovered hosts. Enter a hostname to add a new host.",
            }
          : undefined,
      };
      break;
    case "name":
      title = selectedProviderTargetRef.current ? "Host name · 2/5" : "Host name · 2/4";
      mode = {
        kind: "input",
        label: onSaveNewHost ? "Host name" : "Name / SSH alias",
        initial_value: onSaveNewHost ? hostName : definition.alias,
      };
      onBack = back("host");
      break;
    case "ssh_user":
      title = target ? "SSH user" : "SSH user · 3/5";
      description = "Choose the SSH account on this Tailscale device. Leave blank to use your SSH default." +
        (target ? " Choosing an account saves this host customization in ctmux after verification." : "");
      mode = {
        kind: "input",
        label: "SSH user",
        placeholder: "SSH default",
        initial_value: target && candidateRef.current?.kind === "ssh" ? candidateRef.current.user ?? "" : definition.user ?? "",
        submit_label: target ? "Connect" : "Continue",
      };
      if (!target) onBack = back("name");
      break;
    case "connect_through":
      title = selectedProviderTargetRef.current ? "Connect through · 4/5" : "Connect through · 3/4";
      description = "Choose how to reach this host. A saved VPN starts when needed. Disconnect it later from the VPN page." +
        (vpn_loading ? "\nLoading VPN connections…" : "") +
        (vpn_error ? `\nCould not load VPN connections: ${vpn_error}` : "");
      mode = {
        kind: "pick",
        initial_choice_id: selectedRouteChoice(),
        choices: [
          { id: "direct", label: "Direct", detail: "Use SSH settings without an app VPN or gateway." },
          ...(onSaveNewHost ? [{ id: "custom_route", label: "Build connection route…", detail: "Chain saved hosts, SSH gateways, and VPNs in connection order." }] : []),
          ...(onSaveNewHost ? vpn_connections.map((connection) => ({
            id: `vpn:${connection.connection_id}`,
            label: connection.name,
            detail: `VPN · ${vpnRouteDetail(connection, vpn_statuses)}`,
            group: "Saved VPNs",
          })) : []),
          ...(onSaveNewHost ? gateways.map((gateway) => ({
            id: `gateway:${gateway.gateway_id}`,
            label: gateway.name,
            detail: gateway.kind === "socks5" ? "SOCKS5 gateway" : "SSH gateway",
            group: "Saved gateways",
          })) : []),
          ...(onSaveNewHost ? hosts.filter((host) => host.host_id !== "local" && (!host.source || host.source === "saved"))
            .flatMap((host) => host.connection_methods.map((method) => ({
              id: `host:${JSON.stringify([host.host_id, method.method_id])}`,
              label: host.name,
              detail: `${method.name}${method.method_id === host.preferred_method_id ? " · Preferred" : ""} · Includes its saved route`,
              group: "Saved hosts",
            }))) : []),
        ],
      };
      onBack = back(selectedProviderTargetRef.current ? "ssh_user" : "name");
      break;
    case "auth":
      title = selectedProviderTargetRef.current ? "Authentication · 5/5" : "Authentication · 4/4";
      description =
        "OpenSSH authenticates this host. On macOS, you can choose whether to save a verified password or key passphrase in Keychain for Touch ID access.";
      mode = {
        kind: "pick",
        choices: [
          {
            id: "default",
            label: "SSH config / agent",
            detail: "Use existing keys and SSH settings.",
          },
          {
            id: "identity",
            label: "Identity file",
            detail: "Specify a private-key path; never copy the key.",
          },
          {
            id: "password",
            label: "Password / interactive authentication",
            detail: "Answer OpenSSH's masked prompt when requested.",
          },
        ],
      };
      onBack = back("connect_through");
      break;
    case "identity":
      title = "Identity file";
      description =
        "Choose a file from ~/.ssh using ↑/↓ and Enter, or type any private-key path. Key contents are not read for suggestions.";
      mode = {
        kind: "input",
        label: "Identity file",
        initial_value: definition.identity_file ?? "",
        placeholder: "~/.ssh/id_ed25519",
        suggestions: {
          label: "Identity files in ~/.ssh",
          items: identityFiles.identity_files.map((file) => ({
            id: file.path,
            label: file.display_path,
          })),
          loading: identityFiles.loading,
          loading_message: "Loading identity files…",
          empty_message:
            "No identity-file candidates in ~/.ssh. Enter a path manually.",
          no_match_message:
            "No matching identity files. Enter a path manually.",
          warning: identityFiles.warnings.join("\n") || undefined,
        },
      };
      onBack = back("auth");
      break;
    case "storage":
      title = "Save host";
      description = "Connection verified. Where should this host be saved?";
      mode = saving
        ? { kind: "progress", message: "Saving host…" }
        : {
            kind: "pick",
            choices: [
              {
                id: "ssh_config",
                label: "OpenSSH config",
                detail: "Reusable by ssh, ctl, and ctmux-app.",
              },
              {
                id: "local_storage",
                label: "This app only",
                detail: "Store non-secret connection settings locally.",
              },
            ],
          };
      if (!saving) onBack = back("auth");
      break;
    case "save_retry":
      title = "Could not save host";
      description = "The connection is verified. Retry saving this host to ctmux.";
      mode = saving
        ? { kind: "progress", message: "Saving host…" }
        : { kind: "pick", choices: [{ id: "save", label: "Retry saving host" }] };
      break;
    case "update":
    case "reconnect":
    case "retry":
      title = step === "update"
        ? "Update remote components"
        : step === "retry"
          ? "Could not connect"
          : "Connect host";
      description = needs_remote_vpn_sign_in
        ? "Sign in to the VPN on its SSH host. Open this connection route and check the remote VPN status to sign in, then choose Connect to continue."
        : needs_vpn_sign_in
        ? "Sign in to Tailscale with your browser, then choose Connect to continue. You can also manage this connection from the VPN page."
        : vpn_update_owner
        ? `The VPN execution host ${vpn_update_owner.name} needs updated components. Update that SSH host, then ctl will retry this connection through its VPN. Running sessions are preserved.`
        : needsDaemonRestart
          ? "The bundled components were installed, but the running ctmux daemon is still incompatible. Choose Force restart to end its existing terminal sessions, or Connect to check again. The update has not stopped running sessions."
          : (step === "retry" || step === "update") && canInstallAgent
            ? (needsUpdate
              ? "Update the remote components to match this app. Running sessions are preserved; an already-running daemon may still need to be restarted on the host."
              : "SSH is available, but this host is missing the ctmux remote components. Install them for this user or retry after installing them manually.")
            : "OpenSSH will ask for host verification or authentication if needed.";
      mode = opening_vpn_sign_in ? { kind: "progress", message: "Opening sign-in in your browser…" } : {
        kind: "pick",
        choices: [
          ...(vpn_update_owner ? [{ id: "update_vpn_host", label: `Update components on ${vpn_update_owner.name}`,
            detail: "Install the bundled components on the SSH host that runs this VPN." }] : []),
          ...(needs_vpn_sign_in ? [{ id: "vpn_sign_in", label: "Sign in to Tailscale" }] : []),
          ...((step === "retry" || step === "update") && canInstallAgent
            ? [
                {
                  id: "install_agent",
                  label: needsUpdate ? "Update remote components" : "Install remote components",
                  detail: "Install the bundled ctl-agent, ctld, ctmuxd, and ctl-taskd for this user.",
                },
              ]
            : []),
          ...(needsDaemonRestart ? [{ id: "restart_ctmux", label: "Force restart remote ctmux…" }] : []),
          ...(step === "update" ? [] : [{ id: "retry", label: "Connect" }]),
        ],
      };
      if (target && needsSshUser) onBack = back("ssh_user");
      else if (!target) onBack = back(complex || editingConnection ? "route" : configuredRef.current && !onSaveNewHost ? "host" : "auth");
      break;
    case "restart_confirm":
      title = "Force restart remote ctmux?";
      description = "The components were updated, but the running daemon is still incompatible. Force restart ends all terminal sessions on this host for this account, including sessions used by other clients. Running commands may be interrupted.";
      mode = { kind: "confirm", confirm_label: "Force restart", destructive: true };
      break;
    case "restarting":
      title = "Restarting remote ctmux";
      description = "Ending terminal sessions and starting the updated daemon. Closing this dialog stops waiting; it cannot undo the restart.";
      mode = { kind: "progress" };
      break;
    case "installing":
      title = vpn_update_owner ? `Updating components on ${vpn_update_owner.name}` : "Installing remote components";
      mode = remoteInstallProgressMode(install_progress);
      break;
    case "progress":
      title = "Connecting to host";
      description = candidateRef.current?.kind === "ssh" && hasVpnRoute(candidateRef.current)
        ? "Connecting the selected VPN if needed, then verifying the SSH connection and remote environment."
        : "Verifying the SSH connection and remote environment.";
      mode = { kind: "progress" };
  }

  async function submit(value: string) {
    setError(null);
    if (step === "host") {
      const selectedDevice = availableTailscaleDevices.find((device) => value === `tailscale:${device.node_id}`);
      if (selectedDevice) {
        const candidate = tailscaleTarget(selectedDevice);
        if (candidate.unavailable) {
          setError(candidate.unavailable);
          return;
        }
        selectedProviderTargetRef.current = candidate;
        configuredRef.current = false;
        candidateRef.current = candidate;
        setAddress(candidate.destination);
        setHostName(selectedDevice.name);
        setDefinition({
          alias: candidate.destination,
          hostname: candidate.hostname ?? candidate.destination,
          user: candidate.user ?? null,
          port: candidate.port ?? null,
          identity_file: candidate.identity_file ?? null,
        });
        setStep("name");
        return;
      }
      if (value.startsWith("tailscale:")) {
        setError(discoveryLoading
          ? "Wait for Tailscale discovery to finish before choosing a device."
          : "This Tailscale device is no longer online. Choose another host.");
        return;
      }
      const selectedAlias = value.startsWith("ssh-config:")
        ? value.slice("ssh-config:".length)
        : onSaveNewHost && suggestions.includes(value.trim()) ? value.trim() : null;
      if (selectedAlias) {
        const candidate = configuredSshTarget(selectedAlias);
        if (candidate) {
          selectedProviderTargetRef.current = null;
          configuredRef.current = true;
          candidateRef.current = candidate;
          if (onSaveNewHost) {
            setAddress(selectedAlias);
            setHostName(selectedAlias);
            setDefinition({ alias: selectedAlias, hostname: "", user: null, port: null, identity_file: null });
            setStep("name");
          } else {
            void connect(candidate);
          }
        }
        return;
      }
      const parsed = parseHostAddress(value);
      if (!parsed) {
        setError(
          "Use [user@]hostname[:port], with IPv6 addresses in brackets. SSH flags are not accepted.",
        );
        return;
      }
      configuredRef.current = false;
      selectedProviderTargetRef.current = null;
      setAddress(value);
      setHostName(parsed.hostname);
      setDefinition({ ...parsed, alias: parsed.hostname, identity_file: null });
      setStep("name");
    } else if (step === "name") {
      const alias = value.trim();
      if (onSaveNewHost) {
        if (!alias || /[\x00-\x1f\x7f-\x9f]/u.test(alias)) {
          setError("Enter a host name without control characters.");
          return;
        }
        setHostName(alias);
      } else if (!/^[a-zA-Z0-9_.:-]+$/u.test(alias) || alias.startsWith("-")) {
        setError("Enter a name without spaces or SSH patterns.");
        return;
      } else {
        setDefinition((current) => ({ ...current, alias }));
      }
      setStep(selectedProviderTargetRef.current ? "ssh_user" : "connect_through");
    } else if (step === "ssh_user") {
      const user = value.trim();
      if (user && !/^[a-zA-Z0-9_.-]+$/u.test(user)) {
        setError("Enter an SSH user without spaces, or leave blank to use your SSH default.");
        return;
      }
      const selected = target ? candidateRef.current : selectedProviderTargetRef.current;
      if (selected?.kind !== "ssh") return;
      const { user: _previousUser, ...candidate } = selected;
      const next = { ...candidate, ...(user ? { user } : {}) };
      if (target) {
        void connect(next);
      } else {
        selectedProviderTargetRef.current = next;
        setDefinition((current) => ({ ...current, user: user || null }));
        setStep("connect_through");
      }
    } else if (step === "connect_through") {
      if (value === "custom_route") {
        const selected = selectedProviderTargetRef.current;
        if (selected) {
          setAddress(targetAddress(selected));
          setHostAlias(selected.hostname ? selected.destination : "");
          setHostIdentityFile(selected.identity_file ?? "");
        }
        setStep("route");
        return;
      }
      const vpn = vpn_connections.find((connection) => value === `vpn:${connection.connection_id}`);
      const gateway = gateways.find((item) => value === `gateway:${item.gateway_id}`);
      const host_step = hosts.flatMap((host) => host.connection_methods.map((method) => ({ host, method })))
        .find(({ host, method }) => value === `host:${JSON.stringify([host.host_id, method.method_id])}`);
      if (value !== "direct" && !vpn && !gateway && !host_step) {
        setError("This connection is no longer available. Choose another route.");
        return;
      }
      if (host_step) {
        const next: SshGatewayRouteStep[] = [{ host_id: host_step.host.host_id, method_id: host_step.method.method_id, mode: "automatic" }];
        try {
          resolveSshGateways({ kind: "ssh", destination: address, gateway_route: next }, gateways, hosts);
          setVpnConnectionId(undefined);
          setGatewayRoute(next);
          setStep("auth");
        } catch (failure) {
          setError(errorMessage(failure));
        }
        return;
      }
      setVpnConnectionId(vpn?.connection_id);
      setGatewayRoute(gateway ? [{ gateway_id: gateway.gateway_id, mode: "automatic" }] : []);
      setStep("auth");
    } else if (step === "auth") {
      if (value === "identity") setStep("identity");
      else {
        const next = { ...definition, identity_file: null };
        setDefinition(next);
        connectDefinition(next);
      }
    } else if (step === "identity") {
      const identity_file = value.trim();
      if (!identity_file || /[\x00-\x1f\x7f]/u.test(identity_file)) {
        setError("Enter a valid private-key path.");
        return;
      }
      const next = { ...definition, identity_file };
      setDefinition(next);
      connectDefinition(next);
    } else if (step === "storage") {
      if (!saving && (value === "ssh_config" || value === "local_storage"))
        void save(value);
    } else if (step === "save_retry") {
      if (!saving && value === "save" && candidateRef.current?.kind === "ssh" && identityRef.current) {
        void saveNewHost(candidateRef.current, identityRef.current);
      }
    } else if (step === "restart_confirm" && candidateRef.current && value === "confirm") {
      void restartDaemon(candidateRef.current);
    } else if (
      (step === "retry" || step === "update" || step === "reconnect") &&
      candidateRef.current
    ) {
      if (value === "vpn_sign_in") {
        const candidate = candidateRef.current;
        if (candidate.kind !== "ssh" || opening_vpn_sign_in) return;
        const vpn_id = localVpnConnectionId(candidate);
        if (!vpn_id) return;
        setOpeningVpnSignIn(true);
        try {
          await openVpnSignIn(vpn_id);
        } catch (failure) {
          if (!closedRef.current) setError(errorMessage(failure));
        } finally {
          if (!closedRef.current) setOpeningVpnSignIn(false);
        }
      } else if (value === "update_vpn_host" && vpn_update_owner) {
        void installAgent(vpn_update_owner.target, candidateRef.current);
      } else if (value === "restart_ctmux") setStep("restart_confirm");
      else if (value === "install_agent") void installAgent(candidateRef.current);
      else void connect(candidateRef.current);
    }
  }

  return (
    <QuickInput
      key={`${step}:${saving}`}
      title={title}
      description={description}
      mode={mode}
      error={error}
      onSubmit={submit}
      onCancel={step === "restart_confirm" ? () => setStep("retry") : close}
      cancel_label={step === "restart_confirm" ? "Not now" : undefined}
      onBack={onBack}
    />
  );
}

function targetAddress(target: SshConnectionTarget): string {
  if (!target.hostname) return target.destination;
  const hostname = target.hostname.includes(":") ? `[${target.hostname}]` : target.hostname;
  return `${target.user ? `${target.user}@` : ""}${hostname}${target.port ? `:${target.port}` : ""}`;
}

function promptTitle(prompt: SshPrompt): string {
  switch (prompt.kind) {
    case "confirm":
      return "SSH host verification";
    case "secret":
      return "SSH authentication";
    case "credential_save":
      return "Save SSH credential?";
    case "credential_save_error":
      return "Credential not saved";
  }
}

function promptMode(prompt: SshPrompt): QuickInputMode {
  switch (prompt.kind) {
    case "confirm":
      return { kind: "confirm", confirm_label: "Trust and connect" };
    case "secret":
      return { kind: "input", label: "SSH response", secret: true };
    case "credential_save":
      return {
        kind: "pick",
        choices: [
          {
            id: "yes",
            label: "Yes",
            detail: "Save in Keychain and require Touch ID for future access.",
          },
          {
            id: "no",
            label: "No",
            detail: "Do not save this time; ask again after a future authentication.",
          },
          {
            id: "never",
            label: "Never",
            detail: "Never offer to save credentials for this SSH host.",
          },
        ],
      };
    case "credential_save_error":
      return { kind: "confirm", confirm_label: "Continue" };
  }
}
