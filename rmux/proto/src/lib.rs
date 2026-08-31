use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const PROTOCOL_VERSION: u16 = 7;
pub const MAX_FRAME_SIZE: usize = 8 * 1024 * 1024;
pub const TERMINAL_CHECKPOINT_FORMAT: &str = "rmux_vt_state";
pub const TERMINAL_CHECKPOINT_FORMAT_VERSION: u16 = 1;
/// Maximum UTF-8 byte length of a shell-reported running-command summary.
///
/// This is intentionally much smaller than the editable command-line bound:
/// it is presentation metadata for titles, not an alternate command buffer.
pub const MAX_RUNNING_COMMAND_BYTES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSize {
  pub columns: u16,
  pub rows: u16,
  pub pixel_width: u16,
  pub pixel_height: u16,
}

impl Default for TerminalSize {
  fn default() -> Self {
    Self {
      columns: 80,
      rows: 24,
      pixel_width: 0,
      pixel_height: 0,
    }
  }
}

/// An independently controlled capability of an attached terminal client.
///
/// Input and PTY layout intentionally have separate owners. For example, a
/// phone may view and type into a desktop-sized session without changing its
/// layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseKind {
  Input,
  Layout,
}

/// Lease state as observed by one attached client.
///
/// The daemon deliberately does not expose another attachment's identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseStatus {
  pub held: bool,
  pub owned_by_client: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalCheckpoint {
  pub format: String,
  pub format_version: u16,
  pub sequence: u64,
  pub terminal_size: TerminalSize,
  pub payload: Vec<u8>,
  pub input_prefix: Vec<u8>,
}

impl TerminalCheckpoint {
  #[must_use]
  pub fn is_supported(&self) -> bool {
    self.format == TERMINAL_CHECKPOINT_FORMAT
      && self.format_version == TERMINAL_CHECKPOINT_FORMAT_VERSION
  }
}

/// The shell program that most recently reported session awareness metadata.
///
/// `Unknown` means no supported shell integration has reported its identity;
/// it does not attempt to infer a shell from rendered terminal output.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellType {
  Bash,
  Fish,
  Pwsh,
  Zsh,
  Cmd,
  Sh,
  #[default]
  Unknown,
}

/// Shell-awareness features advertised by a shell integration.
///
/// A capability only says that the integration can report a value. The value
/// may still be absent when it is not meaningful, unavailable, or withheld by
/// a daemon visibility policy.
#[allow(
  clippy::struct_excessive_bools,
  reason = "the wire format deliberately exposes independent named capabilities"
)]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellCapabilities {
  pub reports_cwd: bool,
  pub reports_command_line: bool,
  pub reports_cursor: bool,
  pub reports_prompt_phase: bool,
  /// The integration can report a bounded, non-editable command summary
  /// while the shell is waiting for that command to finish.
  #[serde(default)]
  pub reports_running_command: bool,
}

/// Describes the shell integration currently associated with a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellDescriptor {
  pub shell_type: ShellType,
  /// Version of the shell-integration report format, if one reported it.
  pub integration_version: Option<u16>,
  pub capabilities: ShellCapabilities,
}

impl Default for ShellDescriptor {
  fn default() -> Self {
    Self {
      shell_type: ShellType::Unknown,
      integration_version: None,
      capabilities: ShellCapabilities::default(),
    }
  }
}

/// The shell's high-level interaction phase.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptPhase {
  /// No supported integration has reported a prompt phase.
  #[default]
  Unknown,
  /// The shell is ready at an empty prompt.
  AtPrompt,
  /// The shell is accepting an editable command line.
  Editing,
  /// The shell has accepted a command and is waiting for it to complete.
  Running,
}

/// The current editable shell command line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandLine {
  pub text: String,
  /// Optional Unicode scalar-value offset into [`Self::text`].
  pub cursor_scalar_offset: Option<u32>,
}

