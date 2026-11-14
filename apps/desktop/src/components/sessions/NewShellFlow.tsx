import { type DisplayText } from "../../features/i18n/messages";
import { useLocale } from "../../features/i18n/LocaleContext";
import { useEffect, useRef, useState } from "react";
import { QuickInput } from "../commands/QuickInput";
import { ConnectHostFlow } from "./ConnectHostFlow";
import { hostSelectorChoices } from "./hostChoices";
import {
  LOCAL_TARGET,
  targetKey,
  targetLabel,
} from "../../features/targets/targets";
import { localizedErrorText } from "../../lib/errors";
import type { ConnectionTarget, HostConnectionChange, RemoteIdentity, WorkspaceHost, WorkspaceSshGateway } from "../../lib/types";

interface NewShellFlowProps {
  targets: readonly ConnectionTarget[];
  initial_target_key?: string;
  hosts?: readonly WorkspaceHost[];
  gateways?: readonly WorkspaceSshGateway[];
  discoveryMessage?: string | null;
  onConnectionChange?: HostConnectionChange;
  onVerifyHost(
    target: ConnectionTarget,
    remote_info: RemoteIdentity,
  ): Promise<ConnectionTarget | null>;
  /** Resolve once created, even if subsequent persistence/attachment needs recovery. */
  onCreate(
    target: ConnectionTarget,
    working_directory: string | null,
  ): Promise<void>;
  onClose(): void;
}

/** Collect inputs without contacting any host until the final submission. */
export function NewShellFlow({
  targets,
  initial_target_key,
  hosts,
  gateways,
  discoveryMessage,
  onConnectionChange,
  onVerifyHost,
  onCreate,
  onClose,
}: NewShellFlowProps) {
  const { t, locale, formatText } = useLocale();
  const [selectedTargetKey, setSelectedTargetKey] = useState<string | null>(
    initial_target_key ?? null,
  );
  const [workingDirectory, setWorkingDirectory] = useState("");
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<DisplayText | null>(null);
  const [connecting, setConnecting] = useState<ConnectionTarget | null>(null);
  const pendingDirectoryRef = useRef<{ working_directory: string | null } | null>(null);
  const busyRef = useRef(false);
  const closedRef = useRef(false);
  useEffect(() => {
    closedRef.current = false;
    return () => {
      closedRef.current = true;
      pendingDirectoryRef.current = null;
    };
  }, []);

  // Local is always first, regardless of the active tab or stored host order.
  const choices = [
    targets.find((target) => target.kind === "local") ?? LOCAL_TARGET,
    ...targets.filter((target) => target.kind === "ssh"),
  ];
  const target = choices.find(
    (candidate) => targetKey(candidate) === selectedTargetKey,
  );

  function close() {
    if (busyRef.current || closedRef.current) return;
    closedRef.current = true;
    onClose();
  }

  function submit(value: string) {
    if (!target || busyRef.current || closedRef.current) return;
    busyRef.current = true;
    setWorkingDirectory(value);
    setError(null);
    const working_directory = value.trim() || null;
    if (target.kind === "ssh") {
      pendingDirectoryRef.current = { working_directory };
      setConnecting(target);
    } else {
      void create(target, working_directory);
    }
  }

  async function create(verified: ConnectionTarget, working_directory: string | null) {
    setCreating(true);
    try {
      await onCreate(verified, working_directory);
      if (!closedRef.current) {
        closedRef.current = true;
        onClose();
      }
    } catch (failure) {
      if (!closedRef.current) setError(localizedErrorText(failure));
    } finally {
      busyRef.current = false;
      if (!closedRef.current) setCreating(false);
    }
  }

  if (connecting) {
    return (
      <ConnectHostFlow
        suggestions={[]}
        warning={null}
        target={connecting}
        host={connecting.kind === "ssh" ? hosts?.find((host) => host.host_id === connecting.host_id) : undefined}
        gateways={gateways}
        hosts={hosts}
        onConnectionChange={onConnectionChange}
        onVerified={onVerifyHost}
        onConnected={(verified) => {
          const pending = pendingDirectoryRef.current;
          if (!pending || closedRef.current) return;
          // Consume the request before SshHostFlow's success-close callback.
          pendingDirectoryRef.current = null;
          setConnecting(null);
          void create(verified, pending.working_directory);
        }}
        onClose={() => {
          if (!pendingDirectoryRef.current || closedRef.current) return;
          pendingDirectoryRef.current = null;
          busyRef.current = false;
          setConnecting(null);
        }}
      />
    );
  }

  if (creating) {
    return (
      <QuickInput
        key="creating"
        title={t("new_shell.in_progress")}
        description={t("new_shell.in_progress_hint")}
        mode={{
          kind: "progress",
          message: t("new_shell.creating", { host: target ? targetLabel(target, locale) : t("new_shell.selected_host") }),
        }}
        cancel_disabled
        onSubmit={() => {}}
        onCancel={close}
      />
    );
  }

  if (!target) {
    return (
      <QuickInput
        key="host"
        title={t("new_shell.pick_host")}
        description={`${t("new_shell.pick_host_hint")}${discoveryMessage ? `\n${discoveryMessage}` : ""}`}
        error={
          selectedTargetKey
            ? t("new_shell.host_unavailable")
            : null
        }
        mode={{
          kind: "pick",
          choices: hostSelectorChoices(choices, hosts, t("new_shell.local"), locale),
        }}
        onSubmit={(key) => {
          if (closedRef.current) return;
          setError(null);
          setSelectedTargetKey(key);
        }}
        onCancel={close}
      />
    );
  }

  return (
    <QuickInput
      key={`directory:${targetKey(target)}`}
      title={t("new_shell.directory")}
      description={t("new_shell.directory_hint", { host: targetLabel(target, locale), ssh: String(target.kind === "ssh") })}
      error={error ? formatText(error) : null}
      mode={{
        kind: "input",
        label: t("new_shell.working_directory"),
        initial_value: workingDirectory,
        placeholder: t("new_shell.home_directory"),
        submit_label: t("new_shell.create"),
      }}
      onChange={setWorkingDirectory}
      onSubmit={submit}
      onBack={() => {
        if (busyRef.current) return;
        setSelectedTargetKey(null);
        setError(null);
      }}
      onCancel={close}
    />
  );
}
