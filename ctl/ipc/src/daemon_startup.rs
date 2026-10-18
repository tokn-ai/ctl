//! Endpoint-scoped startup coordination and ownership of spawned children.

use super::{
  CONNECT_RETRY_INTERVAL, CONNECT_TIMEOUT, ConnectError, DAEMON_EXECUTABLE_ENV,
  PROTOCOL_QUERY_TIMEOUT, Stream, check_daemon_protocol, connect, prepare_daemon_executable,
  resolve_executable, retryable_connect_error,
};
use sha2::{Digest as _, Sha256};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::{Instant, sleep};
#[cfg(windows)]
use windows_spawn::{Command, Stdio};

pub(super) async fn connect_or_start(
  path: &Path,
  executable: Option<&Path>,
) -> Result<Stream, ConnectError> {
  // A previous caller's runtime may have ended while its reaper still owns a
  // slow startup. Once that owner is live, use it without waiting for its lock.
  let startup = tokio::select! {
    startup = StartupLock::acquire(path) => startup.map_err(ConnectError::CoordinateDaemon)?,
    owner = connect_when_ready(path) => return owner,
  };
  if let Some(stream) = existing_owner(path).await? {
    return Ok(stream);
  }
  let selected = match executable {
    Some(executable) => executable.to_owned(),
    None => prepare_daemon_executable().await?,
  };
  // Pin both the query and the launch to the same build, including when a
  // staged symlink changes while the selected helper answers its query.
  let executable = resolve_executable(&selected).map_err(|source| ConnectError::StartDaemon {
    executable: selected,
    source,
  })?;
  check_daemon_protocol(&executable, PROTOCOL_QUERY_TIMEOUT).await?;
  // An explicit launch or an older client can become the owner during helper
  // preparation. Its endpoint remains authoritative; never replace it.
  if let Some(stream) = existing_owner(path).await? {
    return Ok(stream);
  }
  let mut command = Command::new(&executable);
  command
    .arg("--socket")
    .arg(path)
    .arg("--detach-from-terminal")
    .env("CTLD_SOCKET_PATH", path)
    .env(DAEMON_EXECUTABLE_ENV, &executable)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
  let startup = StartupReservation(Arc::new(Mutex::new(Some(startup))));
  let exited = spawn_reaped(command, Some(startup.clone()))
    .await
    .map_err(|source| ConnectError::StartDaemon { executable, source })?;
  let connection = connect_until_ready(path).await;
  if connection.is_ok() {
    startup.release();
  } else {
    // Keep the caller's connection deadline, but a slow child must still own
    // the startup reservation until it becomes reachable or exits.
    let path = path.to_owned();
    tokio::spawn(release_when_ready_or_exited(startup, path, exited));
  }
  connection
}

async fn connect_until_ready(path: &Path) -> Result<Stream, ConnectError> {
  let deadline = Instant::now() + CONNECT_TIMEOUT;
  loop {
    match connect(path).await {
      Ok(stream) => return Ok(stream),
      Err(error) if retryable_connect_error(&error) && Instant::now() < deadline => {
        sleep(CONNECT_RETRY_INTERVAL).await;
      }
      Err(error) => return Err(ConnectError::Connect(error)),
    }
  }
}

async fn connect_when_ready(path: &Path) -> Result<Stream, ConnectError> {
  loop {
    if let Some(stream) = existing_owner(path).await? {
      return Ok(stream);
    }
    sleep(CONNECT_RETRY_INTERVAL).await;
  }
}

async fn release_when_ready_or_exited(
  startup: StartupReservation,
  path: PathBuf,
  mut exited: tokio::sync::oneshot::Receiver<()>,
) {
  loop {
    tokio::select! {
      _ = &mut exited => return,
      connection = connect(&path) => {
        if connection.is_ok() {
          startup.release();
          return;
        }
      }
    }
    tokio::select! {
      _ = &mut exited => return,
      () = sleep(CONNECT_RETRY_INTERVAL) => {}
    }
  }
}

#[derive(Clone)]
struct StartupReservation(Arc<Mutex<Option<StartupLock>>>);

impl StartupReservation {
  fn release(&self) {
    self.0.lock().unwrap().take();
  }
}

async fn existing_owner(path: &Path) -> Result<Option<Stream>, ConnectError> {
  match connect(path).await {
    Ok(stream) => Ok(Some(stream)),
    Err(error) if retryable_connect_error(&error) => Ok(None),
    Err(error) => Err(ConnectError::Connect(error)),
  }
}

/// A daemon outlives a client and its Tokio runtime. An OS thread owns the child
/// from spawn through wait, including immediate startup failures. Creating the
/// thread first means a thread-creation failure cannot leave an unowned child.
async fn spawn_reaped(
  mut command: Command,
  startup: Option<StartupReservation>,
) -> io::Result<tokio::sync::oneshot::Receiver<()>> {
  let (started, result) = tokio::sync::oneshot::channel();
  let (exited, finished) = tokio::sync::oneshot::channel();
  std::thread::Builder::new()
    .name("ctld-reaper".into())
    .spawn(move || {
      #[cfg(unix)]
      let child = command.spawn();
      #[cfg(windows)]
      let child = {
        use windows_spawn::{CreationFlags, SpawnOptions};
        // std::process::Command broadly inherits handles even with null stdio.
        // Inherited capture pipes would keep the caller's output open for the
        // entire broker lifetime. Transfer only this daemon's null handles.
        command.spawn_with(SpawnOptions::new().creation_flags(CreationFlags::DETACHED_PROCESS))
      };
      match child {
        Ok(mut child) => {
          let _ = started.send(Ok(()));
          let _ = child.wait();
          drop(startup);
          let _ = exited.send(());
        }
        Err(error) => {
          let _ = started.send(Err(error));
        }
      }
    })?;
  result.await.map_err(io::Error::other)??;
  Ok(finished)
}

