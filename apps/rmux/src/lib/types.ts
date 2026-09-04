export type Sequence = string;

export interface CommandKeybinding {
  code: string;
  primary: boolean;
  shift?: boolean;
  alt?: boolean;
}

export interface KeybindingOverride {
  command_id: string;
  /** null explicitly removes a default binding. */
  keybinding: CommandKeybinding | null;
}

export interface KeybindingsDocument {
  schema_version: 1;
  overrides: KeybindingOverride[];
}

export interface KeybindingsSnapshot {
  path: string;
  /** Exact source text for compare-and-swap with external editor changes. */
  revision: string | null;
  document: KeybindingsDocument;
}

export interface NativeCommandBinding {
  command_id: string;
  title: string;
  keybinding: CommandKeybinding | null;
  enabled: boolean;
}

export interface SshConnectionTarget {
  kind: "ssh";
  /** App-owned identity; stripped at the native transport boundary. */
  host_id?: string;
  destination: string;
  hostname?: string;
  user?: string;
  port?: number;
  identity_file?: string;
}

export type ConnectionTarget = { kind: "local" } | SshConnectionTarget;

export interface WorkspaceHost {
  host_id: string;
  target: ConnectionTarget;
}

export interface SessionReference {
  host_id: string;
  session_id: string;
}

/** Remembered presentation metadata, never authoritative runtime state. */
export interface WorkspaceSession extends SessionReference {
  name: string;
  last_known_cwd: string | null;
  last_known_cwd_display: string | null;
}

export interface WorkspaceDocument {
  schema_version: 1 | 2;
  workspace_id: string;
  hosts: WorkspaceHost[];
  sessions: WorkspaceSession[];
  tabs: WorkspaceTab[];
  active_tab: WorkspaceTab | null;
  task_definitions?: SavedTaskDefinition[];
  task_drafts?: TaskDefinitionDraft[];
  sidebar_view?: "sessions" | "tasks";
  task_references?: TaskReference[];
}

export interface WorkspaceSnapshot {
  revision: string | null;
  document: WorkspaceDocument;
}

export interface SshConfigHost {
  destination: string;
}

export interface SshConfigHostCatalog {
  hosts: SshConfigHost[];
  warnings: string[];
}

export interface SshIdentityFile {
  path: string;
  display_path: string;
}

export interface SshIdentityFileCatalog {
  identity_files: SshIdentityFile[];
  warnings: string[];
}

export interface SshHostDefinition {
  alias: string;
  hostname: string;
  user: string | null;
  port: number | null;
  identity_file: string | null;
}

export interface SaveSshConfigHostResponse {
  destination: string;
}

export type SshHostStorage = "ssh_config" | "local_storage";

export interface SshPrompt {
  prompt_id: string;
  kind: "confirm" | "secret";
  message: string;
}

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
export type SessionStatus =
  | "running"
  | "exited"
  | "unknown"
  | "unreachable"
  | "missing";

export interface SessionSummary {
  target: ConnectionTarget;
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
  cwd_display?: string | null;
  running_command: string | null;
  prompt_phase: PromptPhase;
  tui_hint: TuiHint;
  revision: Sequence;
  observed_sequence: Sequence;
}

/**
 * A session list plus best-effort non-attaching shell snapshots. Entries can
 * be absent when a session exits during refresh or cannot be inspected.
 */
export interface SessionListResponse {
  sessions: SessionSummary[];
  shell_states: Record<string, ShellStateSummary>;
}

export interface SessionInspection {
  session_id: string;
  session: SessionSummary | null;
  shell_state: ShellStateSummary | null;
  error: { code: string; message: string } | null;
}

export interface CreateSessionRequest {
  target: ConnectionTarget;
  working_directory: string | null;
  terminal_size: TerminalSize;
}

export interface KillSessionRequest {
  target: ConnectionTarget;
  session_id: string;
}

export interface RestartLocalDaemonResponse {
  terminated_sessions: number;
}

export interface OpenAttachmentRequest {
  target: ConnectionTarget;
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

export interface TerminalHistorySnapshot {
  format: string;
  format_version: number;
  sequence: Sequence;
  generation: string;
  revision: string;
  retained_bytes: string;
  truncated: boolean;
  lines: string[];
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
  history: TerminalHistorySnapshot;
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
  error_code: string | null;
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


export interface TaskDefinition {
  name: string;
  program: string;
  arguments: string[];
  working_directory: string | null;
  execution_mode: "background" | "interactive";
}
export interface TaskRun {
  run_id: string;
  state: "starting" | "unknown" | "running" | "completed" | "failed" | "stopped";
  started_at_ms: number;
  ended_at_ms: number | null;
  exit_code: number | null;
  definition?: TaskDefinition;
  interactive?: { session_id: string | null; instance_id: string; rmux_socket: string; released: boolean };
}
export interface ManagedTask {
  task_id: string;
  definition: TaskDefinition;
  desired_state: "running" | "stopped";
  active_run: TaskRun | null;
  last_run: TaskRun | null;
}
export interface TaskDefinitionDraft {
  definition_id: string;
  definition: TaskDefinition;
}
export interface SavedTaskDefinition {
  definition_id: string;
  revision: string;
  definition: TaskDefinition;
}
export interface TaskReference {
  host_id: string;
  task_id: string;
  definition_id: string | null;
  applied_revision: string | null;
  is_default: boolean;
}
export type WorkspaceTab =
  | (SessionReference & { kind?: "session" })
  | { kind: "task"; host_id: string; task_id: string }
  | { kind: "task_definition"; definition_id: string };
export type TaskTab = Extract<WorkspaceTab, { kind: "task" }>;
export type TaskRequest =
  | { type: "list_tasks" }
  | { type: "show_task" | "start_task" | "stop_task" | "restart_task" | "remove_task"; task: string }
  | { type: "register_task"; task_id: string; definition: TaskDefinition }
  | { type: "update_task"; task: string; definition: TaskDefinition };
export type TaskResponse =
  | { type: "task_list"; tasks: ManagedTask[] }
  | { type: "task_created" | "task_status"; task: ManagedTask }
  | { type: "task_removed"; task_id: string };
export type TaskLogEvent =
  | { event_type: "log"; subscription_id: string; run_id: string; sequence: string; stream: "stdout" | "stderr"; data: number[] }
  | { event_type: "finished" }
  | { event_type: "error"; message: string };
