export type Sequence = string;

export interface TerminalSize {
  columns: number;
  rows: number;
  pixel_width: number | null;
  pixel_height: number | null;
}

export interface LeaseStatus {
  held: boolean;
  owned_by_client: boolean;
}

export type LeaseKind = "input" | "layout";
export type SessionStatus = "running" | "exited";

export interface SessionSummary {
  session_id: string;
  name: string;
  status: SessionStatus;
  terminal_size: TerminalSize;
  next_sequence: Sequence;
}

export type ShellType =
  | "bash"
  | "zsh"
  | "fish"
  | "pwsh"
  | "cmd"
  | "sh"
  | "unknown";

export type PromptPhase = "unknown" | "at_prompt" | "editing" | "running";
export type TuiHint = "unknown" | "inline" | "alternate_screen";

export interface ShellStateSummary {
  shell_type: ShellType;
  cwd: string | null;
  running_command: string | null;
  prompt_phase: PromptPhase;
  tui_hint: TuiHint;
  revision: Sequence;
  observed_sequence: Sequence;
}

export interface CreateSessionRequest {
  working_directory: string | null;
  terminal_size: TerminalSize;
}

export interface KillSessionRequest {
  session_id: string;
}

export interface OpenAttachmentRequest {
  session: string;
  resume_from: Sequence | null;
  terminal_size: TerminalSize;
  request_input_lease: boolean;
  request_layout_lease: boolean;
}

export interface OpenAttachmentResponse {
  attachment_id: string;
  session: SessionSummary;
  replay_from: Sequence;
  history_gap: boolean;
  terminal_size_mismatch: boolean;
  input_lease: LeaseStatus;
  layout_lease: LeaseStatus;
  shell_state: ShellStateSummary;
}

export interface AttachmentIdRequest {
  attachment_id: string;
}

export interface AttachmentInputRequest extends AttachmentIdRequest {
  data_base64: string;
}

export interface AttachmentResizeRequest extends AttachmentIdRequest {
  terminal_size: TerminalSize;
}

export interface AttachmentLeaseRequest extends AttachmentIdRequest {
  lease: LeaseKind;
}

export interface AttachmentAckRequest extends AttachmentIdRequest {
  event_id: string;
}

export interface TerminalCheckpoint {
  format: string;
  format_version: number;
  sequence: Sequence;
  terminal_size: TerminalSize;
  payload_base64: string;
  input_prefix_base64: string;
}

interface AttachmentEventBase {
  attachment_id: string;
}

interface PresentationEventBase extends AttachmentEventBase {
  event_id: string;
}

export interface CheckpointEvent extends PresentationEventBase {
  event_type: "checkpoint";
  checkpoint: TerminalCheckpoint;
  history_gap: boolean;
}

export interface OutputEvent extends PresentationEventBase {
  event_type: "output";
  sequence_start: Sequence;
  sequence_end: Sequence;
  data_base64: string;
}

export interface PtyGeometryChangedEvent extends PresentationEventBase {
  event_type: "pty_geometry_changed";
  terminal_size: TerminalSize;
  observed_sequence: Sequence;
}

export interface LeaseStatusEvent extends AttachmentEventBase {
  event_type: "lease_status";
  lease: LeaseKind;
  status: LeaseStatus;
}

export interface ShellStateChangedEvent extends AttachmentEventBase {
  event_type: "shell_state_changed";
  shell_state: ShellStateSummary;
}

export interface ServerErrorEvent extends AttachmentEventBase {
  event_type: "server_error";
  code: string;
  message: string;
}

export interface SessionEndedEvent extends AttachmentEventBase {
  event_type: "session_ended";
  session_id: string;
  exit_code: number | null;
}

export type AttachmentExitReason =
  | "detached"
  | "connection_closed"
  | "session_ended";

export interface AttachmentExitedEvent extends AttachmentEventBase {
  event_type: "attachment_exited";
  reason: AttachmentExitReason;
  exit_code: number | null;
  next_sequence: Sequence | null;
  received_sequence: Sequence;
}

export interface AttachmentErrorEvent extends AttachmentEventBase {
  event_type: "attachment_error";
  code: string;
  message: string;
}

export type AttachmentEvent =
  | CheckpointEvent
  | OutputEvent
  | PtyGeometryChangedEvent
  | LeaseStatusEvent
  | ShellStateChangedEvent
  | ServerErrorEvent
  | SessionEndedEvent
  | AttachmentExitedEvent
  | AttachmentErrorEvent;

export type ConnectionPhase =
  | "idle"
  | "connecting"
  | "attached"
  | "reconnecting"
  | "disconnected"
  | "ended"
  | "error";

export interface AttachmentViewState {
  phase: ConnectionPhase;
  attachment_id: string | null;
  session: SessionSummary | null;
  input_lease: LeaseStatus;
  layout_lease: LeaseStatus;
  shell_state: ShellStateSummary | null;
  applied_sequence: Sequence | null;
  reconnect_sequence: Sequence | null;
  history_gap: boolean;
  terminal_size_mismatch: boolean;
  resize_with_window: boolean;
  message: string | null;
}
