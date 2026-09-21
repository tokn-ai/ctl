import { useEffect, useRef, useState } from "react";
import { QuickInput } from "../commands/QuickInput";
import { SshHostFlow } from "./SshHostFlow";
import { hostSelectorChoices } from "./hostChoices";
import {
  sessionKey,
  targetKey,
  targetLabel,
} from "../../features/targets/targets";
import { listSessions } from "../../lib/tauri";
import { errorMessage } from "../../lib/errors";
import type {
  ConnectionTarget,
  RemoteIdentity,
  HostConnectionChange,
  SessionListResponse,
  SessionSummary,
  ShellStateSummary,
  WorkspaceHost,
} from "../../lib/types";

interface AddExistingSessionFlowProps {
  targets: readonly ConnectionTarget[];
  hosts?: readonly WorkspaceHost[];
  onConnectionChange?: HostConnectionChange;
  known: readonly SessionSummary[];
  onVerifyHost(
    target: ConnectionTarget,
    remote_info: RemoteIdentity,
  ): Promise<ConnectionTarget | null>;
  onAdd(
    session: SessionSummary,
    shell_state: ShellStateSummary | null,
  ): Promise<void>;
  onClose(): void;
}

/** Enumeration is confined to this explicit, single-host import flow. */
export function AddExistingSessionFlow(props: AddExistingSessionFlowProps) {
  const [target, setTarget] = useState<ConnectionTarget | null>(null);
  const [connectedTarget, setConnectedTarget] = useState<ConnectionTarget | null>(null);
  const connectedRef = useRef(false);
  function chooseHost() {
    connectedRef.current = false;
    setConnectedTarget(null);
    setTarget(null);
  }
  if (target?.kind === "ssh" && !connectedTarget) {
    return (
      <SshHostFlow
        suggestions={[]}
        warning={null}
        target={target}
        autoConnect
        onConnectionChange={props.onConnectionChange}
        onVerified={props.onVerifyHost}
        onConnected={(verified) => {
          connectedRef.current = true;
          setConnectedTarget(verified);
        }}
        onClose={() => {
          // Successful connections close the SSH flow before discovery starts.
          if (!connectedRef.current) chooseHost();
        }}
      />
    );
  }
  if (target) {
    return (
      <SessionChoices
        key={targetKey(target)}
        {...props}
        target={connectedTarget ?? target}
        onBack={chooseHost}
        onReconnect={() => {
          connectedRef.current = false;
          setTarget(connectedTarget ?? target);
          setConnectedTarget(null);
        }}
      />
    );
  }
  return (
    <QuickInput
      title="Add existing session — host"
      description="Choose one host to discover its running sessions. Other hosts will not be contacted."
      mode={{
        kind: "pick",
        choices: hostSelectorChoices(props.targets, props.hosts),
      }}
      onSubmit={(key) =>
        setTarget(
          props.targets.find((candidate) => targetKey(candidate) === key) ??
            null,
        )
      }
      onCancel={props.onClose}
    />
  );
}

function SessionChoices({
  target,
  known,
  onAdd,
  onClose,
  onBack,
  onReconnect,
}: AddExistingSessionFlowProps & {
  target: ConnectionTarget;
  onBack(): void;
  onReconnect(): void;
}) {
  const [catalog, setCatalog] = useState<SessionListResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [added, setAdded] = useState(0);
  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    void listSessions(target).then(
      (result) => {
        if (!cancelled) {
          setCatalog(result);
          setLoading(false);
        }
      },
      (failure: unknown) => {
        if (!cancelled) {
          setError(errorMessage(failure));
          setLoading(false);
        }
      },
    );
    return () => {
      cancelled = true;
    };
  }, [target, attempt]);

  const knownKeys = new Set(known.map(sessionKey));
  const available =
    catalog?.sessions.filter(
      (session) => !knownKeys.has(sessionKey(session)),
    ) ?? [];
  async function submit(id: string) {
    if (saving) return;
    if (id === "done") {
      onClose();
      return;
    }
    if (id === "retry") {
      if (target.kind === "ssh") onReconnect();
      else setAttempt((current) => current + 1);
      return;
    }
    const session = available.find((candidate) => sessionKey(candidate) === id);
    if (!session) return;
    setSaving(true);
    setError(null);
    try {
      await onAdd(session, catalog?.shell_states[session.session_id] ?? null);
      setAdded((current) => current + 1);
    } catch (failure) {
      setError(
        `Could not save the entry: ${errorMessage(failure)}. Close this picker and use Retry saving.`,
      );
    } finally {
      setSaving(false);
    }
  }

  return (
    <QuickInput
      key={`${attempt}:${loading}:${saving}:${added}`}
      title={`Add existing session — ${targetLabel(target)}`}
      description={
        loading
          ? undefined
          : catalog
            ? `${added ? `${added} added. ` : ""}${available.length ? "Choose sessions to remember without attaching. Select Done when finished." : "No additional running sessions. Existing workspace entries are hidden."}`
            : "Discovery failed. Retry to reconnect to this host and discover its sessions."
      }
      error={error}
      mode={
        loading || saving
          ? {
              kind: "progress",
              message: saving
                ? "Saving workspace…"
                : "Discovering sessions on this host…",
            }
          : {
              kind: "pick",
              choices: [
                ...available.map((session) => ({
                  id: sessionKey(session),
                  label: session.name,
                  detail:
                    catalog?.shell_states[session.session_id]?.cwd_display ??
                    catalog?.shell_states[session.session_id]?.cwd ??
                    session.session_id,
                })),
                ...(!catalog
                  ? [{ id: "retry", label: "Retry discovery" }]
                  : []),
                { id: "done", label: "Done" },
              ],
            }
      }
      onSubmit={submit}
      onBack={saving ? undefined : onBack}
      onCancel={() => {
        if (!saving) onClose();
      }}
    />
  );
}
