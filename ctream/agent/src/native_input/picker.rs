//! The macOS picker grants the preview process access to a specific window.
//! Expose only that selected native worker to its daemon controller, retaining
//! the same delegate fence and EOF cleanup as an ordinary platform worker.
use super::{Gate, Mode};
use ctream_driver_input::macos::{WindowIdentity, WindowInput};
use ctream_proto::{Target, WindowId};
use std::{
  fs,
  io::{self, BufRead, BufReader, Read, Write},
  os::{
    fd::AsRawFd,
    unix::net::{UnixListener, UnixStream},
  },
  path::{Path, PathBuf},
  sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
  },
  thread,
  time::Duration,
};
use uuid::Uuid;

fn path(id: &str) -> io::Result<PathBuf> {
  let id = Uuid::parse_str(id).map_err(io::Error::other)?;
  Ok(crate::daemon::picker_directory()?.join(format!("p-{}", id.simple())))
}

fn authenticate(stream: &UnixStream) -> io::Result<()> {
  let (mut uid, mut gid) = (0, 0);
  // SAFETY: stream owns a live Unix socket and both result pointers are valid.
  if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 {
    return Err(io::Error::last_os_error());
  }
  if uid != rustix::process::geteuid().as_raw() {
    return Err(io::Error::new(
      io::ErrorKind::PermissionDenied,
      "unexpected picker peer",
    ));
  }
  Ok(())
}

pub(crate) fn open(id: &str, window: &str, delegation_key: &str) -> io::Result<UnixStream> {
  let mut stream = UnixStream::connect(path(id)?)?;
  authenticate(&stream)?;
  writeln!(stream, "{window} {delegation_key}")?;
  Ok(stream)
}

struct Registration {
  path: PathBuf,
  closed: Arc<AtomicBool>,
}
impl Drop for Registration {
  fn drop(&mut self) {
    self.closed.store(true, Ordering::Release);
    let _ = UnixStream::connect(&self.path);
    let _ = fs::remove_file(&self.path);
  }
}

fn worker(mut stream: UnixStream, window: &str, identity: WindowIdentity) -> io::Result<()> {
  authenticate(&stream)?;
  stream.set_read_timeout(Some(Duration::from_secs(5)))?;
  let mut input = BufReader::new(stream.try_clone()?);
  let mut hello = String::new();
  input.by_ref().take(192).read_line(&mut hello)?;
  let (source, key) = hello
    .strip_suffix('\n')
    .and_then(|s| s.split_once(' '))
    .ok_or_else(|| io::Error::other("invalid picker handshake"))?;
  if source != window {
    return Err(io::Error::other("picker source changed"));
  }
  let mut gate = Gate::delegated(Target::MacosDesktop, Some(window), key)?;
  let mut native = WindowInput::open_with_delivery(identity, true)?;
  stream.set_read_timeout(None)?;
  stream.write_all(b"ready\n")?;
  let result = super::receive(
    Mode::Synchronized,
    &mut gate,
    input,
    &mut stream,
    |commands| {
      for command in commands {
        native.dispatch(command)?;
      }
      Ok(())
    },
  );
  let released = native.reset();
  drop(native);
  result.and(released).and(gate.finish())
}

pub fn serve(
  exe: &Path,
  identity: WindowIdentity,
  label: &str,
  attachment: Option<&ctream_client::live::ViewerAttachment>,
  input: impl BufRead,
  output: &mut impl Write,
) -> io::Result<()> {
  let window: String = WindowId::macos(identity.id, identity.pid)
    .ok_or_else(|| io::Error::other("invalid picker window"))?
    .into();
  let authority = match attachment {
    Some(proof) => {
      crate::daemon::Authority::attach(Target::MacosDesktop, Some(&window), proof.as_secret())?
    }
    None => super::controller(Target::MacosDesktop, Some(&window), Some(label), Some(exe))?,
  };
  let id = Uuid::new_v4().to_string();
  let path = path(&id)?;
  let listener = UnixListener::bind(&path)?;
  let registration = Registration {
    path,
    closed: Arc::default(),
  };
  let closed = registration.closed.clone();
  let provider = thread::Builder::new()
    .name("ctream-picker-driver".into())
    .spawn(move || {
      for stream in listener.incoming() {
        if closed.load(Ordering::Acquire) {
          break;
        }
        match stream {
          Ok(stream) => {
            let _ = worker(stream, &window, identity);
          }
          Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
          Err(_) => break,
        }
      }
    })?;
  let result = super::serve_controller(
    Mode::Viewer { receipts: true },
    authority,
    Some(&id),
    None,
    input,
    output,
  );
  drop(registration);
  let _ = provider.join();
  result
}
