use super::{Arguments, Command, SessionCommand};
use ctl_core::{ClientIdentity as ControlIdentity, ClientState, CoreError, HostConfig};
use ctl_proto::{CodecError as ControlCodecError, decode_pairing_invitation};
use rmux_client::{
  AttachExitReason, AttachRequest, ClientError as RmuxClientError,
  ClientIdentity as RmuxClientIdentity, InteractiveAttachOptions, attach_interactive_with_options,
  begin_attach, current_terminal_size, request,
};
use rmux_proto::{
  ClientMessage, CodecError as RmuxCodecError, CommandSpec, ErrorCode, ServerMessage, SessionInfo,
};
use rustix::process::getuid;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, IsTerminal, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;
use thiserror::Error;
use tokio::time::sleep;

const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const STATE_FILE_NAME: &str = "client.json";
const INITIAL_RECONNECT_DELAY: Duration = Duration::from_millis(250);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(5);

pub async fn run(arguments: Arguments) -> Result<(), CliError> {
  let state_directory = resolve_state_directory(arguments.state_dir)?;

  match arguments.command {
    Command::Pair { invitation, alias } => {
      let invitation = invitation.map_or_else(read_pairing_invitation, Ok)?;
      pair_device(&state_directory, &invitation, alias).await
    }
    Command::Hosts => list_hosts(&state_directory),
    Command::Session { command } => match command {
      SessionCommand::List { host } => list_sessions(&state_directory, &host).await,
      SessionCommand::New {
        host,
        name,
        cwd,
        command,
      } => create_session(&state_directory, &host, name, command_spec(command), cwd).await,
      SessionCommand::Kill { host, session } => {
        kill_session(&state_directory, &host, &session).await
      }
    },
    Command::Shell {
      host,
      session,
      resume_from,
      read_only,
      resize,
    } => {
      shell(
        &state_directory,
        &host,
        &session,
        resume_from,
        !read_only,
        resize,
      )
      .await
    }
  }
}

async fn pair_device(
  state_directory: &Path,
  encoded_invitation: &str,
  alias: String,
) -> Result<(), CliError> {
  let invitation = decode_pairing_invitation(encoded_invitation)?;
  let mut state = if let Some(state) = load_state(state_directory)? {
    state
  } else {
    let identity = ControlIdentity::generate()?;
    let state = ClientState::new(&identity);
    // Persist the generated identity before consuming the one-time token.
    // If pairing fails, the next invitation still belongs to the same client.
    save_state(state_directory, &state)?;
    state
  };
  let identity = state.identity()?;
  let host = ctl_core::pair(&invitation, alias, &identity, "ctl", CLIENT_VERSION).await?;
  state.upsert_host(host.clone());
  save_state(state_directory, &state).map_err(|source| CliError::PairingStateNotSaved {
    source: Box::new(source),
  })?;

  println!("paired {} as {}", host.device_id, host.alias);
  Ok(())
}

fn read_pairing_invitation() -> Result<String, CliError> {
  if io::stdin().is_terminal() {
    eprint!("Paste the pairing invitation, then press Enter: ");
  }
  let mut invitation = String::new();
  io::stdin()
    .read_line(&mut invitation)
    .map_err(CliError::ReadPairingInvitation)?;
  let invitation = invitation.trim().to_owned();
  if invitation.is_empty() {
    return Err(CliError::MissingPairingInvitation);
  }
  Ok(invitation)
}

fn list_hosts(state_directory: &Path) -> Result<(), CliError> {
  let state = required_state(state_directory)?;
  if state.hosts.is_empty() {
    println!("no paired hosts");
    return Ok(());
  }

  println!("ALIAS\tENDPOINT\tDEVICE_ID");
  for host in state.hosts {
    println!("{}\t{}\t{}", host.alias, host.endpoint, host.device_id);
  }
  Ok(())
}

async fn list_sessions(state_directory: &Path, host: &str) -> Result<(), CliError> {
  let state = required_state(state_directory)?;
  let sessions = remote_sessions(&state, host).await?;
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
  state_directory: &Path,
  host: &str,
  name: Option<String>,
  command: Option<CommandSpec>,
  working_directory: Option<String>,
) -> Result<(), CliError> {
  let state = required_state(state_directory)?;
  let session = create_remote_session(&state, host, name, command, working_directory).await?;
  println!("{}\t{}", session.session_id, session.name);
  Ok(())
}

