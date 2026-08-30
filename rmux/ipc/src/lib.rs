use std::env;
#[cfg(unix)]
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::process::Stdio;
#[cfg(unix)]
use std::time::Duration;
#[cfg(unix)]
use tokio::net::UnixStream;
#[cfg(unix)]
use tokio::time::{Instant, sleep};

const RUNTIME_DIRECTORY_ENV: &str = "RMUX_RUNTIME_DIR";
#[cfg(unix)]
const DAEMON_EXECUTABLE_ENV: &str = "RMUXD_BIN";
#[cfg(unix)]
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
#[cfg(unix)]
const CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(25);

#[must_use]
pub fn socket_path() -> PathBuf {
  runtime_directory().join("rmux.sock")
}

#[must_use]
pub fn runtime_directory() -> PathBuf {
  if let Some(directory) = env::var_os(RUNTIME_DIRECTORY_ENV) {
    return PathBuf::from(directory);
  }

  if let Some(directory) = env::var_os("XDG_RUNTIME_DIR") {
    return PathBuf::from(directory).join("rmux");
  }

  fallback_runtime_directory()
}

/// Connects to the local daemon, starting it when the endpoint is absent.
///
/// Concurrent callers may each launch a daemon candidate. Every caller then
/// waits for whichever candidate binds the endpoint first; losing candidates
/// exit when they observe that the endpoint is already served.
///
/// # Errors
///
/// Returns an error when the endpoint cannot be connected, the daemon
/// executable cannot be resolved, or a daemon candidate cannot be spawned.
#[cfg(unix)]
pub async fn connect_or_start_daemon(socket_path: &Path) -> Result<UnixStream, ConnectError> {
  connect_or_start_with(
    || UnixStream::connect(socket_path),
    || start_daemon(socket_path),
    CONNECT_TIMEOUT,
    CONNECT_RETRY_INTERVAL,
  )
  .await
}

#[cfg(unix)]
async fn connect_or_start_with<T, Connect, ConnectFuture, Start>(
  mut connect: Connect,
  start: Start,
  retry_timeout: Duration,
  retry_interval: Duration,
) -> Result<T, ConnectError>
where
  Connect: FnMut() -> ConnectFuture,
  ConnectFuture: Future<Output = io::Result<T>>,
  Start: FnOnce() -> Result<(), ConnectError>,
{
  match connect().await {
    Ok(connection) => return Ok(connection),
    Err(error) if retryable_connect_error(&error) => {}
    Err(error) => return Err(ConnectError::Connect(error)),
  }

  start()?;
  let deadline = Instant::now() + retry_timeout;
  loop {
    match connect().await {
      Ok(connection) => return Ok(connection),
      Err(error) if Instant::now() < deadline && retryable_connect_error(&error) => {
        sleep(retry_interval).await;
      }
      Err(error) => return Err(ConnectError::Connect(error)),
    }
  }
}

#[cfg(unix)]
fn retryable_connect_error(error: &io::Error) -> bool {
  matches!(
    error.kind(),
    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
  )
}

#[cfg(unix)]
fn start_daemon(socket_path: &Path) -> Result<(), ConnectError> {
  let executable = daemon_executable()?;
  std::process::Command::new(&executable)
    .arg("--socket")
    .arg(socket_path)
    .arg("--detach-from-terminal")
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .map_err(|source| ConnectError::StartDaemon { executable, source })?;
  Ok(())
}

#[cfg(unix)]
fn daemon_executable() -> Result<PathBuf, ConnectError> {
  if let Some(executable) = env::var_os(DAEMON_EXECUTABLE_ENV) {
    return Ok(PathBuf::from(executable));
  }

  let current_executable = env::current_exe().map_err(ConnectError::CurrentExecutable)?;
  let sibling = current_executable.with_file_name("rmuxd");
  if sibling.is_file() {
    return Ok(sibling);
  }

  Ok(PathBuf::from("rmuxd"))
}

