use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use rmux_client::{AttachExit, AttachExitReason, AttachedSession, AttachmentControl};
use rmux_proto::{
  ErrorCode, LeaseKind, LeaseStatus, PromptPhase, SessionInfo, SessionStatus, ShellState,
  ShellType, TerminalCheckpoint, TerminalHistorySnapshot, TerminalSize, TuiHint,
};
use serde::{Deserialize, Serialize};

use crate::error::{CommandErrorDto, CommandResult, protocol_error_code};

/// Explicit endpoint identity carried by every operation that opens a stream.
///
/// The SSH value is an OpenSSH destination or configured host alias. Optional
/// app-local settings are passed as fixed SSH arguments; arbitrary options,
/// remote commands, credentials, and forwarding configuration remain absent.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConnectionTargetDto {
  Local,
  Ssh {
    destination: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hostname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    identity_file: Option<String>,
  },
}

impl ConnectionTargetDto {
  #[cfg(test)]
  #[must_use]
  pub fn ssh(destination: impl Into<String>) -> Self {
    Self::Ssh {
      destination: destination.into(),
      hostname: None,
      user: None,
      port: None,
      identity_file: None,
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TargetRequestDto {
  pub target: ConnectionTargetDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SshConfigHostDto {
  pub destination: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SshConfigHostCatalogDto {
  pub hosts: Vec<SshConfigHostDto>,
  pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SshIdentityFileDto {
  pub path: String,
  pub display_path: String,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct SshIdentityFileCatalogDto {
  pub identity_files: Vec<SshIdentityFileDto>,
  pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SaveSshConfigHostRequestDto {
  pub alias: String,
  pub hostname: String,
  pub user: Option<String>,
  pub port: Option<u16>,
  pub identity_file: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SaveSshConfigHostResponseDto {
  pub destination: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSizeDto {
  pub columns: u16,
  pub rows: u16,
  pub pixel_width: Option<u16>,
  pub pixel_height: Option<u16>,
}

impl TerminalSizeDto {
  pub fn into_proto(self) -> CommandResult<TerminalSize> {
    if self.columns == 0 || self.rows == 0 {
      return Err(CommandErrorDto::new(
        "invalid_terminal_size",
        "terminal columns and rows must be greater than zero",
      ));
    }
    Ok(TerminalSize {
      columns: self.columns,
      rows: self.rows,
      pixel_width: self.pixel_width.unwrap_or(0),
      pixel_height: self.pixel_height.unwrap_or(0),
    })
  }
}

impl From<TerminalSize> for TerminalSizeDto {
  fn from(value: TerminalSize) -> Self {
    Self {
      columns: value.columns,
      rows: value.rows,
      pixel_width: (value.pixel_width != 0).then_some(value.pixel_width),
      pixel_height: (value.pixel_height != 0).then_some(value.pixel_height),
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatusDto {
  Running,
  Exited,
}

impl From<SessionStatus> for SessionStatusDto {
  fn from(value: SessionStatus) -> Self {
    match value {
      SessionStatus::Running => Self::Running,
      SessionStatus::Exited => Self::Exited,
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionDto {
  pub target: ConnectionTargetDto,
  pub session_id: String,
  pub name: String,
  pub status: SessionStatusDto,
  pub next_sequence: String,
  pub terminal_size: TerminalSizeDto,
}

impl SessionDto {
  #[must_use]
  pub fn new(value: SessionInfo, target: ConnectionTargetDto) -> Self {
    Self {
      target,
      session_id: value.session_id,
      name: value.name,
      status: value.status.into(),
      next_sequence: value.next_sequence.to_string(),
      terminal_size: value.terminal_size.into(),
    }
  }
}

/// Session list data enriched with non-attaching shell snapshots.
///
/// Each snapshot is keyed by the durable session ID rather than its mutable
/// display name. A missing entry is expected when a session exits between the
/// list request and its best-effort shell-state inspection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionListDto {
  pub sessions: Vec<SessionDto>,
  pub shell_states: BTreeMap<String, ShellStateDto>,
}

/// Result of a destructive local `rmuxd` restart.
///
/// The count includes sessions for which the daemon accepted a termination
/// request before the replacement daemon passed its health check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RestartLocalDaemonResponseDto {
  pub terminated_sessions: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseKindDto {
  Input,
  Layout,
}

impl From<LeaseKindDto> for LeaseKind {
  fn from(value: LeaseKindDto) -> Self {
    match value {
      LeaseKindDto::Input => Self::Input,
      LeaseKindDto::Layout => Self::Layout,
    }
  }
}

impl From<LeaseKind> for LeaseKindDto {
  fn from(value: LeaseKind) -> Self {
    match value {
      LeaseKind::Input => Self::Input,
      LeaseKind::Layout => Self::Layout,
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LeaseStatusDto {
  pub held: bool,
  pub owned_by_client: bool,
}

impl From<LeaseStatus> for LeaseStatusDto {
  fn from(value: LeaseStatus) -> Self {
    Self {
      held: value.held,
      owned_by_client: value.owned_by_client,
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellTypeDto {
  Bash,
  Fish,
  Pwsh,
  Zsh,
  Cmd,
  Sh,
  Unknown,
}

impl From<ShellType> for ShellTypeDto {
  fn from(value: ShellType) -> Self {
    match value {
      ShellType::Bash => Self::Bash,
      ShellType::Fish => Self::Fish,
      ShellType::Pwsh => Self::Pwsh,
      ShellType::Zsh => Self::Zsh,
      ShellType::Cmd => Self::Cmd,
      ShellType::Sh => Self::Sh,
      ShellType::Unknown => Self::Unknown,
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptPhaseDto {
  Unknown,
  AtPrompt,
  Editing,
  Running,
}

impl From<PromptPhase> for PromptPhaseDto {
  fn from(value: PromptPhase) -> Self {
    match value {
      PromptPhase::Unknown => Self::Unknown,
      PromptPhase::AtPrompt => Self::AtPrompt,
      PromptPhase::Editing => Self::Editing,
      PromptPhase::Running => Self::Running,
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TuiHintDto {
  Unknown,
  Inline,
  AlternateScreen,
}

impl From<TuiHint> for TuiHintDto {
  fn from(value: TuiHint) -> Self {
    match value {
      TuiHint::Unknown => Self::Unknown,
      TuiHint::Inline => Self::Inline,
      TuiHint::AlternateScreen => Self::AlternateScreen,
    }
  }
}

/// Privacy-preserving shell state for GUI status and title presentation.
///
/// Editable command-line data is deliberately absent from this DTO. The
/// running-command summary is separately requested by the GUI and remains
/// subject to rmuxd's input-lease visibility policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShellStateDto {
  pub revision: String,
  pub observed_sequence: String,
  pub shell_type: ShellTypeDto,
  pub cwd: Option<String>,
  pub cwd_display: Option<String>,
  pub prompt_phase: PromptPhaseDto,
  pub running_command: Option<String>,
  pub tui_hint: TuiHintDto,
}

impl From<ShellState> for ShellStateDto {
  fn from(value: ShellState) -> Self {
    Self {
      revision: value.revision.to_string(),
      observed_sequence: value.observed_sequence.to_string(),
      shell_type: value.shell.shell_type.into(),
      cwd: value.cwd,
      cwd_display: value.cwd_display,
      prompt_phase: value.prompt_phase.into(),
      running_command: value.running_command,
      tui_hint: value.tui_hint.into(),
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CreateSessionRequestDto {
  pub target: ConnectionTargetDto,
  pub working_directory: Option<String>,
  pub terminal_size: TerminalSizeDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct KillSessionRequestDto {
  pub target: ConnectionTargetDto,
  pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct OpenAttachmentRequestDto {
  pub target: ConnectionTargetDto,
  pub session: String,
  pub terminal_size: TerminalSizeDto,
  pub resume_from: Option<String>,
  pub request_input_lease: bool,
  pub request_layout_lease: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OpenAttachmentResponseDto {
  pub attachment_id: String,
  pub session: SessionDto,
  pub replay_from: String,
  pub history_gap: bool,
  pub terminal_size_mismatch: bool,
  pub input_lease: LeaseStatusDto,
  pub layout_lease: LeaseStatusDto,
  pub shell_state: ShellStateDto,
}

impl OpenAttachmentResponseDto {
  pub fn new(
    attachment_id: String,
    attached: &AttachedSession,
    target: ConnectionTargetDto,
  ) -> Self {
    Self {
      attachment_id,
      session: SessionDto::new(attached.session.clone(), target),
      replay_from: attached.replay_from.to_string(),
      history_gap: attached.history_gap,
      terminal_size_mismatch: attached.terminal_size_mismatch,
      input_lease: attached.input_lease.clone().into(),
      layout_lease: attached.layout_lease.clone().into(),
      shell_state: attached.shell_state.clone().into(),
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AttachmentRequestDto {
  pub attachment_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SendInputRequestDto {
  pub attachment_id: String,
  pub data_base64: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ResizeAttachmentRequestDto {
  pub attachment_id: String,
  pub terminal_size: TerminalSizeDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AttachmentLeaseRequestDto {
  pub attachment_id: String,
  pub lease: LeaseKindDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AcknowledgeAttachmentEventRequestDto {
  pub attachment_id: String,
  pub event_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TerminalCheckpointDto {
  pub format: String,
  pub format_version: u16,
  pub sequence: String,
  pub terminal_size: TerminalSizeDto,
  pub payload_base64: String,
  pub input_prefix_base64: String,
}

impl From<TerminalCheckpoint> for TerminalCheckpointDto {
  fn from(value: TerminalCheckpoint) -> Self {
    Self {
      format: value.format,
      format_version: value.format_version,
      sequence: value.sequence.to_string(),
      terminal_size: value.terminal_size.into(),
      payload_base64: BASE64_STANDARD.encode(value.payload),
      input_prefix_base64: BASE64_STANDARD.encode(value.input_prefix),
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TerminalHistorySnapshotDto {
  pub format: String,
  pub format_version: u16,
  pub sequence: String,
  pub generation: String,
  pub revision: String,
  pub retained_bytes: String,
  pub truncated: bool,
  pub lines: Vec<String>,
}

impl From<TerminalHistorySnapshot> for TerminalHistorySnapshotDto {
  fn from(value: TerminalHistorySnapshot) -> Self {
    Self {
      format: value.format,
      format_version: value.format_version,
      sequence: value.sequence.to_string(),
      generation: value.generation.to_string(),
      revision: value.revision.to_string(),
      retained_bytes: value.retained_bytes.to_string(),
      truncated: value.truncated,
      lines: value.lines,
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationAcknowledgement {
  Checkpoint { sequence: u64 },
  Output { sequence_end: u64 },
  Geometry { observed_sequence: u64 },
}

impl PresentationAcknowledgement {
  pub async fn apply(
    self,
    control: &AttachmentControl,
  ) -> Result<(), rmux_client::AttachmentAcknowledgementError> {
    match self {
      Self::Checkpoint { sequence } => control.acknowledge_checkpoint(sequence).await,
      Self::Output { sequence_end } => control.acknowledge_output(sequence_end).await,
      Self::Geometry { observed_sequence } => control.acknowledge_geometry(observed_sequence).await,
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentExitReasonDto {
  Detached,
  ConnectionClosed,
  SessionEnded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "event_type", rename_all = "snake_case")]
pub enum AttachmentEventDto {
  Checkpoint {
    attachment_id: String,
    event_id: String,
    checkpoint: TerminalCheckpointDto,
    history: TerminalHistorySnapshotDto,
    history_gap: bool,
  },
  Output {
    attachment_id: String,
    event_id: String,
    sequence_start: String,
    sequence_end: String,
    data_base64: String,
  },
  PtyGeometryChanged {
    attachment_id: String,
    event_id: String,
    terminal_size: TerminalSizeDto,
    observed_sequence: String,
  },
  LeaseStatus {
    attachment_id: String,
    lease: LeaseKindDto,
    status: LeaseStatusDto,
  },
  ShellStateChanged {
    attachment_id: String,
    shell_state: ShellStateDto,
  },
  ServerError {
    attachment_id: String,
    code: String,
    message: String,
  },
  SessionEnded {
    attachment_id: String,
    session_id: String,
    exit_code: Option<u32>,
  },
  AttachmentExited {
    attachment_id: String,
    reason: AttachmentExitReasonDto,
    exit_code: Option<u32>,
    next_sequence: Option<String>,
    received_sequence: String,
  },
  AttachmentError {
    attachment_id: String,
    code: String,
    message: String,
  },
}

impl AttachmentEventDto {
  pub fn checkpoint(
    attachment_id: &str,
    event_id: String,
    checkpoint: TerminalCheckpoint,
    history: TerminalHistorySnapshot,
    history_gap: bool,
  ) -> Self {
    Self::Checkpoint {
      attachment_id: attachment_id.into(),
      event_id,
      checkpoint: checkpoint.into(),
      history: history.into(),
      history_gap,
    }
  }

  pub fn output(
    attachment_id: &str,
    event_id: String,
    sequence_start: u64,
    sequence_end: u64,
    data: &[u8],
  ) -> Self {
    Self::Output {
      attachment_id: attachment_id.into(),
      event_id,
      sequence_start: sequence_start.to_string(),
      sequence_end: sequence_end.to_string(),
      data_base64: BASE64_STANDARD.encode(data),
    }
  }

  pub fn pty_geometry_changed(
    attachment_id: &str,
    event_id: String,
    terminal_size: TerminalSize,
    observed_sequence: u64,
  ) -> Self {
    Self::PtyGeometryChanged {
      attachment_id: attachment_id.into(),
      event_id,
      terminal_size: terminal_size.into(),
      observed_sequence: observed_sequence.to_string(),
    }
  }

  pub fn lease_status(attachment_id: &str, lease: LeaseKind, status: LeaseStatus) -> Self {
    Self::LeaseStatus {
      attachment_id: attachment_id.into(),
      lease: lease.into(),
      status: status.into(),
    }
  }

  pub fn shell_state_changed(attachment_id: &str, state: ShellState) -> Self {
    Self::ShellStateChanged {
      attachment_id: attachment_id.into(),
      shell_state: state.into(),
    }
  }

  pub fn server_error(attachment_id: &str, code: &ErrorCode, message: String) -> Self {
    Self::ServerError {
      attachment_id: attachment_id.into(),
      code: protocol_error_code(code).into(),
      message,
    }
  }

  pub fn session_ended(attachment_id: &str, session_id: String, exit_code: Option<u32>) -> Self {
    Self::SessionEnded {
      attachment_id: attachment_id.into(),
      session_id,
      exit_code,
    }
  }

  pub fn attachment_exited(
    attachment_id: &str,
    exit: &AttachExit,
    require_checkpoint: bool,
  ) -> Self {
    let (reason, exit_code) = match exit.reason {
      AttachExitReason::Detached => (AttachmentExitReasonDto::Detached, None),
      AttachExitReason::ConnectionClosed => (AttachmentExitReasonDto::ConnectionClosed, None),
      AttachExitReason::SessionEnded { exit_code } => {
        (AttachmentExitReasonDto::SessionEnded, exit_code)
      }
    };
    Self::AttachmentExited {
      attachment_id: attachment_id.into(),
      reason,
      exit_code,
      next_sequence: (!require_checkpoint)
        .then_some(exit.next_sequence)
        .flatten()
        .map(|sequence| sequence.to_string()),
      received_sequence: exit.received_sequence.to_string(),
    }
  }

  pub fn attachment_error(
    attachment_id: &str,
    code: impl Into<String>,
    message: impl Into<String>,
  ) -> Self {
    Self::AttachmentError {
      attachment_id: attachment_id.into(),
      code: code.into(),
      message: message.into(),
    }
  }
}

pub fn decode_input(data_base64: &str) -> CommandResult<Vec<u8>> {
  const MAX_INPUT_BYTES: usize = 1024 * 1024;

  let data = BASE64_STANDARD.decode(data_base64).map_err(|error| {
    CommandErrorDto::new(
      "invalid_input_base64",
      format!("invalid input bytes: {error}"),
    )
  })?;
  if data.len() > MAX_INPUT_BYTES {
    return Err(CommandErrorDto::new(
      "input_too_large",
      format!("terminal input exceeds the {MAX_INPUT_BYTES}-byte command limit"),
    ));
  }
  Ok(data)
}

pub fn parse_sequence(value: Option<String>) -> CommandResult<Option<u64>> {
  value
    .map(|value| {
      value.parse::<u64>().map_err(|error| {
        CommandErrorDto::new(
          "invalid_sequence",
          format!("sequence must be an unsigned decimal integer: {error}"),
        )
      })
    })
    .transpose()
}

#[cfg(test)]
mod tests {
  use super::*;
  use rmux_proto::{ShellCapabilities, ShellDescriptor};

  #[test]
  fn session_u64_values_are_decimal_strings() {
    let dto = SessionDto::new(
      SessionInfo {
        session_id: "session".into(),
        name: "large".into(),
        status: SessionStatus::Running,
        created_at_ms: u64::MAX,
        next_sequence: u64::MAX,
        terminal_size: TerminalSize::default(),
      },
      ConnectionTargetDto::Local,
    );
    let json = serde_json::to_value(dto).unwrap();
    assert_eq!(json["target"]["kind"], "local");
    assert_eq!(json["next_sequence"], u64::MAX.to_string());
  }

  #[test]
  fn ssh_target_uses_a_tagged_snake_case_shape() {
    let target: ConnectionTargetDto = serde_json::from_value(serde_json::json!({
      "kind": "ssh",
      "destination": "rmux-docker"
    }))
    .unwrap();

    assert_eq!(target, ConnectionTargetDto::ssh("rmux-docker"));
    assert_eq!(
      serde_json::to_value(target).unwrap(),
      serde_json::json!({ "kind": "ssh", "destination": "rmux-docker" })
    );
  }

  #[test]
  fn app_local_ssh_target_serializes_only_structured_settings() {
    let target: ConnectionTargetDto = serde_json::from_value(serde_json::json!({
      "kind": "ssh",
      "destination": "rmux-remote-test",
      "hostname": "127.0.0.1",
      "user": "rmux",
      "port": 2222,
      "identity_file": "~/.ssh/local.id_rsa"
    }))
    .unwrap();

    assert_eq!(
      serde_json::to_value(target).unwrap(),
      serde_json::json!({
        "kind": "ssh",
        "destination": "rmux-remote-test",
        "hostname": "127.0.0.1",
        "user": "rmux",
        "port": 2222,
        "identity_file": "~/.ssh/local.id_rsa"
      })
    );
  }

  #[test]
  fn ssh_config_catalog_uses_destination_strings_and_warnings() {
    let json = serde_json::to_value(SshConfigHostCatalogDto {
      hosts: vec![SshConfigHostDto {
        destination: "rmux-docker".into(),
      }],
      warnings: vec!["partial discovery".into()],
    })
    .unwrap();

    assert_eq!(json["hosts"][0]["destination"], "rmux-docker");
    assert_eq!(json["warnings"][0], "partial discovery");
  }

  #[test]
  fn session_list_keeps_shell_states_keyed_by_session_id() {
    let states = BTreeMap::from([(
      "session-1".to_owned(),
      ShellStateDto::from(ShellState::default()),
    )]);
    let json = serde_json::to_value(SessionListDto {
      sessions: Vec::new(),
      shell_states: states,
    })
    .unwrap();

    assert!(json["sessions"].as_array().is_some_and(Vec::is_empty));
    assert!(json["shell_states"]["session-1"].is_object());
  }

  #[test]
  fn kill_session_request_uses_stable_snake_case_session_id() {
    let request: KillSessionRequestDto = serde_json::from_value(serde_json::json!({
      "target": { "kind": "local" },
      "session_id": "stable-id"
    }))
    .unwrap();

    assert_eq!(request.session_id, "stable-id");
    assert!(
      serde_json::from_value::<KillSessionRequestDto>(serde_json::json!({
        "sessionId": "unstable-name"
      }))
      .is_err()
    );
  }

  #[test]
  fn daemon_restart_result_uses_a_stable_session_count_field() {
    let json = serde_json::to_value(RestartLocalDaemonResponseDto {
      terminated_sessions: 2,
    })
    .unwrap();

    assert_eq!(json["terminated_sessions"], 2);
  }

  #[test]
  fn output_preserves_sequence_precision_and_bytes() {
    let event = AttachmentEventDto::output(
      "attachment",
      "event".into(),
      u64::MAX - 2,
      u64::MAX,
      &[0, 0xff],
    );
    let json = serde_json::to_value(event).unwrap();
    assert_eq!(json["event_type"], "output");
    assert_eq!(json["sequence_end"], u64::MAX.to_string());
    assert_eq!(json["data_base64"], "AP8=");
  }

  #[test]
  fn terminal_history_preserves_u64_precision_as_decimal_strings() {
    let dto = TerminalHistorySnapshotDto::from(TerminalHistorySnapshot {
      format: rmux_proto::TERMINAL_HISTORY_FORMAT.into(),
      format_version: rmux_proto::TERMINAL_HISTORY_FORMAT_VERSION,
      sequence: u64::MAX,
      generation: u64::MAX - 1,
      revision: u64::MAX - 2,
      retained_bytes: u64::MAX - 3,
      truncated: true,
      lines: vec!["history".into()],
    });

    let json = serde_json::to_value(dto).unwrap();
    assert_eq!(json["sequence"], u64::MAX.to_string());
    assert_eq!(json["generation"], (u64::MAX - 1).to_string());
    assert_eq!(json["revision"], (u64::MAX - 2).to_string());
    assert_eq!(json["retained_bytes"], (u64::MAX - 3).to_string());
  }

  #[test]
  fn shell_state_dto_never_serializes_the_editable_command_line() {
    let state = ShellState {
      current_command_line: Some(rmux_proto::CommandLine {
        text: "secret".into(),
        cursor_scalar_offset: Some(6),
      }),
      ..ShellState::default()
    };
    let json = serde_json::to_string(&ShellStateDto::from(state)).unwrap();
    assert!(!json.contains("secret"));
    assert!(!json.contains("current_command_line"));
  }

  #[test]
  fn shell_state_dto_serializes_a_valid_running_summary() {
    let state = ShellState {
      shell: ShellDescriptor {
        shell_type: ShellType::Zsh,
        integration_version: Some(2),
        capabilities: ShellCapabilities {
          reports_prompt_phase: true,
          reports_running_command: true,
          ..ShellCapabilities::default()
        },
      },
      prompt_phase: PromptPhase::Running,
      running_command: Some("cargo test".into()),
      ..ShellState::default()
    };
    assert!(state.has_valid_metadata());

    let json = serde_json::to_string(&ShellStateDto::from(state)).unwrap();
    assert!(json.contains("running_command"));
    assert!(json.contains("cargo test"));
  }

  #[test]
  fn shell_state_dto_keeps_raw_and_display_working_directories_separate() {
    let state = ShellState {
      cwd: Some("/Users/me/project".into()),
      cwd_display: Some("~/project".into()),
      ..ShellState::default()
    };

    let json = serde_json::to_value(ShellStateDto::from(state)).unwrap();
    assert_eq!(json["cwd"], "/Users/me/project");
    assert_eq!(json["cwd_display"], "~/project");
  }

  #[test]
  fn pending_renderer_event_forces_a_checkpoint_reconnect() {
    let exit = AttachExit {
      reason: AttachExitReason::ConnectionClosed,
      next_sequence: Some(u64::MAX),
      received_sequence: u64::MAX,
    };
    let safe = serde_json::to_value(AttachmentEventDto::attachment_exited(
      "attachment",
      &exit,
      false,
    ))
    .unwrap();
    let pending = serde_json::to_value(AttachmentEventDto::attachment_exited(
      "attachment",
      &exit,
      true,
    ))
    .unwrap();

    assert_eq!(safe["next_sequence"], u64::MAX.to_string());
    assert!(pending["next_sequence"].is_null());
  }
}
