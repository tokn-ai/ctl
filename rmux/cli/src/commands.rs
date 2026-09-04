use super::{Command, ShellCommand, command_spec, shell};
use rmux_client::{
  AttachExitReason, AttachRequest, ClientError as ProtocolError, ClientIdentity,
  DEFAULT_PRESENTATION_WINDOW_BYTES, InteractiveAttachOptions, attach_interactive_with_options,
  begin_attach, current_terminal_size, get_shell_state, request, resume_attach,
};
use rmux_ipc::Stream;
use rmux_proto::{
  ClientMessage, CodecError, CommandSpec, ErrorCode, PromptPhase, ServerMessage, SessionInfo,
  ShellType, TuiHint,
};
use std::error::Error;
use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::time::sleep;

const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const INITIAL_RECONNECT_DELAY: Duration = Duration::from_millis(250);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(5);

pub type ConnectFuture<'a, Stream, ConnectError> =
  Pin<Box<dyn Future<Output = Result<Stream, ConnectError>> + Send + 'a>>;

/// Supplies a fresh raw `rmux-proto` stream without owning rmux commands.
pub trait Connector {
  type Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static;
  type Error: Error + Send + Sync + 'static;

  fn connect(&self) -> ConnectFuture<'_, Self::Stream, Self::Error>;

  fn is_retryable(&self, error: &Self::Error) -> bool;
  fn is_local(&self) -> bool;
  fn label(&self) -> &str;
  fn connection_kind(&self) -> &'static str;
  fn client_name(&self) -> &'static str;
  fn status_prefix(&self) -> &'static str;
}

/// Connector used by the standalone local `rmux` executable.
#[derive(Debug, Clone)]
pub struct LocalConnector {
  socket_path: PathBuf,
}

impl LocalConnector {
  #[must_use]
  pub fn new(socket_path: PathBuf) -> Self {
    Self { socket_path }
  }
}

impl Connector for LocalConnector {
  type Stream = Stream;
  type Error = rmux_ipc::ConnectError;

  fn connect(&self) -> ConnectFuture<'_, Self::Stream, Self::Error> {
    Box::pin(rmux_ipc::connect_or_start_daemon(&self.socket_path))
  }

  fn is_retryable(&self, error: &Self::Error) -> bool {
    error.is_endpoint_unavailable()
  }

  fn is_local(&self) -> bool {
    true
  }

  fn label(&self) -> &'static str {
    "local"
  }

  fn connection_kind(&self) -> &'static str {
    "local"
  }

  fn client_name(&self) -> &'static str {
    "rmux"
  }

  fn status_prefix(&self) -> &'static str {
    "rmux"
  }
}

/// Runs one canonical rmux command through the supplied transport connector.
///
/// # Errors
///
/// Returns an error when the target cannot be reached, the protocol exchange
/// fails, or local command input such as the current directory is invalid.
pub async fn run<C>(command: Command, connector: &C) -> Result<(), CommandError>
where
  C: Connector,
{
  match command {
    Command::New { name, cwd, command } => {
      create_session(connector, name, command_spec(command), cwd).await
    }
    Command::List => list_sessions(connector).await,
    Command::State { session } => show_shell_state(connector, &session).await,
    Command::Attach {
      session,
      resume_from,
      read_only,
      resize,
    } => attach_session(connector, &session, resume_from, !read_only, resize).await,
    Command::Kill { session } => kill_session(connector, &session).await,
    Command::Shell {
      command: ShellCommand::Init { shell: shell_kind },
    } => {
      print!("{}", shell::init_script(shell_kind.into()));
      Ok(())
    }
  }
}

async fn create_session<C: Connector>(
  connector: &C,
  name: Option<String>,
  command: Option<CommandSpec>,
  working_directory: Option<String>,
) -> Result<(), CommandError> {
  let working_directory = target_working_directory(connector, working_directory)?;
  let response = target_request(
    connector,
    ClientMessage::CreateSession {
      name,
      command,
      working_directory,
      terminal_size: current_terminal_size(),
    },
  )
  .await?;

  match response {
    ServerMessage::SessionCreated { session } => {
      println!("{}\t{}", session.session_id, session.name);
      Ok(())
    }
    response => Err(unexpected("session_created", &response)),
  }
}

async fn list_sessions<C: Connector>(connector: &C) -> Result<(), CommandError> {
  match target_request(connector, ClientMessage::ListSessions).await? {
    ServerMessage::SessionList { sessions } => {
      if sessions.is_empty() {
        println!("no sessions");
        return Ok(());
      }

      println!("NAME\tID\tSTATUS\tSIZE\tNEXT_SEQUENCE");
      for session in &sessions {
        print_session(session);
      }
      Ok(())
    }
    response => Err(unexpected("session_list", &response)),
  }
}