/// Failure to connect to or launch the local `rmuxd` process.
#[cfg(unix)]
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConnectError {
  #[error("could not connect to rmuxd: {0}")]
  Connect(#[source] io::Error),
  #[error("could not determine the current executable: {0}")]
  CurrentExecutable(#[source] io::Error),
  #[error("could not start daemon using {}: {source}", executable.display())]
  StartDaemon {
    executable: PathBuf,
    source: io::Error,
  },
}

/// Creates and validates the private directory containing a local endpoint.
///
/// # Errors
///
/// Returns an error when the endpoint has no parent, the directory cannot be
/// created, or the directory is not private and owned by the current user.
pub fn prepare_runtime_directory(path: &Path) -> io::Result<()> {
  let directory = path.parent().ok_or_else(|| {
    io::Error::new(
      io::ErrorKind::InvalidInput,
      format!("socket path {} has no parent directory", path.display()),
    )
  })?;
  let existed = directory.exists();
  std::fs::create_dir_all(directory)?;
  if !existed {
    set_owner_only_permissions(directory)?;
  }
  secure_runtime_directory(directory)
}

#[cfg(unix)]
fn fallback_runtime_directory() -> PathBuf {
  let uid = rustix::process::getuid().as_raw();
  PathBuf::from("/tmp").join(format!("rmux-{uid}"))
}

#[cfg(not(unix))]
fn fallback_runtime_directory() -> PathBuf {
  env::temp_dir().join("rmux")
}

#[cfg(unix)]
fn secure_runtime_directory(directory: &Path) -> io::Result<()> {
  use std::os::unix::fs::{MetadataExt, PermissionsExt};

  let metadata = std::fs::symlink_metadata(directory)?;
  if metadata.file_type().is_symlink() || !metadata.is_dir() {
    return Err(io::Error::new(
      io::ErrorKind::PermissionDenied,
      format!(
        "runtime path {} is not a real directory",
        directory.display()
      ),
    ));
  }

  let expected_uid = rustix::process::getuid().as_raw();
  if metadata.uid() != expected_uid {
    return Err(io::Error::new(
      io::ErrorKind::PermissionDenied,
      format!(
        "runtime directory {} is owned by another user",
        directory.display()
      ),
    ));
  }

  if metadata.permissions().mode() & 0o077 != 0 {
    return Err(io::Error::new(
      io::ErrorKind::PermissionDenied,
      format!(
        "runtime directory {} is accessible by other users",
        directory.display()
      ),
    ));
  }

  Ok(())
}

#[cfg(not(unix))]
fn secure_runtime_directory(_directory: &Path) -> io::Result<()> {
  Ok(())
}

#[cfg(unix)]
fn set_owner_only_permissions(directory: &Path) -> io::Result<()> {
  use std::os::unix::fs::PermissionsExt;

  std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_owner_only_permissions(_directory: &Path) -> io::Result<()> {
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[cfg(unix)]
  use std::sync::Arc;
  #[cfg(unix)]
  use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

  #[test]
  fn socket_is_inside_runtime_directory() {
    assert_eq!(socket_path().file_name().unwrap(), "rmux.sock");
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn healthy_endpoint_does_not_start_daemon() {
    let connect_count = Arc::new(AtomicUsize::new(0));
    let start_count = Arc::new(AtomicUsize::new(0));
    let connection = connect_or_start_with(
      {
        let connect_count = Arc::clone(&connect_count);
        move || {
          connect_count.fetch_add(1, Ordering::Relaxed);
          std::future::ready(Ok::<_, io::Error>(42))
        }
      },
      {
        let start_count = Arc::clone(&start_count);
        move || {
          start_count.fetch_add(1, Ordering::Relaxed);
          Ok(())
        }
      },
      Duration::ZERO,
      Duration::ZERO,
    )
    .await
    .expect("healthy endpoint connects");

    assert_eq!(connection, 42);
    assert_eq!(connect_count.load(Ordering::Relaxed), 1);
    assert_eq!(start_count.load(Ordering::Relaxed), 0);
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn missing_endpoint_starts_daemon_then_retries() {
    let connect_count = Arc::new(AtomicUsize::new(0));
    let start_count = Arc::new(AtomicUsize::new(0));
    let connection = connect_or_start_with(
      {
        let connect_count = Arc::clone(&connect_count);
        move || {
          let attempt = connect_count.fetch_add(1, Ordering::Relaxed);
          std::future::ready(if attempt == 0 {
            Err(io::Error::from(io::ErrorKind::NotFound))
          } else {
            Ok(42)
          })
        }
      },
      {
        let start_count = Arc::clone(&start_count);
        move || {
          start_count.fetch_add(1, Ordering::Relaxed);
          Ok(())
        }
      },
      Duration::from_secs(1),
      Duration::ZERO,
    )
    .await
    .expect("started endpoint connects");

    assert_eq!(connection, 42);
    assert_eq!(connect_count.load(Ordering::Relaxed), 2);
    assert_eq!(start_count.load(Ordering::Relaxed), 1);
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn non_retryable_connect_error_does_not_start_daemon() {
    let start_count = Arc::new(AtomicUsize::new(0));
    let error = connect_or_start_with(
      || {
        std::future::ready(Err::<(), _>(io::Error::from(
          io::ErrorKind::PermissionDenied,
        )))
      },
      {
        let start_count = Arc::clone(&start_count);
        move || {
          start_count.fetch_add(1, Ordering::Relaxed);
          Ok(())
        }
      },
      Duration::ZERO,
      Duration::ZERO,
    )
    .await
    .expect_err("permission denial must be returned");

    assert!(matches!(
      error,
      ConnectError::Connect(error) if error.kind() == io::ErrorKind::PermissionDenied
    ));
    assert_eq!(start_count.load(Ordering::Relaxed), 0);
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn concurrent_connectors_tolerate_duplicate_start_candidates() {
    fn connect(
      barrier: Arc<tokio::sync::Barrier>,
      endpoint_ready: Arc<AtomicBool>,
      start_count: Arc<AtomicUsize>,
    ) -> impl Future<Output = Result<(), ConnectError>> {
      let connect_count = Arc::new(AtomicUsize::new(0));
      let connect_endpoint_ready = Arc::clone(&endpoint_ready);
      connect_or_start_with(
        move || {
          let first_attempt = connect_count.fetch_add(1, Ordering::Relaxed) == 0;
          let barrier = Arc::clone(&barrier);
          let endpoint_ready = Arc::clone(&connect_endpoint_ready);
          async move {
            if first_attempt {
              barrier.wait().await;
              Err(io::Error::from(io::ErrorKind::ConnectionRefused))
            } else if endpoint_ready.load(Ordering::Acquire) {
              Ok(())
            } else {
              Err(io::Error::from(io::ErrorKind::ConnectionRefused))
            }
          }
        },
        move || {
          start_count.fetch_add(1, Ordering::Relaxed);
          endpoint_ready.store(true, Ordering::Release);
          Ok(())
        },
        Duration::from_secs(1),
        Duration::ZERO,
      )
    }

    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let endpoint_ready = Arc::new(AtomicBool::new(false));
    let start_count = Arc::new(AtomicUsize::new(0));
    let first = connect(
      Arc::clone(&barrier),
      Arc::clone(&endpoint_ready),
      Arc::clone(&start_count),
    );
    let second = connect(barrier, endpoint_ready, Arc::clone(&start_count));

    let (first, second) = tokio::join!(first, second);
    first.expect("first connector observes the endpoint");
    second.expect("second connector observes the endpoint");
    assert_eq!(start_count.load(Ordering::Relaxed), 2);
  }
}