struct StartupLock(File);

impl StartupLock {
  async fn acquire(endpoint: &Path) -> io::Result<Self> {
    Self::acquire_for(endpoint, Duration::from_secs(30)).await
  }

  async fn acquire_for(endpoint: &Path, maximum_wait: Duration) -> io::Result<Self> {
    let directory = startup_directory();
    prepare_directory(&directory)?;
    let file = open_lock(&directory.join(endpoint_key(endpoint)?))?;
    let deadline = Instant::now() + maximum_wait;
    loop {
      match file.try_lock() {
        Ok(()) => return Ok(Self(file)),
        Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
          sleep(CONNECT_RETRY_INTERVAL).await;
        }
        Err(TryLockError::WouldBlock) => {
          return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "another client is still starting ctld at this endpoint",
          ));
        }
        Err(TryLockError::Error(error)) => return Err(error),
      }
    }
  }
}

impl Drop for StartupLock {
  fn drop(&mut self) {
    let _ = self.0.unlock();
  }
}

fn startup_directory() -> PathBuf {
  #[cfg(unix)]
  {
    // Keep coordination independent of CTLD_RUNTIME_DIR and XDG_RUNTIME_DIR:
    // two clients using the same explicit endpoint must share the same lock.
    PathBuf::from("/tmp").join(format!(
      "ctld-startup-{}",
      rustix::process::geteuid().as_raw()
    ))
  }
  #[cfg(windows)]
  {
    dirs::data_local_dir()
      .unwrap_or_else(std::env::temp_dir)
      .join("ctl")
      .join("daemon-startup")
  }
}

fn endpoint_key(endpoint: &Path) -> io::Result<String> {
  if endpoint.as_os_str().is_empty() {
    return Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      "ctld endpoint is empty",
    ));
  }
  #[cfg(unix)]
  let endpoint = {
    use std::os::unix::ffi::OsStrExt as _;
    let absolute = std::path::absolute(endpoint)?;
    let mut existing = absolute.as_path();
    let mut missing = Vec::new();
    while !existing.exists() {
      missing.push(existing.file_name().ok_or_else(|| {
        io::Error::new(
          io::ErrorKind::InvalidInput,
          "ctld endpoint has no file name",
        )
      })?);
      existing = existing.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "ctld endpoint has no parent")
      })?;
    }
    let mut canonical = existing.canonicalize()?;
    for name in missing.into_iter().rev() {
      canonical.push(name);
    }
    canonical.as_os_str().as_bytes().to_vec()
  };
  #[cfg(windows)]
  let endpoint = {
    use std::os::windows::ffi::OsStrExt as _;
    // Named pipe names are case insensitive. Fold ASCII without losing any
    // non-Unicode UTF-16 units accepted by the Windows path API.
    endpoint
      .as_os_str()
      .encode_wide()
      .map(|unit| {
        if (u16::from(b'A')..=u16::from(b'Z')).contains(&unit) {
          unit + u16::from(b'a' - b'A')
        } else {
          unit
        }
      })
      .flat_map(u16::to_le_bytes)
      .collect::<Vec<_>>()
  };
  Ok(format!("{:x}.lock", Sha256::digest(&endpoint)))
}

fn prepare_directory(directory: &Path) -> io::Result<()> {
  let mut builder = fs::DirBuilder::new();
  #[cfg(unix)]
  {
    use std::os::unix::fs::DirBuilderExt as _;
    builder.mode(0o700);
  }
  #[cfg(windows)]
  builder.recursive(true);
  match builder.create(directory) {
    Ok(()) => {}
    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
    Err(error) => return Err(error),
  }
  let metadata = fs::symlink_metadata(directory)?;
  let mut invalid = !metadata.is_dir() || metadata.file_type().is_symlink();
  #[cfg(unix)]
  {
    use std::os::unix::fs::MetadataExt as _;
    invalid |=
      metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o077 != 0;
  }
  #[cfg(windows)]
  {
    use std::os::windows::fs::MetadataExt as _;
    invalid |= metadata.file_attributes() & 0x0000_0400 != 0;
  }
  if invalid {
    return Err(io::Error::new(
      io::ErrorKind::PermissionDenied,
      "ctld startup directory is not a private owner-controlled directory",
    ));
  }
  Ok(())
}

fn open_lock(path: &Path) -> io::Result<File> {
  let mut options = OpenOptions::new();
  options.read(true).write(true).create(true).truncate(false);
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt as _;
    options
      .mode(0o600)
      .custom_flags(i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits()).map_err(io::Error::other)?);
  }
  #[cfg(windows)]
  {
    use std::os::windows::fs::OpenOptionsExt as _;
    options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
  }
  let file = options.open(path)?;
  let metadata = file.metadata()?;
  let mut invalid = !metadata.is_file();
  #[cfg(unix)]
  {
    use std::os::unix::fs::MetadataExt as _;
    invalid |= metadata.uid() != rustix::process::geteuid().as_raw()
      || metadata.mode() & 0o077 != 0
      || metadata.nlink() != 1;
  }
  #[cfg(windows)]
  {
    use std::os::windows::fs::MetadataExt as _;
    invalid |= metadata.file_attributes() & 0x0000_0400 != 0;
  }
  if invalid {
    return Err(io::Error::new(
      io::ErrorKind::PermissionDenied,
      "ctld startup lock is not a private owner-controlled regular file",
    ));
  }
  Ok(file)
}

#[cfg(test)]
mod tests;
