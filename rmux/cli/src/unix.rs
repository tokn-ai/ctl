use rmux_client::{
  AttachExitReason, AttachRequest, ClientIdentity, attach_interactive, begin_attach,
  current_terminal_size, get_shell_state, request,
};
use rmux_proto::{
  ClientMessage, CommandSpec, PromptPhase, ServerMessage, SessionInfo, ShellType, TuiHint,
};
use std::env;
use std::io;
use std::path::Path;
use thiserror::Error;

const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

pub async fn create_session(
  socket_path: &Path,
  name: Option<String>,
  command: Option<CommandSpec>,
  working_directory: Option<String>,
) -> Result<(), ClientError> {
  let terminal_size = current_terminal_size();
  let working_directory = match working_directory {
    Some(directory) => Some(directory),
    None => Some(current_working_directory()?),
  };
  let response = local_request(
    socket_path,
    ClientMessage::CreateSession {
      name,
      command,
      working_directory,
      terminal_size,
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

pub async fn list_sessions(socket_path: &Path) -> Result<(), ClientError> {
  let response = local_request(socket_path, ClientMessage::ListSessions).await?;
  match response {
    ServerMessage::SessionList { sessions } => {
      if sessions.is_empty() {
        println!("no sessions");
        return Ok(());
      }

      println!("NAME\tID\tSTATUS\tSIZE\tNEXT_SEQUENCE");
      for session in sessions {
        print_session(&session);
      }
      Ok(())
    }
    response => Err(unexpected("session_list", &response)),
  }
}

pub async fn show_shell_state(socket_path: &Path, session: &str) -> Result<(), ClientError> {
  let stream = rmux_ipc::connect_or_start_daemon(socket_path).await?;
  let state = get_shell_state(stream, &client_identity(), session).await?;
  let command_line = if state.shell_state.command_line_redacted {
    "redacted"
  } else if state.shell_state.current_command_line.is_some() {
    // This command intentionally never prints typed command text. It is
    // useful for verifying state integration without turning a terminal
    // status lookup into a secret-disclosure mechanism.
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
      .cwd
      .as_deref()
      .map_or_else(|| "-".into(), escaped_for_terminal)
  );
  println!(
    "PROMPT_PHASE\t{}",
    prompt_phase_name(state.shell_state.prompt_phase)
  );
  println!("TUI_HINT\t{}", tui_hint_name(state.shell_state.tui_hint));
  println!("COMMAND_LINE\t{command_line}");
  println!("REVISION\t{}", state.shell_state.revision);
  println!("OBSERVED_SEQUENCE\t{}", state.shell_state.observed_sequence);
  Ok(())
}

pub async fn kill_session(socket_path: &Path, session: &str) -> Result<(), ClientError> {
  let response = local_request(
    socket_path,
    ClientMessage::KillSession {
      session: session.into(),
    },
  )
  .await?;
  match response {
    ServerMessage::Success => Ok(()),
    response => Err(unexpected("success", &response)),
  }
}

pub async fn attach_session(
  socket_path: &Path,
  session: &str,
  resume_from: Option<u64>,
  request_input_lease: bool,
  request_layout_lease: bool,
) -> Result<(), ClientError> {
  let stream = rmux_ipc::connect_or_start_daemon(socket_path).await?;
  let identity = client_identity();
  let (stream, attached) = begin_attach(
    stream,
    &identity,
    AttachRequest {
      session: session.into(),
      resume_from,
      terminal_size: current_terminal_size(),
      request_input_lease,
      request_layout_lease,
      // The raw terminal CLI has no metadata UI. Keep sensitive edit buffers
      // local to clients that explicitly need to render them.
      request_command_line: false,
    },
  )
  .await?;
  let exit = attach_interactive(stream, &attached).await?;
  if matches!(exit.reason, AttachExitReason::ConnectionClosed) {
    eprintln!("rmux: attachment connection closed; the session may still be running");
  }
  Ok(())
}

async fn local_request(
  socket_path: &Path,
  message: ClientMessage,
) -> Result<ServerMessage, ClientError> {
  let stream = rmux_ipc::connect_or_start_daemon(socket_path).await?;
  let identity = client_identity();
  Ok(request(stream, &identity, message).await?)
}

fn client_identity() -> ClientIdentity {
  ClientIdentity {
    name: "rmux".into(),
    version: CLIENT_VERSION.into(),
  }
}

fn current_working_directory() -> Result<String, ClientError> {
  let directory = env::current_dir().map_err(ClientError::CurrentDirectory)?;
  directory
    .into_os_string()
    .into_string()
    .map_err(ClientError::NonUtf8CurrentDirectory)
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

fn unexpected(expected: &'static str, response: &ServerMessage) -> ClientError {
  ClientError::UnexpectedResponse {
    expected,
    actual: format!("{response:?}"),
  }
}

#[derive(Debug, Error)]
pub enum ClientError {
  #[error("could not determine the current working directory: {0}")]
  CurrentDirectory(io::Error),
  #[error("the current working directory is not valid UTF-8: {0:?}")]
  NonUtf8CurrentDirectory(std::ffi::OsString),
  #[error(transparent)]
  LocalIpc(#[from] rmux_ipc::ConnectError),
  #[error(transparent)]
  Protocol(#[from] rmux_client::ClientError),
  #[error("expected {expected}, received {actual}")]
  UnexpectedResponse {
    expected: &'static str,
    actual: String,
  },
}
