import type { AttachmentViewState } from "../../lib/types";

export interface AttachmentFeedback {
  message: string | null;
  action: {
    kind: "reconnect" | "connect_host" | "recover_components";
    label: string;
  } | null;
}

/** Share recovery capabilities between the session banner and its notification. */
export function attachmentFeedback(
  state: Pick<AttachmentViewState, "session" | "phase" | "error_code" | "message">,
  windows_client = /^Win/i.test(globalThis.navigator?.platform ?? ""),
): AttachmentFeedback {
  const { session, phase, error_code, message } = state;
  if (!session || !["error", "disconnected", "retry_wait"].includes(phase))
    return { message, action: null };
  if (error_code === "protocol_version_mismatch") {
    if (phase !== "error") return { message, action: null };
    if (session.target.kind === "ssh" && (windows_client || session.target.remote_platform === "windows")) {
      const guidance = session.target.remote_platform === "windows"
        ? "Update the components on the Windows host, then reconnect."
        : "Update the remote components on the host, then reconnect.";
      return {
        message: message ? `${message} ${guidance}` : null,
        action: { kind: "connect_host", label: "Connect host" },
      };
    }
    return {
      message,
      action: { kind: "recover_components", label: session.target.kind === "ssh" ? "Update remote components" : "Restart local daemon" },
    };
  }
  if (error_code === "windows_remote_agent_setup_required") {
    return {
      message,
      action: phase === "error" && session.target.kind === "ssh" ? { kind: "connect_host", label: "Connect host" } : null,
    };
  }
  return { message, action: { kind: "reconnect", label: "Reconnect" } };
}
