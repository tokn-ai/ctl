export type Sequence = string;

export interface CredentialTarget {
  name: string;
  target: SshConnectionTarget;
}

export interface CredentialRecord {
  credential_id: string;
  name: string;
  kind: "ssh_password" | "ssh_key_passphrase" | "ssh_credential" | "vpn_password" | "tailscale_sign_in";
  storage: "keychain" | "vpn_settings" | "container_volume";
  target: string | null;
  account: string | null;
  created_at_ms: number | null;
  updated_at_ms: number | null;
  detail: string | null;
  action: "forget" | "manage_vpn";
  vpn_connection_id: string | null;
}

export interface CredentialSourceStatus {
  source: "keychain" | "vpn";
  state: "ready" | "partial" | "unavailable" | "unsupported";
  message: string | null;
}

export interface CredentialsSnapshot {
  metadata_import_required: boolean;
  credentials: CredentialRecord[];
  sources: CredentialSourceStatus[];
  checked_at_ms: number;
}

export interface IdentityFile {
  identity_id: string;
  path: string;
  display_path: string;
  file_version: string | null;
  key_type: string | null;
  fingerprint: string | null;
  encrypted: boolean | null;
  file_state: "ready" | "missing" | "unreadable" | "unsupported";
  passphrase_state: "saved" | "not_saved" | "not_required" | "file_changed" | "unknown";
  detail: string | null;
  used_by: string[];
}

export interface IdentitySnapshot {
  metadata_import_required: boolean;
  keychain_message: string | null;
  identity_files: IdentityFile[];
  complete: boolean;
  warning: string | null;
  keychain_available: boolean;
  checked_at_ms: number;
}

export interface SaveIdentityPassphraseRequest {
  path: string;
  file_version: string;
  passphrase: string;
}

export interface ComponentProtocolVersion {
  name: string;
  build: number;
  version: string;
  supported_versions: string[];
}

export interface ComponentVersionInfo {
  version: string | null;
  source_revision: string | null;
  source_fingerprint: string | null;
  dirty: boolean | null;
  protocols: ComponentProtocolVersion[];
}

export type ComponentVersionStatus = "current" | "outdated" | "newer" | "different_build" | "incompatible" | "unknown" | "not_running" | "unavailable";
export type ComponentActionKind = "restart" | "reconnect";

export interface ComponentVersionRow {
  component_id: string;
  component: "ctmux" | "ctld" | "ctmuxd" | "ctl-taskd" | "ctl_agent";
  label: string;
  location: "local" | "remote";
  host_id: string | null;
  /** Stable host grouping, including authenticated accounts without a saved host. */
  host_key?: string | null;
  host_name?: string | null;
  observation: "running" | "bundled" | "last_observed" | "installed" | "not_checked" | "legacy";
  status: ComponentVersionStatus;
  running: ComponentVersionInfo | null;
  available: ComponentVersionInfo | null;
  installed?: ComponentVersionInfo | null;
  restart_required?: boolean;
  legacy_protocols?: { name: string; version: number }[];
  /** Protocols compiled into the app; the available executable may itself be stale. */
  required_protocols?: ComponentProtocolVersion[];
  restart_supported: boolean;
  action: ComponentActionKind | null;
  detail: string | null;
  error: string | null;
}

export interface ComponentVersionsSnapshot {
  components: ComponentVersionRow[];
}

export interface ComponentActionPreflight {
  action_token: string;
  component_id: string;
  component: ComponentVersionRow["component"];
  location: ComponentVersionRow["location"];
  host_id: string | null;
  label: string;
  action: ComponentActionKind;
  running: ComponentVersionInfo | null;
  available: ComponentVersionInfo | null;
  impact: {
    ssh_connections: number | null;
    port_forwards: number | null;
    vpn_connections: number | null;
    terminal_sessions: number | null;
    description: string;
  };
}

export interface ComponentActionResult {
  component_id: string;
  component: ComponentVersionRow["component"];
  location: ComponentVersionRow["location"];
  host_id: string | null;
  action: ComponentActionKind;
  running: ComponentVersionInfo | null;
  detail: string | null;
}

export interface ComponentSessionsReset {
  scope: "local" | "remote";
  remote_id?: string | null;
  host_ids: string[];
  session_ids: string[];
  attachment_ids: string[];
}

export interface ComponentReconnectRequest {
  action_id: string;
  attachment_ids: string[];
}

export interface ComponentReconnectResult {
  attachment_id: string;
  replacement_attachment_id: string | null;
  error: string | null;
}

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

export interface TerminalPrefixSettings {
  key: string | null;
  bindings: { command_id: string; key: string | null }[];
}

