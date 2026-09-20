import { useEffect, useRef, useState } from "react";
import { QuickInput, type QuickInputMode } from "../commands/QuickInput";
import { remoteInstallProgressMode } from "./remoteInstallProgress";
import { GatewayRouteDialog } from "./GatewayRouteDialog";
import { resolveSshGateways } from "../../features/workspace/workspaceModel";
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
} from "../../lib/types";

interface SshHostFlowProps {
  suggestions: readonly string[];
  warning: string | null;
  target?: ConnectionTarget;
  updateRequired?: boolean;
  complex?: boolean;
  /** Edit connection settings without entering the reconnect flow. */
  initialTarget?: SshConnectionTarget;
  expectedIdentity?: RemoteIdentity;
  onSaveNewHost?(
    name: string,
    target: SshConnectionTarget,
    remote_info: RemoteIdentity,
  ): Promise<void>;
  onSaveConnection?(
    target: SshConnectionTarget,
    gateways: WorkspaceSshGateway[],
    remote_info: RemoteIdentity,
  ): Promise<void>;
  gateways?: readonly WorkspaceSshGateway[];
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
  | "route"
  | "auth"
  | "identity"
  | "installing"
  | "progress"
  | "storage"
  | "save_retry"
  | "retry"
  | "update"
  | "reconnect";

export function SshHostFlow({
  suggestions,
  warning,
  target,
  updateRequired = false,
  complex = false,
  initialTarget,
  expectedIdentity,
  onSaveNewHost,
  onSaveConnection,
  gateways = [],
  onSaveRoutedHost,
  onActivateHost,
  onVerified,
  onSaveHost,
  onConnected,
  onClose,
}: SshHostFlowProps) {
  const editingConnection = Boolean(onSaveConnection);
  const [step, setStep] = useState<Step>(
    updateRequired ? "update" : target ? "reconnect" : complex || editingConnection ? "route" : "host",
  );
  const identityFiles = useSshIdentityFiles(step === "identity" || (editingConnection && step === "route"));
  const [address, setAddress] = useState(() => initialTarget ? targetAddress(initialTarget) : "");
  const [hostName, setHostName] = useState("");
  const [hostAlias, setHostAlias] = useState(initialTarget?.hostname ? initialTarget.destination : "");
  const [hostIdentityFile, setHostIdentityFile] = useState(initialTarget?.identity_file ?? "");
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
  const [gatewayRoute, setGatewayRoute] = useState<SshGatewayRouteStep[]>(initialTarget?.gateway_route ?? []);
  const [error, setError] = useState<string | null>(null);
  const [prompt, setPrompt] = useState<SshPrompt | null>(null);
  const [saving, setSaving] = useState(false);
  const [canInstallAgent, setCanInstallAgent] = useState(updateRequired);
  const [install_progress, setInstallProgress] = useState<RemoteAgentInstallProgress | null>(null);
  const attemptRef = useRef<string | null>(null);
  const identityRef = useRef<RemoteIdentity | null>(null);
  const [needsUpdate, setNeedsUpdate] = useState(updateRequired);
  const candidateRef = useRef<ConnectionTarget | null>(target ?? null);
  const configuredRef = useRef(false);
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
  }

  useEffect(() => {
    closedRef.current = false;
    return () => {
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
    const attempt = crypto.randomUUID();
    attemptRef.current = attempt;
    setError(null);
    setCanInstallAgent(false);
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
          !candidate.gateway_route?.length && !suggestions.includes(candidate.destination)) {
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
        uncommittedTargetRef.current = null;
        attemptRef.current = null;
        onClose();
        return;
      }
      const recovered = await onVerified?.(candidate, remote_info);
      if (attemptRef.current !== attempt || closedRef.current) return;
      if (complex && !target && candidate.kind === "ssh") {
        if (!onSaveRoutedHost) throw new Error("Routed host saving is unavailable.");
        await onSaveRoutedHost(candidate, draftGatewaysRef.current, remote_info);
        if (closedRef.current) return;
        uncommittedTargetRef.current = null;
        onClose();
      } else if (recovered || target) {
        uncommittedTargetRef.current = null;
        onConnected?.(recovered ?? target!);
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
      const update = errorCode(failure) === "ctl_agent_identity_unsupported";
      setNeedsUpdate(update);
      setCanInstallAgent(update || errorCode(failure) === "ctl_agent_not_found");
      setStep("retry");
    } finally {
      if (!closedRef.current) setSaving(false);
    }
  }

  async function installAgent(candidate: ConnectionTarget) {
    cancelAttempt();
    candidateRef.current = candidate;
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
      await connect(candidate);
    } catch (failure) {
      if (attemptRef.current !== attempt || closedRef.current) return;
      attemptRef.current = null;
      setPrompt(null);
      setError(errorMessage(failure));
      setCanInstallAgent(true);
      setStep("retry");
    }
  }

  function connectDefinition(next = definition) {
    const candidate = onSaveNewHost && configuredRef.current
      ? configuredSshTarget(address)
      : appLocalSshTarget(next);
    if (candidate && onSaveNewHost && configuredRef.current && next.identity_file) {
      candidate.identity_file = next.identity_file;
    }
    if (candidate) void connect(candidate);
  }

