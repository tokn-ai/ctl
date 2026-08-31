use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use rmux_client::{AttachExit, AttachExitReason, AttachedSession, AttachmentControl};
use rmux_proto::{
  ErrorCode, LeaseKind, LeaseStatus, PromptPhase, SessionInfo, SessionStatus, ShellState,
  ShellType, TerminalCheckpoint, TerminalSize, TuiHint,
};
use serde::{Deserialize, Serialize};

use crate::error::{CommandErrorDto, CommandResult};

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
  pub session_id: String,
  pub name: String,
  pub status: SessionStatusDto,
  pub next_sequence: String,
  pub terminal_size: TerminalSizeDto,
}

impl From<SessionInfo> for SessionDto {
  fn from(value: SessionInfo) -> Self {
    Self {
      session_id: value.session_id,
      name: value.name,
      status: value.status.into(),
      next_sequence: value.next_sequence.to_string(),
      terminal_size: value.terminal_size.into(),
    }
  }
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

/// Privacy-preserving shell state for the GUI status bar.
///
/// Editable command-line data is deliberately absent from this DTO and the
/// attachment never requests it from `rmuxd`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShellStateDto {
  pub revision: String,
  pub observed_sequence: String,
  pub shell_type: ShellTypeDto,
  pub cwd: Option<String>,
  pub prompt_phase: PromptPhaseDto,
  pub tui_hint: TuiHintDto,
}

impl From<ShellState> for ShellStateDto {
  fn from(value: ShellState) -> Self {
    Self {
      revision: value.revision.to_string(),
      observed_sequence: value.observed_sequence.to_string(),
      shell_type: value.shell.shell_type.into(),
      cwd: value.cwd,
      prompt_phase: value.prompt_phase.into(),
      tui_hint: value.tui_hint.into(),
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CreateSessionRequestDto {
  pub name: Option<String>,
  pub working_directory: Option<String>,
  pub terminal_size: TerminalSizeDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct OpenAttachmentRequestDto {
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
  pub fn new(attachment_id: String, attached: &AttachedSession) -> Self {
    Self {
      attachment_id,
      session: attached.session.clone().into(),
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
    history_gap: bool,
  ) -> Self {
    Self::Checkpoint {
      attachment_id: attachment_id.into(),
      event_id,
      checkpoint: checkpoint.into(),
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
      code: error_code_name(code).into(),
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

fn error_code_name(code: &ErrorCode) -> &'static str {
  match code {
    ErrorCode::InvalidRequest => "invalid_request",
    ErrorCode::InvalidSessionName => "invalid_session_name",
    ErrorCode::ProtocolVersionMismatch => "protocol_version_mismatch",
    ErrorCode::SequenceAhead => "sequence_ahead",
    ErrorCode::SessionAlreadyExists => "session_already_exists",
    ErrorCode::SessionNotFound => "session_not_found",
    ErrorCode::InputLeaseRequired => "input_lease_required",
    ErrorCode::LayoutLeaseRequired => "layout_lease_required",
    ErrorCode::Internal => "internal",
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn session_u64_values_are_decimal_strings() {
    let dto: SessionDto = SessionInfo {
      session_id: "session".into(),
      name: "large".into(),
      status: SessionStatus::Running,
      created_at_ms: u64::MAX,
      next_sequence: u64::MAX,
      terminal_size: TerminalSize::default(),
    }
    .into();
    let json = serde_json::to_value(dto).unwrap();
    assert_eq!(json["next_sequence"], u64::MAX.to_string());
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
  fn shell_state_dto_never_serializes_the_command_line() {
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
