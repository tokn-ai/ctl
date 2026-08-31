use super::{Arguments, Command, SessionCommand};
use ctl_core::{ConnectionTarget, CoreError, is_retryable_connection_error, open_transport};
use rmux_client::{
  AttachExitReason, AttachRequest, ClientError as RmuxClientError,
  ClientIdentity as RmuxClientIdentity, InteractiveAttachOptions, attach_interactive_with_options,
  begin_attach, current_terminal_size, request, resume_attach,
};
use rmux_proto::{
  ClientMessage, CodecError as RmuxCodecError, CommandSpec, ErrorCode, ServerMessage, SessionInfo,
};
use std::time::Duration;
use std::{env, io};
use thiserror::Error;
use tokio::time::sleep;

const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const INITIAL_RECONNECT_DELAY: Duration = Duration::from_millis(250);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(5);

pub async fn run(arguments: Arguments) -> Result<(), CliError> {
  let target = arguments
    .host
    .map_or_else(ConnectionTarget::local, ConnectionTarget::ssh);
  match arguments.command {
    Command::Session { command } => match command {
      SessionCommand::List => list_sessions(&target).await,
      SessionCommand::New { name, cwd, command } => {
        create_session(&target, name, command_spec(command), cwd).await
      }
      SessionCommand::Kill { session } => kill_session(&target, &session).await,
    },
    Command::Shell {
      session,
      resume_from,
      read_only,
      resize,
    } => shell(&target, &session, resume_from, !read_only, resize).await,
  }
}

async fn list_sessions(target: &ConnectionTarget) -> Result<(), CliError> {
  let sessions = target_sessions(target).await?;
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

async fn create_session(
  target: &ConnectionTarget,
  name: Option<String>,
  command: Option<CommandSpec>,
  working_directory: Option<String>,
) -> Result<(), CliError> {
  let session = create_target_session(target, name, command, working_directory).await?;
  println!("{}\t{}", session.session_id, session.name);
  Ok(())
}

async fn kill_session(target: &ConnectionTarget, session: &str) -> Result<(), CliError> {
  let response = target_request(
    target,
    ClientMessage::KillSession {
      session: session.into(),
    },
  )
  .await?;
  match response {
    ServerMessage::Success => {
      println!("terminated {session}");
      Ok(())
    }
    response => Err(unexpected("success", &response)),
  }
}

async fn shell(
  target: &ConnectionTarget,
  session: &str,
  initial_resume_from: Option<u64>,
  request_input_lease: bool,
  request_layout_lease: bool,
) -> Result<(), CliError> {
  ensure_session(target, session).await?;

  let rmux_identity = rmux_identity();
  let mut resume_from = initial_resume_from;
  let mut reconnect_delay = INITIAL_RECONNECT_DELAY;
  let mut recover_leases_after_connection_loss = false;
  let mut attachment_token = None;

  loop {
    let stream = match open_transport(target).await {
      Ok(stream) => stream,
      Err(error) if is_retryable_connection_error(&error) => {
        wait_to_reconnect(&mut reconnect_delay, &error).await;
        continue;
      }
      Err(error) => return Err(error.into()),
    };
    let request = AttachRequest {
      session: session.into(),
      resume_from,
      terminal_size: current_terminal_size(),
      request_input_lease,
      request_layout_lease,
      // `ctl shell` is a raw PTY presentation and deliberately does not
      // request sensitive editable command buffers or command summaries it
      // cannot render.
      request_command_line: false,
      request_running_command: false,
    };
    let attachment = if let Some(token) = attachment_token.clone() {
      resume_attach(stream, &rmux_identity, token, request).await
    } else {
      begin_attach(stream, &rmux_identity, request).await
    };
    let (stream, attached) = match attachment {
      Ok(attachment) => attachment,
      Err(RmuxClientError::Server {
        code: ErrorCode::AttachmentResumeRejected,
        ..
      }) => {
        attachment_token = None;
        recover_leases_after_connection_loss = true;
        continue;
      }
      Err(error) if is_retryable_rmux_error(&error) => {
        wait_to_reconnect(&mut reconnect_delay, &error).await;
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
          eprintln!("ctl: detached from {}:{session}", target.label());
          return Ok(());
        }
        AttachExitReason::SessionEnded { exit_code } => {
          eprintln!(
            "ctl: {}:{session} ended with exit code {exit_code:?}; not reconnecting",
            target.label()
          );
          return Ok(());
        }
        AttachExitReason::ConnectionClosed => {
          resume_from = exit.next_sequence;
          recover_leases_after_connection_loss = true;
          let reason = if target.is_local() {
            "local connection closed"
          } else {
            "SSH connection closed"
          };
          wait_to_reconnect(&mut reconnect_delay, reason).await;
        }
      },
      Err(error) if is_retryable_rmux_error(&error) => {
        recover_leases_after_connection_loss = true;
        wait_to_reconnect(&mut reconnect_delay, &error).await;
      }
      Err(error) => return Err(error.into()),
    }
  }
}