export interface KeybindingsDocument {
  schema_version: 1;
  overrides: KeybindingOverride[];
  prefix?: TerminalPrefixSettings;
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

export interface RemoteIdentity {
  remote_id: string;
  agent_version: string;
  ctmux_restart_supported?: boolean;
  bundle?: RemoteAgentInstallResult;
}

export interface SshConnectionTarget {
  /** Runtime-only: retained workspace references cannot currently be connected. */
  unavailable?: string;
  /** Verified remote environment and last observed installed version. */
  remote_info?: RemoteIdentity;
  kind: "ssh";
  /** App-owned identity; stripped at the native transport boundary. */
  host_id?: string;
  /** Display-only host identity and selected connection method. */
  host_name?: string;
  method_id?: string;
  /** Runtime provider binding; persisted on the connection method. */
  tailscale_node_id?: string;
  /** SSH-config origin, retained even when using a private master. */
  ssh_config_alias?: string;
  /** Runtime copy of the method's master preference; absent uses its source default. */
  use_ssh_config_master?: boolean;
  destination: string;
  hostname?: string;
  user?: string;
  port?: number;
  identity_file?: string;
  /** Saved VPN profile, resolved to its current proxy before connecting. */
  vpn_connection_id?: string;
  /** Persisted route references, resolved from saved gateways and host methods. */
  gateway_route?: SshGatewayRouteStep[];
  /** Runtime-only gateway definitions passed to the native SSH boundary. */
  gateways?: ResolvedSshGateway[];
}

export type SshGatewayMode =
  | "automatic"
  | "native_only"
  | "agent_relay_only";

export interface SshGatewayReference {
  gateway_id: string;
  mode: SshGatewayMode;
}

export interface VpnGatewayReference {
  vpn_connection_id: string;
}

export interface HostGatewayReference {
  host_id: string;
  method_id: string;
  mode: SshGatewayMode;
}

export type SshGatewayRouteStep = SshGatewayReference | VpnGatewayReference | HostGatewayReference;

export interface WorkspaceSshGateway {
  kind?: "ssh" | "socks5";
  gateway_id: string;
  name: string;
  destination: string;
  hostname?: string;
  user?: string;
  port?: number;
  identity_file?: string;
  remote_info?: RemoteIdentity;
}

export interface ResolvedSshGatewayReference extends WorkspaceSshGateway {
  mode: SshGatewayMode;
}

export interface ResolvedVpnGateway {
  kind: "vpn";
  gateway_id: string;
  name: string;
  destination: string;
  vpn_connection_id: string;
  mode: "automatic";
  hostname?: never;
  user?: never;
  port?: never;
  identity_file?: never;
  remote_info?: never;
}

export type ResolvedSshGateway = ResolvedSshGatewayReference | ResolvedVpnGateway;

export type ConnectionTarget = { kind: "local" } | SshConnectionTarget;

/** Live broker observation; never persisted in the host catalog. */
export interface SshConnectionStatus {
  connected: boolean;
  manually_disconnected: boolean;
}

/** A bounded SSH greeting check that never authenticates or starts a route. */
export interface SshReachability {
  state: "available" | "unavailable" | "not_checked" | "unknown";
  reason: "vpn_disconnected" | "route_requires_connection" | "unsupported_configuration" |
    "connection_refused" | "timed_out" | "invalid_greeting" | "check_failed" | null;
  message: string | null;
}

export interface HostReachabilityObservation {
  state: SshReachability["state"] | "checking";
  reason: SshReachability["reason"];
  /** Methods whose endpoint answered with an SSH greeting; these are not authenticated connections. */
  method_names: string[];
  message: string | null;
  checked_at_ms: number | null;
}

export interface HostConnectionStatus {
  state: "checking" | "connected" | "connecting" | "disconnecting" | "disconnected" | "error";
  method_names: string[];
  message: string | null;
  /** Manual pause policy is independent of observed SSH-master availability. */
  manually_disconnected?: boolean;
  /** Runtime SSH-master evidence; this does not establish terminal/network health. */
  observation?: HostConnectionObservation;
  /** Independent endpoint reachability; never grants connection or attachment authority. */
  reachability?: HostReachabilityObservation;
  /** An attempt is independent of already observed SSH-master availability. */
  operation?: HostConnectionOperation;
}

export interface HostConnectionObservation {
  availability: "available" | "unavailable" | "unknown";
  completeness: "complete" | "partial" | "failed" | "pending";
  method_names: string[];
  failed_method_names: string[];
  message: string | null;
  checked_at_ms: number | null;
  /** A route/configuration change invalidated the previous observation. */
  stale: boolean;
}

export interface HostConnectionOperation {
  kind: "connect" | "disconnect" | null;
  state: "idle" | "pending" | "failed";
  method_name: string | null;
  message: string | null;
}

export type HostConnectionChange = (
  target: ConnectionTarget,
  state: "connecting" | "connected" | "error" | "cancelled",
  message?: string,
) => void;

export interface WorkspaceHost {
  /** Runtime provenance; omitted on persisted records and legacy callers. */
  source?: "saved" | "ssh_config" | "tailscale" | "unavailable";
  /** Runtime discovery metadata, never a saved host definition. */
  tailscale_device?: TailscaleDevice;
  /** Runtime workspace observation, separate from the saved catalog identity. */
  expected_remote_info?: RemoteIdentity;
  host_id: string;
  name: string;
  connection_methods: WorkspaceConnectionMethod[];
  preferred_method_id: string | null;
  remote_info?: RemoteIdentity;
}

export interface WorkspaceConnectionMethod {
  method_id: string;
  name: string;
  target: SshConnectionTarget;
  tailscale_node_id?: string;
  ssh_config_alias?: string;
  /** Absent uses SSH-config sharing for aliases and a private master otherwise. */
  use_ssh_config_master?: boolean;
}

export interface LegacyWorkspaceHost {
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
  last_known_terminal_size?: TerminalSize | null;
  last_seen_at_ms?: number | null;
}

export interface WorkspaceDocument {
  schema_version: 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8;
  workspace_id: string;
  /** Legacy definitions, migrated to hosts.json in schema 8. */
  hosts?: (WorkspaceHost | LegacyWorkspaceHost)[];
  host_identities?: WorkspaceHostIdentity[];
  sessions: WorkspaceSession[];
  tabs: WorkspaceTab[];
  active_tab: WorkspaceTab | null;
  task_definitions?: SavedTaskDefinition[];
  task_drafts?: TaskDefinitionDraft[];
  task_definition_scope?: TaskDefinitionScope;
  sidebar_view?: WorkspaceSidebarView;
  task_references?: TaskReference[];
  port_forwards?: WorkspacePortForward[];
  ssh_gateways?: WorkspaceSshGateway[];
}

export interface LegacyWorkspaceDocument extends WorkspaceDocument {
  schema_version: 1 | 2 | 3 | 4 | 5 | 6 | 7;
  hosts: (WorkspaceHost | LegacyWorkspaceHost)[];
}

export interface WorkspaceHostIdentity {
  host_id: string;
  remote_info: RemoteIdentity;
}

export interface HostCatalogDocument {
  schema_version: 1;
  hosts: WorkspaceHost[];
  ssh_gateways: WorkspaceSshGateway[];
}

export interface HostCatalogSnapshot {
  revision: string | null;
  document: HostCatalogDocument;
}

export type WorkspaceSidebarView = "sessions" | "tasks" | "ports" | "vpn";

export type VpnProvider = "openconnect" | "tailscale";

/** Saved connection metadata. Passwords are never returned to the webview. */
export type VpnConnection = OpenconnectVpnConnection | TailscaleVpnConnection;

export interface OpenconnectVpnConnection {
  /** Missing on legacy OpenConnect profiles. */
  provider?: "openconnect";
  connection_id: string;
  name: string;
  url: string;
  username: string;
  has_password: boolean;
  auth_method: string | null;
  target_ip: string | null;
}

export interface TailscaleVpnConnection {
  provider: "tailscale";
  connection_id: string;
  name: string;
  hostname: string | null;
  accept_routes: boolean;
}

export type VpnConnectionInput = OpenconnectVpnConnectionInput | TailscaleVpnConnection;

export interface OpenconnectVpnConnectionInput {
  provider?: "openconnect";
  connection_id: string;
  name: string;
  url: string;
  username: string;
  /** Null preserves the stored password when editing a connection. */
  password: string | null;
  auth_method: string | null;
  target_ip: string | null;
}

export interface VpnEnrollmentInput {
  name: string;
  hostname: string | null;
  accept_routes: boolean;
}

export interface VpnEnrollmentSnapshot {
  enrollment_id: string;
  connection_id: string;
  status: VpnStatus;
  error: { code: string; message: string } | null;
}

export interface VpnConnectionsSnapshot {
  revision: string | null;
  connections: VpnConnection[];
}

export type VpnState = "stopped" | "starting" | "connected" | "stopping";

export interface VpnStatus {
  provider?: VpnProvider;
  auth_url?: string | null;
  hostname?: string | null;
  tailnet?: string | null;
  message?: string | null;
  vpn_id?: string | null;
  state: VpnState;
  running: boolean;
  connection_id: string | null;
  /** Older ctld versions may omit connection metadata. */
  vpn_url?: string | null;
  username?: string | null;
  /** Local SOCKS5 proxy, distinct from the VPN server. */
  endpoint: string | null;
  container_name: string | null;
  container_id?: string | null;
  shared_container?: boolean;
  /** Omitted by older daemons; false means this daemon has no heartbeat interest. */
  locally_connected?: boolean | null;
  /** Metadata is retained because the current container state could not be observed. */
  status_unavailable?: boolean;
}

export interface VpnSnapshot {
  /** Missing on older daemons, which support only OpenConnect. */
  supported_providers?: VpnProvider[];
  supports_tailscale_enrollment?: boolean;
  connections: VpnStatus[];
  supports_multiple: boolean;
  discovery_warnings?: string[];
}

export interface LocalPortForward {
  forward_id: string;
  bind_address: "127.0.0.1";
  local_port: number;
  remote_host: string;
  remote_port: number;
}

export interface WorkspacePortForward extends LocalPortForward {
  host_id: string;
  name: string;
  enabled: boolean;
}

export type PortForwardState =
  | "waiting_for_authentication"
  | "active"
  | "error";

export interface PortForwardStatus {
  forward: LocalPortForward;
  state: PortForwardState;
  message: string | null;
}

export interface TcpListener {
  bind_address: string;
  port: number;
}

export interface TcpListenerCatalog {
  listeners: TcpListener[];
  warnings: string[];
}

export interface LocalPortAvailability {
  port: number;
  available: boolean;
  message: string | null;
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

export interface TailscaleDevice {
  node_id: string;
  name: string;
  dns_name: string | null;
  addresses: string[];
  online: boolean | null;
  os: string | null;
}

export interface TailscaleDeviceCatalog {
  devices: TailscaleDevice[];
  warnings: string[];
  state: "available" | "not_installed" | "not_running" | "needs_login" | "error";
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
  kind:
    | "confirm"
    | "secret"
    | "credential_save"
    | "credential_save_error";
  message: string;
  warning?: string | null;
}

export interface RemoteCtmuxRestartResult {
  terminated_sessions: number;
}

export interface RemoteAgentInstallResult {
  app_version: string;
  bundle_id: string;
  git_revision: string;
  target_triple: string;
}

export interface RemoteAgentInstallProgress {
  phase:
    | "detecting_platform"
    | "verifying_bundle"
    | "connecting"
    | "transferring"
    | "extracting"
    | "checking"
    | "activating"
    | "complete";
  file_name: string | null;
  transferred_bytes: number;
  total_bytes: number;
  bytes_per_second: number;
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
  // Missing on saved workspace entries until the daemon is inspected.
  view_id?: string;
  terminal_id?: string;
  target: ConnectionTarget;
  session_id: string;
  name: string;
  status: SessionStatus;
  terminal_size: TerminalSize;
  next_sequence: Sequence;
  last_seen_at_ms?: number | null;
  /** Internal provenance: restored fallback dimensions are never observed. */
  terminal_size_known?: boolean;
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

export type NotificationSeverity = "info" | "success" | "warning" | "error";

export interface NotificationAction {
  label: string;
  command_id: string;
  args?: { session_key?: string; target_key?: string; value?: string };
}

export interface NotificationInput {
  severity: NotificationSeverity;
  title: string;
  message: string;
  source: string;
  actions?: readonly NotificationAction[];
}

export interface AppNotification extends NotificationInput {
  id: string;
  source_key: string;
  created_at: number;
  updated_at: number;
  occurrence_count: number;
  read: boolean;
  toast_visible: boolean;
  resolved_at: number | null;
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

export interface TerminalHistoryRow {
  text: string;
  wrapped: boolean;
}

export interface TerminalHistoryManifest {
  snapshot_id: string;
  sequence: Sequence;
  generation: string;
  revision: string;
  total_rows: string;
  total_bytes: string;
  total_lines: string;
  first_line: string;
  truncated: boolean;
  content_hash: string;
  scrollback_limit: string;
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
  history_manifest?: TerminalHistoryManifest;
  history_gap: boolean;
}

export interface HistorySyncedEvent extends AttachmentEventBase {
  event_type: "history_synced";
  snapshot_id: string;
  checkpoint: TerminalCheckpoint;
  history: TerminalHistorySnapshot;
  rows: TerminalHistoryRow[];
  scrollback_limit: string;
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

export interface SessionObservedEvent extends AttachmentEventBase {
  event_type: "session_observed";
  last_seen_at_ms: number;
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
  | HistorySyncedEvent
  | OutputEvent
  | PtyGeometryChangedEvent
  | LeaseStatusEvent
  | ShellStateChangedEvent
  | SessionObservedEvent
  | ServerErrorEvent
  | SessionEndedEvent
  | AttachmentExitedEvent
  | AttachmentErrorEvent;

export type ConnectionPhase =
  | "idle"
  | "connecting"
  | "attached"
  | "reconnecting"
  | "retry_wait"
  | "disconnected"
  | "ended"
  | "error";

export interface AttachmentViewState {
  phase: ConnectionPhase;
  /** Present only while a retry is scheduled; never persisted with the session. */
  retry_at_ms?: number | null;
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
  interactive?: { session_id: string | null; instance_id: string; ctmux_socket: string; released: boolean };
}
export interface ManagedTask {
  task_id: string;
  definition: TaskDefinition;
  desired_state: "running" | "stopped";
  active_run: TaskRun | null;
  last_run: TaskRun | null;
}
export interface TaskDefinitionDraft {
  command_line?: string;
  definition_id: string;
  definition: TaskDefinition;
  scope?: TaskDefinitionScope;
  /** Missing means a legacy draft whose original revision must be reviewed. */
  base_revision?: string | null;
}
export type TaskDefinitionScope =
  | { kind: "global" }
  | { kind: "project"; project_root: string };
export interface TaskDefinitionCatalog {
  scope: TaskDefinitionScope;
  path: string;
  definitions: SavedTaskDefinition[];
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
  definition_scope?: TaskDefinitionScope;
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

export type ViewLayout =
  | { kind: "terminal"; terminal_id: string }
  | { kind: "split"; axis: "horizontal" | "vertical"; children: ViewLayout[] };

export interface SessionView {
  session_name: string;
  session_id: string;
  view_id: string;
  revision: string;
  canvas_size: TerminalSize;
  panes: { terminal_id: string; left: number; top: number; columns: number; rows: number }[];
  layout: ViewLayout;
  terminals: { terminal_id: string; name: string; next_sequence: Sequence; terminal_size: TerminalSize }[];
}

export type ViewAction =
  | { kind: "get"; session_id: string }
  | { kind: "split"; terminal_id: string; axis: "horizontal" | "vertical"; terminal_size: TerminalSize; working_directory: string | null }
  | { kind: "update"; session_id: string; expected_revision: string; layout: ViewLayout }
  | { kind: "promote"; terminal_id: string; name: string | null }
  | { kind: "merge"; source: string; destination: string }
  | { kind: "kill_terminal"; terminal_id: string };

export interface ArchivedTerminalInfo {
  terminal_id: string;
  reason: string;
  lines: string[];
  history_gap?: boolean;
}
export interface SessionArchive {
  session_id: string;
  name: string;
  host_key: string;
  archived_at_ms: number;
  expires_at_ms: number;
  terminals: ArchivedTerminalInfo[];
}
export type ArchiveAction =
  | { kind: "list" }
  | { kind: "save"; archive: SessionArchive }
  | { kind: "delete"; host_key: string; session_id: string }
  | { kind: "read"; host_key: string; session_id: string; terminal_id: string; offset: string };
export type ArchiveResponse = { kind: "list"; archives: SessionArchive[] } | { kind: "saved" } | { kind: "deleted" } | { kind: "output"; lines: string[]; next_offset: string | null; history_gap: boolean };


export interface CachedSessionPresentation {
  terminal_id: string;
  checkpoint: TerminalCheckpoint;
  history: string[];
  history_gap: boolean;
}
export type SessionCacheAction =
  | { kind: "load"; host_key: string; session_id: string; terminal_id?: string }
  | { kind: "archive"; host_key: string; session_id: string; reason: string };
export type SessionCacheResponse =
  | { kind: "loaded"; cache: CachedSessionPresentation | null }
  | { kind: "archived" };


export type ComponentBundlePurpose = "local" | "upload";
export type ComponentBundlePhase = "verifying" | "selecting";
export interface ComponentBundle {
  bundle_id: string;
  target_triple: string;
  source: "ci" | "release" | "local";
  app_version: string;
  git_revision: string | null;
  dirty: boolean;
  compatible: boolean;
  local_use: "selected" | "available" | "unavailable";
  upload_use: "selected" | "available" | "unavailable";
}
export interface ComponentBundlesSnapshot {
  bundles: ComponentBundle[];
  errors: string[];
}
export interface ComponentBundleSelection {
  bundle_id: string;
  target_triple: string;
  purpose: ComponentBundlePurpose;
}
export interface ComponentBundleSelectionResult extends ComponentBundleSelection {
  services_preserved: boolean;
}
