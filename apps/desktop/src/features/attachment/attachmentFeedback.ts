import type { AttachmentViewState } from "../../lib/types";
import { message, type DisplayText, type LocalizedMessage } from "../i18n/messages";

export interface AttachmentFeedback {
  message: DisplayText | null;
  action: {
    kind: "reconnect" | "connect_host" | "recover_components";
    label: LocalizedMessage;
  } | null;
}

/** Share recovery capabilities between the session banner and its notification. */
export function attachmentFeedback(
  state: Pick<AttachmentViewState, "session" | "phase" | "error_code" | "message">,
  windows_client = /^Win/i.test(globalThis.navigator?.platform ?? ""),
): AttachmentFeedback {
  const { session, phase, error_code, message: detail } = state;
  if (!session || !["error", "disconnected", "retry_wait"].includes(phase))
    return { message: detail, action: null };
  if (error_code === "protocol_version_mismatch") {
    if (phase !== "error") return { message: detail, action: null };
    if (session.target.kind === "ssh" && (windows_client || session.target.remote_platform === "windows")) {
      const guidance = session.target.remote_platform === "windows" ? "attachment.windows_guidance" : "attachment.remote_guidance";
      return {
        message: detail ? message(guidance, { detail }) : null,
        action: { kind: "connect_host", label: message("attachment.connect_host") },
      };
    }
    return {
      message: detail,
      action: { kind: "recover_components", label: message(session.target.kind === "ssh" ? "attachment.update_remote" : "attachment.restart_local") },
    };
  }
  if (error_code === "windows_remote_agent_setup_required") {
    return {
      message: detail,
      action: phase === "error" && session.target.kind === "ssh" ? { kind: "connect_host", label: message("attachment.connect_host") } : null,
    };
  }
  return { message: detail, action: { kind: "reconnect", label: message("session.reconnect") } };
}