  async function saveNewHost(candidate: SshConnectionTarget, remote_info: RemoteIdentity) {
    setSaving(true);
    setError(null);
    try {
      await onSaveNewHost?.(hostName, candidate, remote_info);
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
    const destination = address.trim();
    const alias = hostAlias.trim();
    const identity_file = hostIdentityFile.trim();
    if (!destination) throw new Error("Enter the SSH host or config alias.");
    if (identity_file && /[\x00-\x1f\x7f]/u.test(identity_file)) {
      throw new Error("Enter a valid identity-file path.");
    }
    if (suggestions.includes(destination) ||
      (initialTarget && !initialTarget.hostname && destination === initialTarget.destination)) {
      if (alias && alias !== destination) {
        throw new Error("A saved SSH config host must keep its existing alias.");
      }
      const target = configuredSshTarget(destination);
      if (!target) throw new Error("Enter a valid SSH config host.");
      return {
        ...target,
        ...(initialTarget && !initialTarget.hostname && destination === initialTarget.destination
          ? { user: initialTarget.user, port: initialTarget.port }
          : {}),
        ...(identity_file ? { identity_file } : {}),
      };
    }
    const parsed = parseHostAddress(destination);
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
        mode={promptMode(prompt)}
        onSubmit={answer}
        onCancel={close}
      />
    );

  if (step === "route") {
    return (
      <GatewayRouteDialog
        title={editingConnection ? initialTarget ? "Edit connection method" : "Add connection method" : undefined}
        submitLabel={editingConnection ? "Verify and save" : undefined}
        target={{ kind: "ssh", destination: address.trim() || "New host", gateway_route: gatewayRoute }}
        gateways={draftGateways}
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
        onSave={async (nextGateways, nextRoute) => {
          const candidate = routedHostCandidate();
          draftGatewaysRef.current = nextGateways;
          setDraftGateways(nextGateways);
          setGatewayRoute(nextRoute);
          void connect(resolveSshGateways(
            { ...candidate, gateway_route: nextRoute },
            nextGateways,
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
      title = "Add host · 1/3";
      description =
        "Enter [user@]hostname[:port], or choose an SSH config host." +
        (warning ? `\n${warning}` : "");
      mode = {
        kind: "input",
        label: "SSH host",
        placeholder: "rmux@127.0.0.1:2222",
        initial_value: address,
        suggestions: suggestions.length
          ? {
              label: "SSH config hosts",
              items: suggestions.map((host) => ({
                id: `ssh-config:${host}`,
                label: host,
              })),
              empty_message: "Enter a hostname to add a new host.",
              no_match_message:
                "No matching SSH config hosts. Enter a hostname to add a new host.",
            }
          : undefined,
      };
      break;
    case "name":
      title = "Host name · 2/3";
      mode = {
        kind: "input",
        label: onSaveNewHost ? "Host name" : "Name / SSH alias",
        initial_value: onSaveNewHost ? hostName : definition.alias,
      };
      onBack = back("host");
      break;
    case "auth":
      title = "Authentication · 3/3";
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
      onBack = back("name");
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
                detail: "Reusable by ssh, ctl, and rmux-app.",
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
      description = "The connection is verified. Retry saving this host to rmux.";
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
      description =
        (step === "retry" || step === "update") && canInstallAgent
          ? (needsUpdate
            ? "Update the remote components to add listener discovery and keep this host compatible with the app."
            : "SSH is available, but this host is missing the rmux remote components. Install them for this user or retry after installing them manually.")
          : "OpenSSH will ask for host verification or authentication if needed.";
      mode = {
        kind: "pick",
        choices: [
          ...((step === "retry" || step === "update") && canInstallAgent
            ? [
                {
                  id: "install_agent",
                  label: needsUpdate ? "Update remote components" : "Install remote components",
                  detail: "Install the bundled ctl-agent, rmuxd, and taskd for this user.",
                },
              ]
            : []),
          ...(step === "update" ? [] : [{ id: "retry", label: "Connect" }]),
        ],
      };
      if (!target) onBack = back(complex || editingConnection ? "route" : configuredRef.current && !onSaveNewHost ? "host" : "auth");
      break;
    case "installing":
      title = "Installing remote components";
      mode = remoteInstallProgressMode(install_progress);
      break;
    case "progress":
      title = "Connecting to host";
      description = "Starting the fixed ctl-agent remote command.";
      mode = { kind: "progress" };
  }

  function submit(value: string) {
    setError(null);
    if (step === "host") {
      const selectedAlias = value.startsWith("ssh-config:")
        ? value.slice("ssh-config:".length)
        : onSaveNewHost && suggestions.includes(value.trim()) ? value.trim() : null;
      if (selectedAlias) {
        const candidate = configuredSshTarget(selectedAlias);
        if (candidate) {
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
    } else if (
      (step === "retry" || step === "update" || step === "reconnect") &&
      candidateRef.current
    ) {
      if (value === "install_agent") void installAgent(candidateRef.current);
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
      onCancel={close}
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
