use interprocess::local_socket::{
  GenericFilePath, ListenerOptions, ToFsName,
  tokio::{Listener, Stream},
  traits::tokio::Stream as _,
};
use std::{
  io,
  path::{Path, PathBuf},
  process::{Command, Stdio},
  thread,
  time::Duration,
};
use tokio::time::{Instant, sleep, timeout};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as platform;

pub(super) struct Endpoint {
  // Drop the listener (including its pathname) before releasing the Unix election lock.
  pub(super) listener: Listener,
  #[cfg(unix)]
  _lock: std::fs::File,
}

pub(super) use platform::check_client;

pub(super) fn discard_pending(_stream: &Stream) {
  // Dropping an unread Windows named pipe can otherwise retain its handle in
  // a background flush until the evicted peer decides to read again.
  #[cfg(windows)]
  {
    let Stream::NamedPipe(pipe) = _stream;
    pipe.inner().assume_flushed();
  }
}

pub(super) fn bind() -> io::Result<Endpoint> {
  let path = platform::path()?;
  #[cfg(unix)]
  let lock = platform::acquire(&path)?;
  let options = ListenerOptions::new().name(path.to_fs_name::<GenericFilePath>()?);
  #[cfg(windows)]
  let options = platform::restrict(options)?;
  Ok(Endpoint {
    listener: options.create_tokio()?,
    #[cfg(unix)]
    _lock: lock,
  })
}

async fn try_connect(path: &Path) -> io::Result<Stream> {
  let stream = timeout(
    Duration::from_secs(2),
    Stream::connect(path.to_fs_name::<GenericFilePath>()?),
  )
  .await??;
  platform::check_server(&stream)?;
  Ok(stream)
}

fn spawn_daemon(executable: Option<&Path>) -> io::Result<()> {
  let mut command = Command::new(match executable {
    Some(path) => path.to_owned(),
    None => std::env::current_exe()?,
  });
  command
    .arg("daemon")
    .env_remove("CTREAM_INPUT_DELEGATION")
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
  #[cfg(windows)]
  let mut child = {
    use std::os::windows::process::CommandExt as _;
    // The detached daemon belongs to the interactive account, independently of
    // a particular SDK client. Some external job policies prohibit breakaway.
    const DETACHED_PROCESS: u32 = 0x00000008;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x01000000;
    match command
      .creation_flags(DETACHED_PROCESS | CREATE_BREAKAWAY_FROM_JOB)
      .spawn()
    {
      Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
        command.creation_flags(DETACHED_PROCESS).spawn()?
      }
      other => other?,
    }
  };
  #[cfg(not(windows))]
  let mut child = command.spawn()?;
  thread::Builder::new()
    .name("ctream-daemon-exit".into())
    .spawn(move || {
      let _ = child.wait();
    })?;
  Ok(())
}

fn offline(error: &io::Error) -> bool {
  matches!(
    error.kind(),
    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused | io::ErrorKind::ConnectionReset
  )
}

pub(super) async fn connect() -> io::Result<Stream> {
  connect_via(None).await
}

pub(super) async fn connect_via(executable: Option<&Path>) -> io::Result<Stream> {
  let path: PathBuf = platform::path()?;
  match try_connect(&path).await {
    Err(error) if offline(&error) => {}
    result => return result,
  }
  spawn_daemon(executable)?;
  let deadline = Instant::now() + Duration::from_secs(4);
  loop {
    sleep(Duration::from_millis(25)).await;
    match try_connect(&path).await {
      Err(error) if offline(&error) && Instant::now() < deadline => {}
      result => return result,
    }
  }
}

#[cfg(target_os = "linux")]
pub(super) use platform::fence_file;

pub(super) fn gesture_fence() -> io::Result<std::fs::File> {
  let file = platform::fence_file()?;
  file.try_lock_shared()?;
  Ok(file)
}

pub(super) fn wait_for_old_gestures() -> io::Result<()> {
  // Controllers and native drivers retain shared handles through completion or
  // held-input cleanup. A replacement daemon must drain these process-owned
  // pins before recovering with an empty in-memory turn table.
  platform::fence_file()?.lock()
}

#[cfg(windows)]
pub(crate) use windows::host_account_id;

#[cfg(target_os = "macos")]
pub(super) fn picker_directory() -> io::Result<PathBuf> {
  platform::path()?
    .parent()
    .map(Path::to_owned)
    .ok_or_else(|| io::Error::other("missing desktop runtime directory"))
}
pub(super) async fn connect_existing() -> io::Result<Stream> {
  try_connect(&platform::path()?).await
}