impl CommandLine {
  /// Returns whether `cursor_scalar_offset`, when present, is within
  /// [`Self::text`].
  #[must_use]
  pub fn has_valid_cursor(&self) -> bool {
    let Some(cursor_scalar_offset) = self.cursor_scalar_offset else {
      return true;
    };
    let Ok(cursor_scalar_offset) = usize::try_from(cursor_scalar_offset) else {
      return false;
    };

    cursor_scalar_offset <= self.text.chars().count()
  }
}

/// A presentation hint derived from terminal alternate-screen state.
///
/// This is not an authoritative classification of an application. Some TUIs
/// do not use the alternate screen, and some non-TUI programs do.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TuiHint {
  /// The terminal parser has not established an alternate-screen state yet.
  #[default]
  Unknown,
  /// The terminal is using its normal inline screen buffer.
  Inline,
  /// The terminal parser observed the alternate screen buffer as active.
  AlternateScreen,
}

/// Current shell-awareness metadata for one session.
///
/// `observed_sequence` is the raw-output next offset when this state was
/// observed: every raw byte below it has reached the daemon. It is metadata
/// for correlating display state with output, never a resume cursor.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellState {
  /// Monotonically increasing, session-scoped state revision.
  pub revision: u64,
  pub observed_sequence: u64,
  pub shell: ShellDescriptor,
  /// A shell-reported directory display string, not a portable file path.
  pub cwd: Option<String>,
  pub prompt_phase: PromptPhase,
  /// True when a visibility policy deliberately omitted the current command.
  /// When true, `current_command_line` must be `None`.
  pub command_line_redacted: bool,
  /// Omitted when no current editable line is available or visible.
  pub current_command_line: Option<CommandLine>,
  /// True when a visibility policy deliberately omitted the running-command
  /// summary. When true, `running_command` must be `None`.
  #[serde(default)]
  pub running_command_redacted: bool,
  /// A bounded shell-reported command summary while `prompt_phase` is
  /// `running`. This is never an editable buffer.
  #[serde(default)]
  pub running_command: Option<String>,
  pub tui_hint: TuiHint,
}

impl ShellState {
  /// Returns whether the command-line fields obey their privacy and cursor
  /// invariants.
  #[must_use]
  pub fn has_valid_command_line(&self) -> bool {
    match &self.current_command_line {
      Some(command_line) => {
        !self.command_line_redacted
          && matches!(
            self.prompt_phase,
            PromptPhase::AtPrompt | PromptPhase::Editing
          )
          && command_line.has_valid_cursor()
      }
      None => true,
    }
  }

  /// Returns whether the running-command fields obey their privacy,
  /// capability, phase, and bounded-text invariants.
  #[must_use]
  pub fn has_valid_running_command(&self) -> bool {
    match &self.running_command {
      Some(command) => {
        !self.running_command_redacted
          && self.shell.capabilities.reports_running_command
          && self.prompt_phase == PromptPhase::Running
          && is_valid_running_command(command)
      }
      None => true,
    }
  }

  /// Returns whether every shell-awareness field obeys its wire invariants.
  #[must_use]
  pub fn has_valid_metadata(&self) -> bool {
    self.has_valid_command_line() && self.has_valid_running_command()
  }

  /// Returns this snapshot with command metadata filtered for one viewer.
  ///
  /// Visibility is evaluated by the daemon from the attachment's explicit
  /// request and current input lease. The fields remain in every complete
  /// snapshot so clients can distinguish unavailable data from policy
  /// redaction.
  #[must_use]
  pub fn filtered_for_visibility(
    mut self,
    may_view_command_line: bool,
    may_view_running_command: bool,
  ) -> Self {
    self.command_line_redacted = false;
    if !may_view_command_line && self.current_command_line.is_some() {
      self.current_command_line = None;
      self.command_line_redacted = true;
    }
    self.running_command_redacted = false;
    if !may_view_running_command && self.running_command.is_some() {
      self.running_command = None;
      self.running_command_redacted = true;
    }
    self
  }
}

