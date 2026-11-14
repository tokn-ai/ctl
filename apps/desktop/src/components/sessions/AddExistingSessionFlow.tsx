import { message, type DisplayText } from "../../features/i18n/messages";
import { useLocale } from "../../features/i18n/LocaleContext";
import { useEffect, useRef, useState } from "react";
import { QuickInput } from "../commands/QuickInput";
import { ConnectHostFlow } from "./ConnectHostFlow";
import { hostSelectorChoices } from "./hostChoices";
import {
  sessionKey,
  targetKey,
  targetLabel,
} from "../../features/targets/targets";
import { listSessions } from "../../lib/tauri";
import { localizedErrorText } from "../../lib/errors";
import type {
  ConnectionTarget,
  RemoteIdentity,
  HostConnectionChange,
  SessionListResponse,
  SessionSummary,
  ShellStateSummary,
  WorkspaceHost,
  WorkspaceSshGateway,
} from "../../lib/types";

interface AddExistingSessionFlowProps {
  targets: readonly ConnectionTarget[];
  hosts?: readonly WorkspaceHost[];
  gateways?: readonly WorkspaceSshGateway[];
  discoveryMessage?: string | null;
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
  const { t, locale } = useLocale();
  const [target, setTarget] = useState<ConnectionTarget | null>(null);
  const [connectedTarget, setConnectedTarget] = useState<ConnectionTarget | null>(null);
  const [retryMethodId, setRetryMethodId] = useState<string | undefined>();
  const connectedRef = useRef(false);
  function chooseHost() {
    connectedRef.current = false;
    setConnectedTarget(null);
    setTarget(null);
    setRetryMethodId(undefined);
  }
  if (target?.kind === "ssh" && !connectedTarget) {
    return (
      <ConnectHostFlow
        suggestions={[]}
        warning={null}
        target={target}
        host={props.hosts?.find((host) => host.host_id === target.host_id)}
        gateways={props.gateways}
        hosts={props.hosts}
        selected_method_id={retryMethodId}
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
          const retryTarget = connectedTarget ?? target;
          setTarget(retryTarget);
          setRetryMethodId(retryTarget.kind === "ssh" ? retryTarget.method_id : undefined);
          setConnectedTarget(null);
        }}
      />
    );
  }
  return (
    <QuickInput
      title={t("session_import.host")}
      description={`${t("session_import.host_hint")}${props.discoveryMessage ? `\n${props.discoveryMessage}` : ""}`}
      mode={{
        kind: "pick",
        choices: hostSelectorChoices(props.targets, props.hosts, undefined, locale),
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
  const { t, locale, formatText } = useLocale();
  const [catalog, setCatalog] = useState<SessionListResponse | null>(null);
  const [error, setError] = useState<DisplayText | null>(null);
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
          setError(localizedErrorText(failure));
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
        message("session_import.save_failed", { detail: localizedErrorText(failure) }),
      );
    } finally {
      setSaving(false);
    }
  }

  return (
    <QuickInput
      key={`${attempt}:${loading}:${saving}:${added}`}
      title={t("session_import.title", { host: targetLabel(target, locale) })}
      description={
        loading
          ? undefined
          : catalog
            ? t(available.length ? "session_import.available" : "session_import.no_more", { count: added })
            : t("session_import.failed")
      }
      error={error ? formatText(error) : null}
      mode={
        loading || saving
          ? {
              kind: "progress",
              message: saving
                ? t("session_import.saving")
                : t("session_import.discovering"),
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
                  ? [{ id: "retry", label: t("session_import.retry") }]
                  : []),
                { id: "done", label: t("session_import.done") },
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
