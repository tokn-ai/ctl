use super::{Arguments, Command, SessionCommand};
use ctl_core::{CoreError, is_retryable_connection_error, open_ssh_tunnel};
use rmux_client::{
  AttachExitReason, AttachRequest, ClientError as RmuxClientError,
  ClientIdentity as RmuxClientIdentity, InteractiveAttachOptions, attach_interactive_with_options,
  begin_attach, current_terminal_size, request, resume_attach,
};
use rmux_proto::{
  ClientMessage, CodecError as RmuxCodecError, CommandSpec, ErrorCode, ServerMessage, SessionInfo,
};
use std::io;
use std::time::Duration;
use thiserror::Error;
use tokio::time::sleep;

const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const INITIAL_RECONNECT_DELAY: Duration = Duration::from_millis(250);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(5);

pub async fn run(arguments: Arguments) -> Result<(), CliError> {
  match arguments.command {
    Command::Session { command } => match command {
      SessionCommand::List { host } => list_sessions(&host).await,
      SessionCommand::New {
        host,
        name,
        cwd,
        command,
      } => create_session(&host, name, command_spec(command), cwd).await,
      SessionCommand::Kill { host, session } => kill_session(&host, &session).await,
    },
    Command::Shell {
      host,
      session,
      resume_from,
      read_only,
      resize,
    } => shell(&host, &session, resume_from, !read_only, resize).await,
  }
}

async fn list_sessions(host: &str) -> Result<(), CliError> {
  let sessions = remote_sessions(host).await?;
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
  host: &str,
  name: Option<String>,
  command: Option<CommandSpec>,
  working_directory: Option<String>,
) -> Result<(), CliError> {
  let session = create_remote_session(host, name, command, working_directory).await?;
  println!("{}\t{}", session.session_id, session.name);
  Ok(())
}

async fn kill_session(host: &str, session: &str) -> Result<(), CliError> {
  let response = remote_request(
    host,
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
  host: &str,
  session: &str,
  initial_resume_from: Option<u64>,
  request_input_lease: bool,
  request_layout_lease: bool,
) -> Result<(), CliError> {
  ensure_session(host, session).await?;

  let rmux_identity = rmux_identity();
  let mut resume_from = initial_resume_from;
  let mut reconnect_delay = INITIAL_RECONNECT_DELAY;
  let mut recover_leases_after_connection_loss = false;
  let mut attachment_token = None;

  loop {
    let stream = match open_ssh_tunnel(host).await {
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
          eprintln!("ctl: detached locally from {host}:{session}");
          return Ok(());
        }
        AttachExitReason::SessionEnded { exit_code } => {
          eprintln!("ctl: {host}:{session} ended with exit code {exit_code:?}; not reconnecting");
          return Ok(());
        }
        AttachExitReason::ConnectionClosed => {
          resume_from = exit.next_sequence;
          recover_leases_after_connection_loss = true;
          wait_to_reconnect(&mut reconnect_delay, "SSH connection closed").await;
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

async fn ensure_session(host: &str, session: &str) -> Result<(), CliError> {
  if remote_sessions(host)
    .await?
    .iter()
    .any(|candidate| candidate.name == session || candidate.session_id == session)
  {
    return Ok(());
  }

  match create_remote_session(host, Some(session.into()), None, None).await {
    Ok(created) => {
      eprintln!("ctl: created remote session {}", created.name);
      Ok(())
    }
    Err(CliError::Rmux(RmuxClientError::Server {
      code: ErrorCode::SessionAlreadyExists,
      ..
    })) => Ok(()),
    Err(error) => Err(error),
  }
}

async fn remote_sessions(host: &str) -> Result<Vec<SessionInfo>, CliError> {
  match remote_request(host, ClientMessage::ListSessions).await? {
    ServerMessage::SessionList { sessions } => Ok(sessions),
    response => Err(unexpected("session_list", &response)),
  }
}

async fn create_remote_session(
  host: &str,
  name: Option<String>,
  command: Option<CommandSpec>,
  working_directory: Option<String>,
) -> Result<SessionInfo, CliError> {
  let response = remote_request(
    host,
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

async fn remote_request(host: &str, message: ClientMessage) -> Result<ServerMessage, CliError> {
  let stream = open_ssh_tunnel(host).await?;
  Ok(request(stream, &rmux_identity(), message).await?)
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
    "ctl: remote connection interrupted ({reason}); reconnecting in {} ms",
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
