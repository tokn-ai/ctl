import type { AttachmentViewState, OpenAttachmentResponse, SessionSummary } from "../../lib/types";
import { canAutomaticallyRecoverAttachment } from "./attachmentRecovery";

export type ConnectionIntent = "attach" | "reconnect";

export type AttachmentTransition =
  | { type: "begin"; intent: ConnectionIntent; session: SessionSummary; resume_from: string | null; resize_with_window: boolean; resize_control_desired?: boolean }
  | { type: "attached"; response: OpenAttachmentResponse; resize_with_window: boolean; layout_request_pending?: boolean }
  | { type: "failed"; code: string | null; message: string; resume_from: string | null }
  | { type: "closed"; reason: "connection_closed" | "detached" | "session_ended"; next_sequence: string | null }
  | { type: "ended"; exit_code: number | null }
  | { type: "retry_scheduled"; retry_at_ms: number }
  | { type: "retry_exhausted" }
  | { type: "reset" };

const EMPTY_LEASE = { held: false, owned_by_client: false };
const RESIZE_CONTROL_HELD_MESSAGE = "Another attachment holds resize control.";
const RESIZE_CONTROL_AVAILABLE_MESSAGE = "Resize control is available. Take resize control to resize panes.";

export function resizeControlNotice(held: boolean): string {
  return held ? RESIZE_CONTROL_HELD_MESSAGE : RESIZE_CONTROL_AVAILABLE_MESSAGE;
}

/** Identify generated ownership notices without concealing other action errors. */
export function isResizeControlNotice(message: string | null): boolean {
  return message === RESIZE_CONTROL_HELD_MESSAGE || message === RESIZE_CONTROL_AVAILABLE_MESSAGE;
}

export function initialAttachmentState(): AttachmentViewState {
  return {
    phase: "idle", retry_at_ms: null, error_code: null, attachment_id: null, session: null,
    input_lease: EMPTY_LEASE, layout_lease: EMPTY_LEASE, shell_state: null,
    applied_sequence: null, reconnect_sequence: null, history_gap: false,
    terminal_size_mismatch: false, resize_with_window: false, resize_control_desired: false, message: null,
  };
}

/**
 * Lifecycle authority. Only an open response proves attachment; only an ended
 * event proves process exit. Cursors select replay data, never connection intent.
 * Non-attached states retain observations but cannot claim live input leases.
 * Callers fence async events with the owning generation before transitioning.
 */
export function transitionAttachment(state: AttachmentViewState, event: AttachmentTransition): AttachmentViewState {
  const offline = { ...state, attachment_id: null, input_lease: EMPTY_LEASE, layout_lease: EMPTY_LEASE, retry_at_ms: null };
  switch (event.type) {
    case "reset": return initialAttachmentState();
    case "begin": return {
      ...initialAttachmentState(),
      phase: event.intent === "reconnect" ? "reconnecting" : "connecting",
      session: event.session,
      applied_sequence: event.resume_from,
      reconnect_sequence: event.resume_from,
      resize_with_window: event.resize_with_window,
      resize_control_desired: event.resize_control_desired ?? event.resize_with_window,
    };
    case "attached": {
      if (state.phase !== "connecting" && state.phase !== "reconnecting") return state;
      const response = event.response;
      const resizing = event.resize_with_window && (response.layout_lease.owned_by_client || event.layout_request_pending === true);
      return {
        ...state, phase: "attached", retry_at_ms: null, error_code: null,
        attachment_id: response.attachment_id, session: response.session,
        input_lease: response.input_lease, layout_lease: response.layout_lease,
        terminal_size_mismatch: response.terminal_size_mismatch,
        history_gap: state.history_gap || response.history_gap,
        reconnect_sequence: null, resize_with_window: resizing,
        message: state.resize_control_desired && !response.layout_lease.owned_by_client && !event.layout_request_pending
          ? resizeControlNotice(response.layout_lease.held)
          : null,
      };
    }
    case "ended":
      if (state.phase !== "attached") return state;
      return {
      ...offline, phase: "ended", error_code: null, resize_with_window: false, resize_control_desired: false,
      message: event.exit_code === null ? "Session ended." : `Session ended with exit code ${event.exit_code}.`,
    };
    case "failed":
      if (state.phase === "ended" || state.phase === "idle") return state;
      return { ...offline, phase: "error", error_code: event.code, message: event.message, reconnect_sequence: event.resume_from };
    case "closed":
      if (state.phase === "ended") return offline;
      if (state.phase !== "attached") return state;
      return {
        ...offline,
        phase: event.reason === "session_ended" ? "ended" : event.reason === "detached" ? "idle" : "disconnected",
        error_code: null, reconnect_sequence: event.next_sequence,
        resize_with_window: event.reason === "connection_closed" && state.resize_with_window,
        resize_control_desired: event.reason === "connection_closed" && state.resize_control_desired,
        message: event.reason === "connection_closed" ? "Connection interrupted." : event.reason === "session_ended" ? "Session ended." : null,
      };
    case "retry_scheduled":
      if (!state.session || !canAutomaticallyRecoverAttachment(state.error_code) ||
        (state.phase !== "disconnected" && state.phase !== "error")) return state;
      return { ...offline, phase: "retry_wait", retry_at_ms: event.retry_at_ms };
    case "retry_exhausted":
      if (!state.session || !["disconnected", "error", "retry_wait"].includes(state.phase)) return state;
      return {
        ...offline, phase: "error", error_code: "automatic_reconnect_timeout",
        message: "Automatic retries stopped. The remote session's current state is unknown. Retry to reconnect.",
      };
  }
}

export function attachmentPhaseLabel(phase: AttachmentViewState["phase"]): string {
  switch (phase) {
    case "idle": return "Not attached";
    case "connecting": return "Connecting…";
    case "reconnecting": return "Reconnecting…";
    case "retry_wait": return "Waiting to retry…";
    case "attached": return "Attached";
    case "disconnected": return "Disconnected";
    case "ended": return "Session ended";
    case "error": return "Connection failed";
  }
}