async fn show_shell_state<C: Connector>(connector: &C, session: &str) -> Result<(), CommandError> {
  let stream = connect(connector).await?;
  let state = get_shell_state(stream, &client_identity(connector), session).await?;
  let command_line = if state.shell_state.command_line_redacted {
    "redacted"
  } else if state.shell_state.current_command_line.is_some() {
    // State inspection reports availability without disclosing typed input.
    "available (not displayed)"
  } else {
    "none"
  };
  let running_command = if state.shell_state.running_command_redacted {
    "redacted"
  } else if state.shell_state.running_command.is_some() {
    // Keep this defensive branch non-disclosing if daemon policy changes.
    "available (not displayed)"
  } else {
    "none"
  };

  println!("SESSION\t{}", state.session.name);
  println!(
    "SHELL\t{}",
    shell_type_name(state.shell_state.shell.shell_type)
  );
  println!(
    "CWD\t{}",
    state
      .shell_state
      .displayed_cwd()
      .map_or_else(|| "-".into(), escaped_for_terminal)
  );
  println!(
    "PROMPT_PHASE\t{}",
    prompt_phase_name(state.shell_state.prompt_phase)
  );
  println!("TUI_HINT\t{}", tui_hint_name(state.shell_state.tui_hint));
  println!("COMMAND_LINE\t{command_line}");
  println!("RUNNING_COMMAND\t{running_command}");
  println!("REVISION\t{}", state.shell_state.revision);
  println!("OBSERVED_SEQUENCE\t{}", state.shell_state.observed_sequence);
  Ok(())
}

async fn kill_session<C: Connector>(connector: &C, session: &str) -> Result<(), CommandError> {
  match target_request(
    connector,
    ClientMessage::KillSession {
      session: session.into(),
    },
  )
  .await?
  {
    ServerMessage::Success => Ok(()),
    response => Err(unexpected("success", &response)),
  }
}

async fn attach_session<C: Connector>(
  connector: &C,
  session: &str,
  initial_resume_from: Option<u64>,
  request_input_lease: bool,
  request_layout_lease: bool,
) -> Result<(), CommandError> {
  let identity = client_identity(connector);
  let mut resume_from = initial_resume_from;
  let mut reconnect_delay = INITIAL_RECONNECT_DELAY;
  let mut recover_leases_after_connection_loss = false;
  let mut attachment_token = None;

  loop {
    let stream = match connector.connect().await {
      Ok(stream) => stream,
      Err(error) if connector.is_retryable(&error) => {
        wait_to_reconnect(connector.status_prefix(), &mut reconnect_delay, &error).await;
        continue;
      }
      Err(error) => return Err(connection_error(error)),
    };
    let request = AttachRequest {
      session: session.into(),
      resume_from,
      terminal_size: current_terminal_size(),
      request_input_lease,
      request_layout_lease,
      request_command_line: false,
      request_running_command: false,
      presentation_window_bytes: DEFAULT_PRESENTATION_WINDOW_BYTES,
    };
    let attachment = if let Some(token) = attachment_token.clone() {
      resume_attach(stream, &identity, token, request).await
    } else {
      begin_attach(stream, &identity, request).await
    };
    let (stream, attached) = match attachment {
      Ok(attachment) => attachment,
      Err(ProtocolError::Server {
        code: ErrorCode::AttachmentResumeRejected,
        ..
      }) => {
        attachment_token = None;
        recover_leases_after_connection_loss = true;
        continue;
      }
      Err(error) if is_retryable_protocol_error(&error) => {
        wait_to_reconnect(connector.status_prefix(), &mut reconnect_delay, &error).await;
        continue;
      }
      Err(error) => return Err(error.into()),
    };
    attachment_token = Some(attached.attachment_token.clone());

    let interactive_options = if recover_leases_after_connection_loss {
      InteractiveAttachOptions {
        reacquire_input_lease: request_input_lease && !attached.input_lease.owned_by_client,
        reacquire_layout_lease: request_layout_lease && !attached.layout_lease.owned_by_client,
        resize_after_layout_reacquire: request_layout_lease
          && !attached.layout_lease.owned_by_client,
      }
    } else {
      InteractiveAttachOptions::default()
    };
    reconnect_delay = INITIAL_RECONNECT_DELAY;
    match attach_interactive_with_options(stream, &attached, interactive_options).await {
      Ok(exit) => match exit.reason {
        AttachExitReason::Detached => {
          eprintln!(
            "{}: detached from {}:{session}",
            connector.status_prefix(),
            connector.label()
          );
          return Ok(());
        }
        AttachExitReason::SessionEnded { exit_code } => {
          eprintln!(
            "{}: {}:{session} ended with exit code {exit_code:?}; not reconnecting",
            connector.status_prefix(),
            connector.label()
          );
          return Ok(());
        }
        AttachExitReason::ConnectionClosed => {
          resume_from = exit.next_sequence;
          recover_leases_after_connection_loss = true;
          wait_to_reconnect(
            connector.status_prefix(),
            &mut reconnect_delay,
            format!("{} connection closed", connector.connection_kind()),
          )
          .await;
        }
      },
      Err(error) if is_retryable_protocol_error(&error) => {
        recover_leases_after_connection_loss = true;
        wait_to_reconnect(connector.status_prefix(), &mut reconnect_delay, &error).await;
      }
      Err(error) => return Err(error.into()),
    }
  }
}