async fn ensure_session(target: &ConnectionTarget, session: &str) -> Result<(), CliError> {
  if target_sessions(target)
    .await?
    .iter()
    .any(|candidate| candidate.name == session || candidate.session_id == session)
  {
    return Ok(());
  }

  match create_target_session(target, Some(session.into()), None, None).await {
    Ok(created) => {
      eprintln!("ctl: created session {}", created.name);
      Ok(())
    }
    Err(CliError::Rmux(RmuxClientError::Server {
      code: ErrorCode::SessionAlreadyExists,
      ..
    })) => Ok(()),
    Err(error) => Err(error),
  }
}

async fn target_sessions(target: &ConnectionTarget) -> Result<Vec<SessionInfo>, CliError> {
  match target_request(target, ClientMessage::ListSessions).await? {
    ServerMessage::SessionList { sessions } => Ok(sessions),
    response => Err(unexpected("session_list", &response)),
  }
}

async fn create_target_session(
  target: &ConnectionTarget,
  name: Option<String>,
  command: Option<CommandSpec>,
  working_directory: Option<String>,
) -> Result<SessionInfo, CliError> {
  let working_directory = target_working_directory(target, working_directory)?;
  let response = target_request(
    target,
    ClientMessage::CreateSession {
      name,
      command,
      working_directory,
      terminal_size: current_terminal_size(),
    },
  )
  .await?;
  match response {
    ServerMessage::SessionCreated { session } => Ok(session),
    response => Err(unexpected("session_created", &response)),
  }
}

async fn target_request(
  target: &ConnectionTarget,
  message: ClientMessage,
) -> Result<ServerMessage, CliError> {
  let stream = open_transport(target).await?;
  Ok(request(stream, &rmux_identity(), message).await?)
}

fn target_working_directory(
  target: &ConnectionTarget,
  requested: Option<String>,
) -> Result<Option<String>, CliError> {
  if requested.is_some() || !target.is_local() {
    return Ok(requested);
  }
  let directory = env::current_dir().map_err(CliError::CurrentDirectory)?;
  directory
    .into_os_string()
    .into_string()
    .map(Some)
    .map_err(CliError::NonUtf8CurrentDirectory)
}

fn rmux_identity() -> RmuxClientIdentity {
  RmuxClientIdentity {
    name: "ctl".into(),
    version: CLIENT_VERSION.into(),
  }
}

fn command_spec(command: Vec<String>) -> Option<CommandSpec> {
  let mut command = command.into_iter();
  let program = command.next()?;
  Some(CommandSpec {
    program,
    arguments: command.collect(),
  })
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

fn is_retryable_rmux_error(error: &RmuxClientError) -> bool {
  match error {
    RmuxClientError::UnexpectedEof => true,
    RmuxClientError::Codec(RmuxCodecError::Io(source)) => is_retryable_io_error(source),
    _ => false,
  }
}

fn is_retryable_io_error(error: &io::Error) -> bool {
  !matches!(
    error.kind(),
    io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput | io::ErrorKind::PermissionDenied
  )
}

async fn wait_to_reconnect(delay: &mut Duration, reason: impl std::fmt::Display) {
  eprintln!(
    "ctl: connection interrupted ({reason}); reconnecting in {} ms",
    delay.as_millis()
  );
  sleep(*delay).await;
  *delay = delay.saturating_mul(2).min(MAX_RECONNECT_DELAY);
}

fn unexpected(expected: &'static str, response: &ServerMessage) -> CliError {
  CliError::UnexpectedResponse {
    expected,
    actual: format!("{response:?}"),
  }
}

#[derive(Debug, Error)]
pub enum CliError {
  #[error("could not determine the current working directory: {0}")]
  CurrentDirectory(io::Error),
  #[error("the current working directory is not valid UTF-8: {0:?}")]
  NonUtf8CurrentDirectory(std::ffi::OsString),
  #[error(transparent)]
  Control(#[from] CoreError),
  #[error(transparent)]
  Rmux(#[from] RmuxClientError),
  #[error("expected {expected}, received {actual}")]
  UnexpectedResponse {
    expected: &'static str,
    actual: String,
  },
}
