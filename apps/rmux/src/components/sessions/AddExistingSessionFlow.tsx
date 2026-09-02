import { useEffect, useState } from "react";
import { QuickInput } from "../commands/QuickInput";
import {
  sessionKey,
  targetKey,
  targetLabel,
} from "../../features/targets/targets";
import { listSessions } from "../../lib/tauri";
import { errorMessage } from "../../lib/errors";
import type {
  ConnectionTarget,
  SessionListResponse,
  SessionSummary,
  ShellStateSummary,
} from "../../lib/types";

interface AddExistingSessionFlowProps {
  targets: readonly ConnectionTarget[];
  known: readonly SessionSummary[];
  onAdd(
    session: SessionSummary,
    shell_state: ShellStateSummary | null,
  ): Promise<void>;
  onClose(): void;
}

/** Enumeration is confined to this explicit, single-host import flow. */
export function AddExistingSessionFlow(props: AddExistingSessionFlowProps) {
  const [target, setTarget] = useState<ConnectionTarget | null>(null);
  if (target) {
    return (
      <SessionChoices
        key={targetKey(target)}
        {...props}
        target={target}
        onBack={() => setTarget(null)}
      />
    );
  }
  return (
    <QuickInput
      title="Add existing session — host"
      description="Choose one host to discover its running sessions. Other hosts will not be contacted."
      mode={{
        kind: "pick",
        choices: props.targets.map((candidate) => ({
          id: targetKey(candidate),
          label: targetLabel(candidate),
        })),
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
}: AddExistingSessionFlowProps & {
  target: ConnectionTarget;
  onBack(): void;
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
      setAttempt((current) => current + 1);
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
            : "Discovery failed. If authentication is required, close this picker and use Connect host first."
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
      onSubmit={(id) => void submit(id)}
      onBack={saving ? undefined : onBack}
      onCancel={() => {
        if (!saving) onClose();
      }}
    />
  );
}