async fn target_request<C: Connector>(
  connector: &C,
  message: ClientMessage,
) -> Result<ServerMessage, CommandError> {
  let stream = connect(connector).await?;
  Ok(request(stream, &client_identity(connector), message).await?)
}

async fn connect<C: Connector>(connector: &C) -> Result<C::Stream, CommandError> {
  connector.connect().await.map_err(connection_error)
}

fn target_working_directory<C: Connector>(
  connector: &C,
  requested: Option<String>,
) -> Result<Option<String>, CommandError> {
  if requested.is_some() || !connector.is_local() {
    return Ok(requested);
  }
  let directory = std::env::current_dir().map_err(CommandError::CurrentDirectory)?;
  directory
    .into_os_string()
    .into_string()
    .map(Some)
    .map_err(CommandError::NonUtf8CurrentDirectory)
}

fn client_identity<C: Connector>(connector: &C) -> ClientIdentity {
  ClientIdentity {
    name: connector.client_name().into(),
    version: CLIENT_VERSION.into(),
  }
}

fn print_session(session: &SessionInfo) {
  println!(
    "{}\t{}\t{:?}\t{}x{}\t{}",
    session.name,
    session.session_id,
    session.status,
    session.terminal_size.columns,
    session.terminal_size.rows,
    session.next_sequence
  );
}

fn shell_type_name(shell_type: ShellType) -> &'static str {
  match shell_type {
    ShellType::Bash => "bash",
    ShellType::Fish => "fish",
    ShellType::Pwsh => "pwsh",
    ShellType::Zsh => "zsh",
    ShellType::Cmd => "cmd",
    ShellType::Sh => "sh",
    ShellType::Unknown => "unknown",
  }
}

fn prompt_phase_name(prompt_phase: PromptPhase) -> &'static str {
  match prompt_phase {
    PromptPhase::Unknown => "unknown",
    PromptPhase::AtPrompt => "at_prompt",
    PromptPhase::Editing => "editing",
    PromptPhase::Running => "running",
  }
}

fn tui_hint_name(tui_hint: TuiHint) -> &'static str {
  match tui_hint {
    TuiHint::Unknown => "unknown",
    TuiHint::Inline => "inline",
    TuiHint::AlternateScreen => "alternate_screen",
  }
}

fn escaped_for_terminal(value: &str) -> String {
  value.escape_default().to_string()
}

fn is_retryable_protocol_error(error: &ProtocolError) -> bool {
  match error {
    ProtocolError::UnexpectedEof => true,
    ProtocolError::Codec(CodecError::Io(source)) => !matches!(
      source.kind(),
      io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput | io::ErrorKind::PermissionDenied
    ),
    _ => false,
  }
}

async fn wait_to_reconnect(
  status_prefix: &str,
  delay: &mut Duration,
  reason: impl std::fmt::Display,
) {
  eprintln!(
    "{status_prefix}: connection interrupted ({reason}); reconnecting in {} ms",
    delay.as_millis()
  );
  sleep(*delay).await;
  *delay = delay.saturating_mul(2).min(MAX_RECONNECT_DELAY);
}

fn unexpected(expected: &'static str, response: &ServerMessage) -> CommandError {
  CommandError::UnexpectedResponse {
    expected,
    actual: format!("{response:?}"),
  }
}

fn connection_error(error: impl Error + Send + Sync + 'static) -> CommandError {
  CommandError::Connection(Box::new(error))
}

#[derive(Debug, Error)]
pub enum CommandError {
  #[error("transport connection failed: {0}")]
  Connection(#[source] Box<dyn Error + Send + Sync>),
  #[error("could not determine the current working directory: {0}")]
  CurrentDirectory(io::Error),
  #[error("the current working directory is not valid UTF-8: {0:?}")]
  NonUtf8CurrentDirectory(std::ffi::OsString),
  #[error(transparent)]
  Protocol(#[from] ProtocolError),
  #[error("expected {expected}, received {actual}")]
  UnexpectedResponse {
    expected: &'static str,
    actual: String,
  },
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn local_connector_preserves_the_standalone_identity() {
    let connector = LocalConnector::new(std::path::Path::new("/tmp/rmux.sock").into());
    assert!(connector.is_local());
    assert_eq!(connector.client_name(), "rmux");
    assert_eq!(connector.status_prefix(), "rmux");
  }
}