/// Returns whether a running-command title summary is safe to retain.
///
/// The summary is a bounded non-empty UTF-8 string without control
/// characters. It intentionally does not attempt to parse command syntax.
#[must_use]
pub fn is_valid_running_command(command: &str) -> bool {
  !command.is_empty()
    && command.len() <= MAX_RUNNING_COMMAND_BYTES
    && !command.chars().any(char::is_control)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSpec {
  pub program: String,
  pub arguments: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
  Running,
  Exited,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
  pub session_id: String,
  pub name: String,
  pub status: SessionStatus,
  pub created_at_ms: u64,
  pub next_sequence: u64,
  pub terminal_size: TerminalSize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
  Handshake {
    protocol_version: u16,
    client_name: String,
    client_version: String,
  },
  CreateSession {
    name: Option<String>,
    command: Option<CommandSpec>,
    working_directory: Option<String>,
    terminal_size: TerminalSize,
  },
  ListSessions,
  /// Retrieves the latest shell-awareness state without creating an
  /// attachment. Command-line visibility is subject to daemon policy.
  GetShellState {
    session: String,
  },
  AttachSession {
    session: String,
    resume_from: Option<u64>,
    terminal_size: TerminalSize,
    /// Claim input if no other attached client currently owns it. This never
    /// takes the lease from another client.
    request_input_lease: bool,
    /// Claim PTY layout ownership if unheld. A successful request applies the
    /// terminal size in this attach request as an explicit resize.
    request_layout_lease: bool,
    /// Request the current editable command line in shell-awareness state.
    /// The daemon may redact it according to its visibility policy.
    request_command_line: bool,
    /// Request the current running-command summary in shell-awareness state.
    /// The daemon may redact it according to its visibility policy.
    #[serde(default)]
    request_running_command: bool,
  },
  /// Rebinds a new transport to a recently disconnected logical attachment.
  ///
  /// The opaque token is issued only in `attached`, remains memory-only, and
  /// preserves the attachment's existing input/layout lease ownership.
  ResumeAttachment {
    session: String,
    attachment_token: String,
    resume_from: Option<u64>,
    terminal_size: TerminalSize,
    request_command_line: bool,
    #[serde(default)]
    request_running_command: bool,
  },
  KillSession {
    session: String,
  },
  Input {
    data: Vec<u8>,
  },
  Resize {
    terminal_size: TerminalSize,
  },
  AcquireLease {
    lease: LeaseKind,
  },
  ReleaseLease {
    lease: LeaseKind,
  },
  /// Confirms that an attached client is still reachable without affecting
  /// terminal input or layout ownership.
  Heartbeat {
    nonce: u64,
  },
  Detach,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
  InvalidRequest,
  InvalidSessionName,
  ProtocolVersionMismatch,
  SequenceAhead,
  SessionAlreadyExists,
  SessionNotFound,
  AttachmentResumeRejected,
  InputLeaseRequired,
  LayoutLeaseRequired,
  Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
  HandshakeAccepted {
    protocol_version: u16,
    server_version: String,
    /// Suggested cadence for attached-client heartbeats.
    heartbeat_interval_ms: u64,
    /// Maximum interval without client activity before an attachment expires.
    attachment_liveness_timeout_ms: u64,
  },
  SessionCreated {
    session: SessionInfo,
  },
  SessionList {
    sessions: Vec<SessionInfo>,
  },
  ShellStateResponse {
    session: SessionInfo,
    shell_state: ShellState,
  },
  Attached {
    /// Opaque, memory-only credential for reconnecting this logical
    /// attachment through a replacement transport.
    attachment_token: String,
    session: SessionInfo,
    earliest_sequence: u64,
    next_sequence: u64,
    replay_from: u64,
    history_gap: bool,
    checkpoint: Option<TerminalCheckpoint>,
    terminal_size_mismatch: bool,
    input_lease: LeaseStatus,
    layout_lease: LeaseStatus,
    /// Complete shell-awareness state at attach time. An unsupported shell
    /// produces the default unknown state rather than omitting this snapshot.
    shell_state: ShellState,
  },
  /// Confirms that the daemon processed an explicit detach and released the
  /// logical attachment without reconnect grace.
  Detached,
  LeaseStatus {
    lease: LeaseKind,
    status: LeaseStatus,
  },
  /// Echoes an attached client's heartbeat nonce.
  HeartbeatAck {
    nonce: u64,
  },
  /// Replaces the attached client's shell-awareness state with a newer,
  /// complete session snapshot.
  ShellStateChanged {
    state: ShellState,
  },
  /// Reports an authoritative PTY geometry transition to every attachment.
  ///
  /// `observed_sequence` is the raw-output next offset at the transition.
  /// The daemon sends this after every output byte below the offset and before
  /// any output byte at or above it. This changes a terminal renderer's grid;
  /// it neither grants layout ownership nor changes a client's viewport.
  PtyGeometryChanged {
    terminal_size: TerminalSize,
    observed_sequence: u64,
  },
  Checkpoint {
    checkpoint: TerminalCheckpoint,
    history_gap: bool,
  },
  Output {
    sequence_start: u64,
    sequence_end: u64,
    data: Vec<u8>,
  },
  SessionEnded {
    session_id: String,
    exit_code: Option<u32>,
  },
  Success,
  Error {
    code: ErrorCode,
    message: String,
  },
}

#[derive(Debug, Error)]
pub enum CodecError {
  #[error("I/O error: {0}")]
  Io(#[from] io::Error),
  #[error("frame length {actual} exceeds the maximum of {maximum} bytes")]
  FrameTooLarge { actual: usize, maximum: usize },
  #[error("invalid JSON frame: {0}")]
  Json(#[from] serde_json::Error),
}

/// Serializes and writes one length-prefixed protocol frame.
///
/// # Errors
///
/// Returns an error when serialization fails, the encoded message is too
/// large, or the transport cannot be written or flushed.
pub async fn write_frame<W, T>(writer: &mut W, message: &T) -> Result<(), CodecError>
where
  W: AsyncWrite + Unpin,
  T: Serialize,
{
  let payload = serde_json::to_vec(message)?;
  if payload.len() > MAX_FRAME_SIZE {
    return Err(CodecError::FrameTooLarge {
      actual: payload.len(),
      maximum: MAX_FRAME_SIZE,
    });
  }

  #[allow(clippy::cast_possible_truncation)]
  let length = payload.len() as u32;
  writer.write_all(&length.to_be_bytes()).await?;
  writer.write_all(&payload).await?;
  writer.flush().await?;
  Ok(())
}

/// Reads and deserializes one length-prefixed protocol frame.
///
/// A clean end of stream before a new frame returns `Ok(None)`.
///
/// # Errors
///
/// Returns an error when the transport fails mid-frame, the declared frame is
/// too large, or its payload is not valid JSON for the requested type.
pub async fn read_frame<R, T>(reader: &mut R) -> Result<Option<T>, CodecError>
where
  R: AsyncRead + Unpin,
  T: DeserializeOwned,
{
  let mut length_bytes = [0_u8; 4];
  match reader.read(&mut length_bytes[..1]).await {
    Ok(0) => return Ok(None),
    Ok(_) => {
      reader.read_exact(&mut length_bytes[1..]).await?;
    }
    Err(error) => return Err(error.into()),
  }

  let length = u32::from_be_bytes(length_bytes) as usize;
  if length > MAX_FRAME_SIZE {
    return Err(CodecError::FrameTooLarge {
      actual: length,
      maximum: MAX_FRAME_SIZE,
    });
  }

  let mut payload = vec![0_u8; length];
  reader.read_exact(&mut payload).await?;
  Ok(Some(serde_json::from_slice(&payload)?))
}

#[cfg(test)]
mod tests {
  use super::*;

  fn shell_state() -> ShellState {
    ShellState {
      revision: 7,
      observed_sequence: 123,
      shell: ShellDescriptor {
        shell_type: ShellType::Zsh,
        integration_version: Some(1),
        capabilities: ShellCapabilities {
          reports_cwd: true,
          reports_command_line: true,
          reports_cursor: true,
          reports_prompt_phase: true,
          reports_running_command: false,
        },
      },
      cwd: Some("/work/rmux".into()),
      prompt_phase: PromptPhase::Editing,
      command_line_redacted: false,
      current_command_line: Some(CommandLine {
        text: "cargo test".into(),
        cursor_scalar_offset: Some(10),
      }),
      running_command_redacted: false,
      running_command: None,
      tui_hint: TuiHint::Inline,
    }
  }

  #[tokio::test]
  async fn frames_round_trip() {
    let expected = ClientMessage::AttachSession {
      session: "work".into(),
      resume_from: Some(42),
      terminal_size: TerminalSize::default(),
      request_input_lease: true,
      request_layout_lease: false,
      request_command_line: false,
      request_running_command: true,
    };
    let (mut client, mut server) = tokio::io::duplex(1024);

    let write = tokio::spawn(async move { write_frame(&mut client, &expected).await });
    let actual: ClientMessage = read_frame(&mut server).await.unwrap().unwrap();

    write.await.unwrap().unwrap();
    assert_eq!(
      actual,
      ClientMessage::AttachSession {
        session: "work".into(),
        resume_from: Some(42),
        terminal_size: TerminalSize::default(),
        request_input_lease: true,
        request_layout_lease: false,
        request_command_line: false,
        request_running_command: true,
      }
    );
  }

  #[tokio::test]
  async fn lease_status_frame_round_trips() {
    let expected = ServerMessage::LeaseStatus {
      lease: LeaseKind::Layout,
      status: LeaseStatus {
        held: true,
        owned_by_client: false,
      },
    };
    let (mut server, mut client) = tokio::io::duplex(1024);

    let write = tokio::spawn(async move { write_frame(&mut server, &expected).await });
    let actual: ServerMessage = read_frame(&mut client).await.unwrap().unwrap();

    write.await.unwrap().unwrap();
    assert_eq!(
      actual,
      ServerMessage::LeaseStatus {
        lease: LeaseKind::Layout,
        status: LeaseStatus {
          held: true,
          owned_by_client: false,
        },
      }
    );
  }

  #[tokio::test]
  async fn heartbeat_frames_round_trip() {
    let expected = ClientMessage::Heartbeat { nonce: 42 };
    let (mut client, mut server) = tokio::io::duplex(1024);

    let write = tokio::spawn(async move { write_frame(&mut client, &expected).await });
    let actual: ClientMessage = read_frame(&mut server).await.unwrap().unwrap();

    write.await.unwrap().unwrap();
    assert_eq!(actual, ClientMessage::Heartbeat { nonce: 42 });
  }

  #[tokio::test]
  async fn shell_state_changed_frame_round_trips() {
    let expected = ServerMessage::ShellStateChanged {
      state: shell_state(),
    };
    let (mut server, mut client) = tokio::io::duplex(4096);

    let write = tokio::spawn(async move { write_frame(&mut server, &expected).await });
    let actual: ServerMessage = read_frame(&mut client).await.unwrap().unwrap();

    write.await.unwrap().unwrap();
    assert_eq!(
      actual,
      ServerMessage::ShellStateChanged {
        state: shell_state(),
      }
    );
  }

  #[tokio::test]
  async fn pty_geometry_changed_frame_round_trips() {
    let expected = ServerMessage::PtyGeometryChanged {
      terminal_size: TerminalSize {
        columns: 132,
        rows: 43,
        pixel_width: 1056,
        pixel_height: 860,
      },
      observed_sequence: 456,
    };
    let (mut server, mut client) = tokio::io::duplex(1024);

    let write = tokio::spawn(async move { write_frame(&mut server, &expected).await });
    let actual: ServerMessage = read_frame(&mut client).await.unwrap().unwrap();

    write.await.unwrap().unwrap();
    assert_eq!(
      actual,
      ServerMessage::PtyGeometryChanged {
        terminal_size: TerminalSize {
          columns: 132,
          rows: 43,
          pixel_width: 1056,
          pixel_height: 860,
        },
        observed_sequence: 456,
      }
    );
  }

  #[test]
  fn shell_state_json_uses_stable_snake_case_names() {
    let encoded = serde_json::to_value(ServerMessage::ShellStateChanged {
      state: shell_state(),
    })
    .unwrap();

    assert_eq!(encoded["type"], "shell_state_changed");
    assert_eq!(encoded["state"]["shell"]["shell_type"], "zsh");
    assert_eq!(
      encoded["state"]["current_command_line"]["cursor_scalar_offset"],
      10
    );
    assert_eq!(encoded["state"]["tui_hint"], "inline");
    assert_eq!(encoded["state"]["running_command"], serde_json::Value::Null);
  }

  #[test]
  fn version_six_shell_state_fixture_defaults_running_command_fields() {
    let fixture = r#"
      {
        "type": "shell_state_changed",
        "state": {
          "revision": 7,
          "observed_sequence": 123,
          "shell": {
            "shell_type": "zsh",
            "integration_version": 1,
            "capabilities": {
              "reports_cwd": true,
              "reports_command_line": true,
              "reports_cursor": true,
              "reports_prompt_phase": true
            }
          },
          "cwd": "/work/rmux",
          "prompt_phase": "editing",
          "command_line_redacted": false,
          "current_command_line": {
            "text": "cargo test",
            "cursor_scalar_offset": 10
          },
          "tui_hint": "inline"
        }
      }
    "#;

    let ServerMessage::ShellStateChanged { state } = serde_json::from_str(fixture).unwrap() else {
      panic!("fixture must decode as shell_state_changed");
    };

    assert!(!state.shell.capabilities.reports_running_command);
    assert!(!state.running_command_redacted);
    assert_eq!(state.running_command, None);
  }

  #[test]
  fn legacy_attach_fixture_defaults_running_command_request() {
    let fixture = r#"
      {
        "type": "attach_session",
        "session": "work",
        "resume_from": 42,
        "terminal_size": {
          "columns": 80,
          "rows": 24,
          "pixel_width": 0,
          "pixel_height": 0
        },
        "request_input_lease": true,
        "request_layout_lease": false,
        "request_command_line": true
      }
    "#;

    let ClientMessage::AttachSession {
      request_running_command,
      ..
    } = serde_json::from_str(fixture).unwrap()
    else {
      panic!("fixture must decode as attach_session");
    };

    assert!(!request_running_command);
  }

  #[test]
  fn reconnect_support_uses_protocol_version_seven() {
    assert_eq!(PROTOCOL_VERSION, 7);
  }

  #[test]
  fn resume_attachment_uses_stable_snake_case_fields() {
    let encoded = serde_json::to_value(ClientMessage::ResumeAttachment {
      session: "work".into(),
      attachment_token: "secret".into(),
      resume_from: Some(42),
      terminal_size: TerminalSize::default(),
      request_command_line: false,
      request_running_command: false,
    })
    .unwrap();

    assert_eq!(encoded["type"], "resume_attachment");
    assert_eq!(encoded["attachment_token"], "secret");
    assert_eq!(encoded["resume_from"], 42);
  }

  #[test]
  fn pty_geometry_changed_json_uses_stable_snake_case_names() {
    let encoded = serde_json::to_value(ServerMessage::PtyGeometryChanged {
      terminal_size: TerminalSize {
        columns: 132,
        rows: 43,
        pixel_width: 1056,
        pixel_height: 860,
      },
      observed_sequence: 456,
    })
    .unwrap();

    assert_eq!(encoded["type"], "pty_geometry_changed");
    assert_eq!(encoded["terminal_size"]["columns"], 132);
    assert_eq!(encoded["observed_sequence"], 456);
  }

  #[test]
  fn command_line_cursor_uses_unicode_scalar_offsets() {
    let valid_before_accent = CommandLine {
      text: "café".into(),
      cursor_scalar_offset: Some(3),
    };
    let valid_end = CommandLine {
      text: "café".into(),
      cursor_scalar_offset: Some(4),
    };
    let invalid_after_end = CommandLine {
      text: "café".into(),
      cursor_scalar_offset: Some(5),
    };

    assert!(valid_before_accent.has_valid_cursor());
    assert!(valid_end.has_valid_cursor());
    assert!(!invalid_after_end.has_valid_cursor());
  }

  #[test]
  fn shell_state_rejects_a_command_line_marked_as_redacted() {
    let mut state = shell_state();
    assert!(state.has_valid_command_line());

    state.command_line_redacted = true;
    assert!(!state.has_valid_command_line());

    state.current_command_line = None;
    assert!(state.has_valid_command_line());
  }

  #[test]
  fn shell_state_rejects_a_command_line_while_running() {
    let mut state = shell_state();
    state.prompt_phase = PromptPhase::Running;

    assert!(!state.has_valid_command_line());
  }

  #[test]
  fn shell_state_accepts_a_bounded_running_command_only_while_running() {
    let mut state = shell_state();
    state.prompt_phase = PromptPhase::Running;
    state.current_command_line = None;
    state.shell.capabilities.reports_running_command = true;
    state.running_command = Some("cargo test --workspace".into());

    assert!(state.has_valid_running_command());
    assert!(state.has_valid_metadata());

    state.prompt_phase = PromptPhase::AtPrompt;
    assert!(!state.has_valid_running_command());
  }

  #[test]
  fn shell_state_rejects_invalid_running_command_summaries() {
    let mut state = shell_state();
    state.prompt_phase = PromptPhase::Running;
    state.current_command_line = None;
    state.shell.capabilities.reports_running_command = true;

    state.running_command = Some(String::new());
    assert!(!state.has_valid_running_command());

    state.running_command = Some("cargo\ntest".into());
    assert!(!state.has_valid_running_command());

    state.running_command = Some("x".repeat(MAX_RUNNING_COMMAND_BYTES + 1));
    assert!(!state.has_valid_running_command());
  }

  #[test]
  fn shell_state_rejects_a_running_command_marked_as_redacted() {
    let mut state = shell_state();
    state.prompt_phase = PromptPhase::Running;
    state.current_command_line = None;
    state.shell.capabilities.reports_running_command = true;
    state.running_command = Some("cargo test".into());
    state.running_command_redacted = true;

    assert!(!state.has_valid_running_command());
  }

  #[test]
  fn shell_state_visibility_filters_editing_and_running_text_independently() {
    let mut editing = shell_state();
    let hidden_editing = editing.clone().filtered_for_visibility(false, true);
    assert!(hidden_editing.command_line_redacted);
    assert_eq!(hidden_editing.current_command_line, None);
    assert!(!hidden_editing.running_command_redacted);

    editing.prompt_phase = PromptPhase::Running;
    editing.current_command_line = None;
    editing.shell.capabilities.reports_running_command = true;
    editing.running_command = Some("cargo test".into());
    let hidden_running = editing.clone().filtered_for_visibility(true, false);
    assert!(hidden_running.running_command_redacted);
    assert_eq!(hidden_running.running_command, None);
    assert!(!hidden_running.command_line_redacted);

    let visible_running = editing.filtered_for_visibility(false, true);
    assert!(!visible_running.running_command_redacted);
    assert_eq!(
      visible_running.running_command.as_deref(),
      Some("cargo test")
    );
  }

  #[test]
  fn default_shell_state_is_an_explicit_unknown_snapshot() {
    assert_eq!(
      ShellState::default(),
      ShellState {
        revision: 0,
        observed_sequence: 0,
        shell: ShellDescriptor::default(),
        cwd: None,
        prompt_phase: PromptPhase::Unknown,
        command_line_redacted: false,
        current_command_line: None,
        running_command_redacted: false,
        running_command: None,
        tui_hint: TuiHint::Unknown,
      }
    );
  }

  #[tokio::test]
  async fn clean_eof_returns_none() {
    let (client, mut server) = tokio::io::duplex(16);
    drop(client);

    let message: Option<ClientMessage> = read_frame(&mut server).await.unwrap();
    assert!(message.is_none());
  }

  #[tokio::test]
  async fn truncated_header_is_an_error() {
    let (mut client, mut server) = tokio::io::duplex(16);
    client.write_all(&[0, 0]).await.unwrap();
    drop(client);

    let result = read_frame::<_, ClientMessage>(&mut server).await;
    assert!(matches!(
      result,
      Err(CodecError::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof
    ));
  }
}
