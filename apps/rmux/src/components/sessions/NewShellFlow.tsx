import { useEffect, useRef, useState } from "react";
import { QuickInput } from "../commands/QuickInput";
import { SshHostFlow } from "./SshHostFlow";
import { hostSelectorChoices } from "./hostChoices";
import {
  LOCAL_TARGET,
  targetKey,
  targetLabel,
} from "../../features/targets/targets";
import { errorMessage } from "../../lib/errors";
import type { ConnectionTarget, RemoteIdentity, WorkspaceHost } from "../../lib/types";

interface NewShellFlowProps {
  targets: readonly ConnectionTarget[];
  hosts?: readonly WorkspaceHost[];
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
  hosts,
  onVerifyHost,
  onCreate,
  onClose,
}: NewShellFlowProps) {
  const [selectedTargetKey, setSelectedTargetKey] = useState<string | null>(
    null,
  );
  const [workingDirectory, setWorkingDirectory] = useState("");
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);
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
      if (!closedRef.current) setError(errorMessage(failure));
    } finally {
      busyRef.current = false;
      if (!closedRef.current) setCreating(false);
    }
  }

  if (connecting) {
    return (
      <SshHostFlow
        suggestions={[]}
        warning={null}
        target={connecting}
        autoConnect
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
        title="New shell — creating"
        description="Creation has started and cannot be cancelled. Please wait for the result before trying again."
        mode={{
          kind: "progress",
          message: `Creating and opening a shell on ${target ? targetLabel(target) : "the selected host"}…`,
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
        title="New shell — host · 1/2"
        description="Choose where to create the shell. Local is the default; hosts are not contacted until you create."
        error={
          selectedTargetKey
            ? "That host is no longer available. Choose another host."
            : null
        }
        mode={{
          kind: "pick",
          choices: hostSelectorChoices(choices, hosts, "Local"),
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
      title="New shell — working directory · 2/2"
      description={`Create on ${targetLabel(target)}. Leave blank for its home directory.${target.kind === "ssh" ? " You’ll be prompted if SSH authentication is needed." : ""}`}
      error={error}
      mode={{
        kind: "input",
        label: "Working directory",
        initial_value: workingDirectory,
        placeholder: "home directory",
        submit_label: "Create shell",
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
