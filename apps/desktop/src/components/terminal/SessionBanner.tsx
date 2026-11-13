import { useSyncExternalStore } from "react";
import { attachmentFeedback } from "../../features/attachment/attachmentFeedback";
import { useCommandEnvironment } from "../../features/commands/CommandContext";
import { COMMAND_IDS } from "../../features/commands/commandIds";
import { targetKey } from "../../features/targets/targets";
import type { AttachmentViewState } from "../../lib/types";
import "./SessionBanner.css";

interface BannerAction {
  label: string;
  on_click(): void;
  disabled?: boolean;
}

export function SessionBanner({ title, message, action, severity = "warning", label = "Session status" }: {
  title: string;
  message?: string | null;
  action?: BannerAction;
  severity?: "info" | "warning" | "error";
  label?: string;
}) {
  return <div className={`session-banner session-banner-${severity}`} role={severity === "error" ? "alert" : "status"} aria-label={label} aria-live="polite" aria-atomic="true">
    <div className="session-banner-content"><strong>{title}</strong>{message && message !== title ? <span>{message}</span> : null}</div>
    {action ? <button type="button" disabled={action.disabled} title={message ?? undefined} onClick={action.on_click}>{action.label}</button> : null}
  </div>;
}

const subscribeNothing = () => () => {};
const emptySnapshot = () => 0;

export function SessionConnectionBanner({ state, on_reconnect, reconnect_available, interaction_enabled, retry_paused }: {
  state: Pick<AttachmentViewState, "session" | "phase" | "error_code" | "message">;
  on_reconnect?(): void;
  reconnect_available: boolean;
  interaction_enabled: boolean;
  retry_paused: boolean;
}) {
  const environment = useCommandEnvironment();
  useSyncExternalStore(environment?.dispatcher.subscribe ?? subscribeNothing, environment?.dispatcher.snapshot ?? emptySnapshot);
  const feedback = attachmentFeedback(state);
  const opening = state.phase === "connecting" || state.phase === "reconnecting";
  const title = state.phase === "connecting" ? "Connecting…"
    : opening || state.phase === "retry_wait" ? "Reconnecting…"
    : retry_paused ? "Retries paused" : "Disconnected";
  let action: BannerAction | undefined;
  if (feedback.action?.kind === "connect_host" || feedback.action?.kind === "recover_components") {
    const target = state.session?.target;
    if (target) {
      const command_id = feedback.action.kind === "connect_host" ? COMMAND_IDS.connectHost
        : target.kind === "ssh" ? COMMAND_IDS.updateComponents : COMMAND_IDS.restartDaemon;
      const args = { target_key: targetKey(target) };
      action = {
        label: feedback.action.label,
        disabled: !interaction_enabled || !environment?.dispatcher.canExecute(command_id, args),
        on_click: () => { environment?.dispatcher.execute(command_id, args); },
      };
    }
  } else if (on_reconnect && reconnect_available && (feedback.action?.kind === "reconnect" ||
      (!state.session || opening) && !["protocol_version_mismatch", "windows_remote_agent_setup_required"].includes(state.error_code ?? ""))) {
    action = { label: "Reconnect", disabled: !interaction_enabled || opening, on_click: on_reconnect };
  }
  return <SessionBanner title={title} message={feedback.message} action={action} label="Session connection" />;
}