async fn kill_session(state_directory: &Path, host: &str, session: &str) -> Result<(), CliError> {
  let state = required_state(state_directory)?;
  let response = remote_request(
    &state,
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
  state_directory: &Path,
  host_alias: &str,
  session: &str,
  initial_resume_from: Option<u64>,
  request_input_lease: bool,
  request_layout_lease: bool,
) -> Result<(), CliError> {
  let state = required_state(state_directory)?;
  ensure_session(&state, host_alias, session).await?;

  let host = host_configuration(&state, host_alias)?;
  let identity = state.identity()?;
  let rmux_identity = rmux_identity();
  let mut resume_from = initial_resume_from;
  let mut reconnect_delay = INITIAL_RECONNECT_DELAY;
  let mut recover_leases_after_connection_loss = false;

  loop {
    let stream = match ctl_core::open_rmux_tunnel(&host, &identity, "ctl", CLIENT_VERSION).await {
      Ok(stream) => stream,
      Err(error) if is_retryable_control_error(&error) => {
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
      // request sensitive editable command buffers it cannot render.
      request_command_line: false,
    };
    let (stream, attached) = match begin_attach(stream, &rmux_identity, request).await {
      Ok(attachment) => attachment,
      Err(error) if is_retryable_rmux_error(&error) => {
        wait_to_reconnect(&mut reconnect_delay, &error).await;
        continue;
      }
      Err(error) => return Err(error.into()),
    };

    let interactive_options = if recover_leases_after_connection_loss {
      // Initial lease contention deliberately remains view-only. Only a
      // connection that was already interactive can safely try to reclaim its
      // former lease, and `rmuxd` will claim it only after its old attachment
      // has expired or disconnected.
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
          eprintln!("ctl: detached locally from {host_alias}:{session}");
          return Ok(());
        }
        AttachExitReason::SessionEnded { exit_code } => {
          eprintln!(
            "ctl: {host_alias}:{session} ended with exit code {exit_code:?}; not reconnecting"
          );
          return Ok(());
        }
        AttachExitReason::ConnectionClosed => {
          // The raw output sequence advances only after bytes have reached the
          // local terminal. Reattaching from this point avoids both replaying
          // output and replaying user input.
          resume_from = Some(exit.next_sequence);
          recover_leases_after_connection_loss = true;
          wait_to_reconnect(&mut reconnect_delay, "connection closed").await;
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

async fn ensure_session(state: &ClientState, host: &str, session: &str) -> Result<(), CliError> {
  if remote_sessions(state, host)
    .await?
    .iter()
    .any(|candidate| candidate.name == session || candidate.session_id == session)
  {
    return Ok(());
  }

  match create_remote_session(state, host, Some(session.into()), None, None).await {
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

async fn remote_sessions(state: &ClientState, host: &str) -> Result<Vec<SessionInfo>, CliError> {
  match remote_request(state, host, ClientMessage::ListSessions).await? {
    ServerMessage::SessionList { sessions } => Ok(sessions),
    response => Err(unexpected("session_list", &response)),
  }
}

async fn create_remote_session(
  state: &ClientState,
  host: &str,
  name: Option<String>,
  command: Option<CommandSpec>,
  working_directory: Option<String>,
) -> Result<SessionInfo, CliError> {
  let response = remote_request(
    state,
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

async fn remote_request(
  state: &ClientState,
  host_alias: &str,
  message: ClientMessage,
) -> Result<ServerMessage, CliError> {
  let host = host_configuration(state, host_alias)?;
  let identity = state.identity()?;
  let stream = ctl_core::open_rmux_tunnel(&host, &identity, "ctl", CLIENT_VERSION).await?;
  Ok(request(stream, &rmux_identity(), message).await?)
}

fn host_configuration(state: &ClientState, alias: &str) -> Result<HostConfig, CliError> {
  state
    .host(alias)
    .cloned()
    .ok_or_else(|| CliError::UnknownHost { host: alias.into() })
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

fn is_retryable_control_error(error: &CoreError) -> bool {
  match error {
    CoreError::UnexpectedEof => true,
    CoreError::Io(source) | CoreError::Codec(ControlCodecError::Io(source)) => {
      is_retryable_io_error(source)
    }
    _ => false,
  }
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

async fn wait_to_reconnect(reason_delay: &mut Duration, reason: impl std::fmt::Display) {
  eprintln!(
    "ctl: remote connection interrupted ({reason}); reconnecting in {} ms",
    reason_delay.as_millis()
  );
  sleep(*reason_delay).await;
  *reason_delay = reason_delay.saturating_mul(2).min(MAX_RECONNECT_DELAY);
}

fn required_state(state_directory: &Path) -> Result<ClientState, CliError> {
  load_state(state_directory)?.ok_or(CliError::StateNotConfigured)
}

fn resolve_state_directory(requested: Option<PathBuf>) -> Result<PathBuf, CliError> {
  requested.map_or_else(default_state_directory, Ok)
}

fn default_state_directory() -> Result<PathBuf, CliError> {
  if let Some(directory) = env::var_os("CTL_STATE_DIR") {
    return Ok(PathBuf::from(directory));
  }

  let home = env::var_os("HOME").ok_or(CliError::MissingHomeDirectory)?;
  #[cfg(target_os = "macos")]
  {
    Ok(
      PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("ctl"),
    )
  }
  #[cfg(not(target_os = "macos"))]
  {
    if let Some(directory) = env::var_os("XDG_CONFIG_HOME") {
      return Ok(PathBuf::from(directory).join("ctl"));
    }
    Ok(PathBuf::from(home).join(".config").join("ctl"))
  }
}

fn load_state(state_directory: &Path) -> Result<Option<ClientState>, CliError> {
  prepare_state_directory(state_directory)?;
  let state_path = state_file_path(state_directory);
  let metadata = match fs::symlink_metadata(&state_path) {
    Ok(metadata) => metadata,
    Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
    Err(source) => return Err(state_io(&state_path, source)),
  };
  validate_state_file(&state_path, &metadata)?;

  let serialized = fs::read(&state_path).map_err(|source| state_io(&state_path, source))?;
  let state: ClientState =
    serde_json::from_slice(&serialized).map_err(|source| CliError::InvalidState {
      path: state_path,
      source,
    })?;
  state.identity()?;
  Ok(Some(state))
}

fn save_state(state_directory: &Path, state: &ClientState) -> Result<(), CliError> {
  prepare_state_directory(state_directory)?;
  state.identity()?;
  let state_path = state_file_path(state_directory);
  match fs::symlink_metadata(&state_path) {
    Ok(metadata) => validate_state_file(&state_path, &metadata)?,
    Err(source) if source.kind() == io::ErrorKind::NotFound => {}
    Err(source) => return Err(state_io(&state_path, source)),
  }
  let serialized = serde_json::to_vec_pretty(state).map_err(|source| CliError::InvalidState {
    path: state_path.clone(),
    source,
  })?;

  let (temporary_path, mut file) = create_temporary_state_file(state_directory)?;
  let write_result = (|| {
    file
      .write_all(&serialized)
      .map_err(|source| state_io(&temporary_path, source))?;
    file
      .write_all(b"\n")
      .map_err(|source| state_io(&temporary_path, source))?;
    file
      .sync_all()
      .map_err(|source| state_io(&temporary_path, source))
  })();
  drop(file);
  if let Err(error) = write_result {
    let _ignored = fs::remove_file(&temporary_path);
    return Err(error);
  }

  if let Err(source) = fs::rename(&temporary_path, &state_path) {
    let _ignored = fs::remove_file(&temporary_path);
    return Err(state_io(&state_path, source));
  }
  fs::set_permissions(&state_path, fs::Permissions::from_mode(0o600))
    .map_err(|source| state_io(&state_path, source))?;
  Ok(())
}

fn prepare_state_directory(state_directory: &Path) -> Result<(), CliError> {
  match fs::symlink_metadata(state_directory) {
    Ok(metadata) => validate_state_directory(state_directory, &metadata),
    Err(source) if source.kind() == io::ErrorKind::NotFound => {
      fs::create_dir_all(state_directory).map_err(|source| state_io(state_directory, source))?;
      fs::set_permissions(state_directory, fs::Permissions::from_mode(0o700))
        .map_err(|source| state_io(state_directory, source))?;
      let metadata = fs::symlink_metadata(state_directory)
        .map_err(|source| state_io(state_directory, source))?;
      validate_state_directory(state_directory, &metadata)
    }
    Err(source) => Err(state_io(state_directory, source)),
  }
}

fn validate_state_directory(
  state_directory: &Path,
  metadata: &fs::Metadata,
) -> Result<(), CliError> {
  if metadata.file_type().is_symlink()
    || !metadata.is_dir()
    || metadata.uid() != getuid().as_raw()
    || metadata.mode() & 0o077 != 0
  {
    return Err(CliError::InsecureStateDirectory {
      path: state_directory.into(),
    });
  }
  Ok(())
}

fn validate_state_file(state_path: &Path, metadata: &fs::Metadata) -> Result<(), CliError> {
  if metadata.file_type().is_symlink()
    || !metadata.is_file()
    || metadata.uid() != getuid().as_raw()
    || metadata.mode() & 0o077 != 0
  {
    return Err(CliError::InsecureStateFile {
      path: state_path.into(),
    });
  }
  Ok(())
}

fn create_temporary_state_file(state_directory: &Path) -> Result<(PathBuf, fs::File), CliError> {
  for attempt in 0_u8..32 {
    let temporary_path = state_directory.join(format!(
      ".{STATE_FILE_NAME}.{}.{attempt}.tmp",
      std::process::id()
    ));
    match OpenOptions::new()
      .create_new(true)
      .write(true)
      .mode(0o600)
      .open(&temporary_path)
    {
      Ok(file) => return Ok((temporary_path, file)),
      Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {}
      Err(source) => return Err(state_io(&temporary_path, source)),
    }
  }
  Err(CliError::TemporaryStateFileExhausted {
    directory: state_directory.into(),
  })
}

fn state_file_path(state_directory: &Path) -> PathBuf {
  state_directory.join(STATE_FILE_NAME)
}

fn state_io(path: &Path, source: io::Error) -> CliError {
  CliError::StateIo {
    path: path.into(),
    source,
  }
}

fn unexpected(expected: &'static str, response: &ServerMessage) -> CliError {
  CliError::UnexpectedResponse {
    expected,
    actual: format!("{response:?}"),
  }
}

#[derive(Debug, Error)]
pub enum CliError {
  #[error("a pairing invitation is required")]
  MissingPairingInvitation,
  #[error("could not read the pairing invitation: {0}")]
  ReadPairingInvitation(io::Error),
  #[error("could not determine the default client-state directory because HOME is not set")]
  MissingHomeDirectory,
  #[error("client-state I/O failed at {}: {source}", path.display())]
  StateIo { path: PathBuf, source: io::Error },
  #[error("client-state directory {} is not an owner-only directory", path.display())]
  InsecureStateDirectory { path: PathBuf },
  #[error("client-state file {} is not an owner-only regular file", path.display())]
  InsecureStateFile { path: PathBuf },
  #[error("client-state file {} is invalid: {source}", path.display())]
  InvalidState {
    path: PathBuf,
    source: serde_json::Error,
  },
  #[error("could not allocate a temporary client-state file in {}", directory.display())]
  TemporaryStateFileExhausted { directory: PathBuf },
  #[error("no client identity is configured; run `ctl pair <invitation> --alias NAME` first")]
  StateNotConfigured,
  #[error("host '{host}' is not paired; run `ctl hosts` to inspect configured devices")]
  UnknownHost { host: String },
  #[error("pairing succeeded but client state could not be saved: {source}")]
  PairingStateNotSaved {
    #[source]
    source: Box<CliError>,
  },
  #[error(transparent)]
  Invitation(#[from] ctl_proto::InvitationError),
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

#[cfg(test)]
mod tests {
  use super::*;
  use uuid::Uuid;

  #[test]
  fn state_round_trips_in_owner_only_storage() {
    let directory = TestDirectory::new();
    let identity = ControlIdentity::generate().unwrap();
    let mut expected = ClientState::new(&identity);
    expected.upsert_host(HostConfig {
      alias: "mac".into(),
      endpoint: "100.100.100.100:9944".into(),
      server_name: "mac.ctl.invalid".into(),
      device_id: "device-id".into(),
      device_certificate_base64: "certificate".into(),
    });

    save_state(directory.path(), &expected).unwrap();
    let state_path = state_file_path(directory.path());
    let directory_mode = fs::symlink_metadata(directory.path()).unwrap().mode() & 0o777;
    let file_mode = fs::symlink_metadata(&state_path).unwrap().mode() & 0o777;

    assert_eq!(directory_mode, 0o700);
    assert_eq!(file_mode, 0o600);
    assert_eq!(load_state(directory.path()).unwrap(), Some(expected));
  }

  #[test]
  fn existing_world_readable_state_directory_is_rejected() {
    let directory = TestDirectory::new();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).unwrap();

    assert!(matches!(
      load_state(directory.path()),
      Err(CliError::InsecureStateDirectory { .. })
    ));
  }

  struct TestDirectory {
    path: PathBuf,
  }

  impl TestDirectory {
    fn new() -> Self {
      let path = env::temp_dir().join(format!("ctl-cli-test-{}", Uuid::new_v4()));
      fs::create_dir(&path).unwrap();
      fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
      Self { path }
    }

    fn path(&self) -> &Path {
      &self.path
    }
  }

  impl Drop for TestDirectory {
    fn drop(&mut self) {
      let _ignored = fs::remove_dir_all(&self.path);
    }
  }
}
