use std::{
  fs::{self, File, OpenOptions},
  io::{self, BufRead, BufReader, BufWriter, Read, Write},
  net::Shutdown,
  os::{
    fd::AsRawFd,
    unix::{
      fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
      net::{UnixListener, UnixStream},
    },
  },
  path::PathBuf,
  thread,
  time::Duration,
};

// The GUI worker reserializes the CLI arguments, including a 4096-byte options
// JSON argument whose characters can expand during JSON escaping.
const MAX_REQUEST: u64 = 32 * 1024;
const MAX_CHUNK: usize = 64 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const DATA: u8 = 0;
const DONE: u8 = 1;
const FAILED: u8 = 2;

fn uid() -> libc::uid_t {
  // SAFETY: geteuid takes no arguments and cannot fail.
  unsafe { libc::geteuid() }
}

fn directory(create: bool) -> io::Result<PathBuf> {
  // The desktop and SSH environments may have different temporary directories.
  let path = PathBuf::from(format!("/private/tmp/ctream-agent-{}", uid()));
  if create {
    match fs::DirBuilder::new().mode(0o700).create(&path) {
      Ok(()) => {}
      Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
      Err(error) => return Err(error),
    }
  }
  let info = fs::symlink_metadata(&path)?;
  if !info.is_dir() || info.uid() != uid() || info.mode() & 0o777 != 0o700 {
    return Err(io::Error::new(
      io::ErrorKind::PermissionDenied,
      "ctream agent socket directory must be private and owned by the desktop user",
    ));
  }
  Ok(path)
}

fn authenticate(stream: &UnixStream) -> io::Result<()> {
  let mut user = 0;
  let mut group = 0;
  // SAFETY: stream owns an open Unix socket and both output pointers are valid.
  if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut user, &mut group) } != 0 {
    return Err(io::Error::last_os_error());
  }
  if user != uid() {
    return Err(io::Error::new(
      io::ErrorKind::PermissionDenied,
      "the ctream agent socket peer must have the same user ID",
    ));
  }
  Ok(())
}

type Handler = fn(
  &[String],
  Option<&str>,
  &mut BufReader<UnixStream>,
  &mut Response,
) -> ctream_driver::Result<()>;

pub struct Listener {
  listener: UnixListener,
  path: PathBuf,
  _lock: File,
}

impl Listener {
  pub fn bind() -> io::Result<Self> {
    let directory = directory(true)?;
    let lock = OpenOptions::new()
      .write(true)
      .create(true)
      .mode(0o600)
      .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
      .open(directory.join("serve.lock"))?;
    if !lock.metadata()?.is_file() {
      return Err(io::Error::other("ctream agent lock must be a regular file"));
    }
    // SAFETY: lock owns an open file; flock retains the lock until it closes.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
      let error = io::Error::last_os_error();
      return if error.kind() == io::ErrorKind::WouldBlock {
        Err(io::Error::new(
          io::ErrorKind::AddrInUse,
          "a ctream agent is already running for this user",
        ))
      } else {
        Err(error)
      };
    }
    let path = directory.join("serve.sock");
    match fs::symlink_metadata(&path) {
      Ok(info) if info.file_type().is_socket() => fs::remove_file(&path)?,
      Ok(_) => return Err(io::Error::other("ctream agent socket path is occupied")),
      Err(error) if error.kind() == io::ErrorKind::NotFound => {}
      Err(error) => return Err(error),
    }
    let server = Self {
      listener: UnixListener::bind(&path)?,
      path,
      _lock: lock,
    };
    fs::set_permissions(&server.path, fs::Permissions::from_mode(0o600))?;
    Ok(server)
  }

  pub fn run(self, handle: Handler) -> io::Result<()> {
    for stream in self.listener.incoming() {
      let stream = match stream {
        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
        other => other?,
      };
      if authenticate(&stream).is_ok() {
        thread::spawn(move || {
          let _ = serve_connection(stream, handle);
        });
      }
    }
    Ok(())
  }
}

impl Drop for Listener {
  fn drop(&mut self) {
    let _ = fs::remove_file(&self.path);
  }
}

fn connect() -> io::Result<UnixStream> {
  let stream = directory(false)
    .and_then(|directory| UnixStream::connect(directory.join("serve.sock")))
    .map_err(|error| {
      if matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
      ) {
        io::Error::new(
          error.kind(),
          "start ctream-agent serve from the logged-in macOS desktop first",
        )
      } else {
        error
      }
    })?;
  authenticate(&stream)?;
  Ok(stream)
}

fn serve_connection(
  stream: UnixStream,
  handle: impl FnOnce(
    &[String],
    Option<&str>,
    &mut BufReader<UnixStream>,
    &mut Response,
  ) -> ctream_driver::Result<()>,
) -> io::Result<()> {
  stream.set_read_timeout(Some(COMMAND_TIMEOUT))?;
  stream.set_write_timeout(Some(COMMAND_TIMEOUT))?;
  let mut input = BufReader::new(stream.try_clone()?);
  let mut response = BufWriter::new(Reply(stream));
  let result = (|| {
    let mut line = Vec::new();
    input
      .by_ref()
      .take(MAX_REQUEST)
      .read_until(b'\n', &mut line)?;
    if line.last() != Some(&b'\n') {
      return Err("incomplete or oversized ctream agent request".into());
    }
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Request {
      Legacy(Vec<String>),
      Controlled {
        arguments: Vec<String>,
        delegation: Option<String>,
      },
    }
    let (arguments, delegation) = match serde_json::from_slice(&line)? {
      Request::Legacy(args) => (args, None),
      Request::Controlled {
        arguments,
        delegation,
      } => (arguments, delegation),
    };
    input.get_ref().set_read_timeout(None)?;
    handle(&arguments, delegation.as_deref(), &mut input, &mut response)
  })();
  response
    .into_inner()
    .map_err(|error| error.into_error())?
    .finish(result)
}

pub fn relay(arguments: &[String]) -> io::Result<()> {
  let mut stream = connect()?;
  // Older GUI workers cannot validate desktop admission. Use a distinct
  // private envelope for every mutation so such workers fail before delivery.
  let mutating = matches!(
    arguments.get(1).map(String::as_str),
    Some("input" | "input-sync" | "input-window" | "session-input" | "viewer-input")
  );
  let mut request = if mutating {
    serde_json::to_vec(
      &serde_json::json!({"arguments":arguments,"delegation":std::env::var("CTREAM_INPUT_DELEGATION").ok()}),
    )?
  } else {
    serde_json::to_vec(arguments)?
  };
  request.push(b'\n');
  if request.len() as u64 > MAX_REQUEST {
    return Err(io::Error::other("ctream agent request exceeds size limit"));
  }
  stream.write_all(&request)?;
  let incoming = stream.try_clone()?;
  thread::spawn(move || {
    let _ = io::copy(&mut io::stdin().lock(), &mut stream);
    let _ = stream.shutdown(Shutdown::Write);
  });
  receive(BufReader::new(incoming), io::stdout().lock(), mutating)
}

// The private socket carries stdout bytes and a terminal result separately.
// The SSH side reproduces the original stdio and exit status contract.
pub type Response = BufWriter<Reply>;

pub struct Reply(UnixStream);

impl Reply {
  fn chunk(&mut self, tag: u8, bytes: &[u8]) -> io::Result<()> {
    let mut header = [tag; 5];
    header[1..].copy_from_slice(&(bytes.len() as u32).to_be_bytes());
    self.0.write_all(&header)?;
    self.0.write_all(bytes)
  }

  fn finish(&mut self, result: ctream_driver::Result<()>) -> io::Result<()> {
    match result {
      Ok(()) => self.chunk(DONE, &[]),
      Err(error) => {
        let message = error.to_string();
        let bytes = message.as_bytes();
        self.chunk(FAILED, &bytes[..bytes.len().min(MAX_CHUNK)])
      }
    }
  }
}

impl Write for Reply {
  fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
    if bytes.is_empty() {
      return Ok(0);
    }
    let count = bytes.len().min(MAX_CHUNK);
    self.chunk(DATA, &bytes[..count])?;
    Ok(count)
  }

  fn flush(&mut self) -> io::Result<()> {
    self.0.flush()
  }
}

fn receive(mut input: impl Read, mut output: impl Write, mutating: bool) -> io::Result<()> {
  // The parent SDK decides whether a mutation was in flight. Preserve a
  // transport loss across this private hop, distinct from a GUI worker refusal.
  let lost = |error| {
    if mutating {
      ctream_driver::UnconfirmedDelivery::error(error)
    } else {
      error
    }
  };
  let mut bytes = vec![0; MAX_CHUNK];
  loop {
    let mut header = [0; 5];
    input.read_exact(&mut header).map_err(|error| {
      lost(if error.kind() == io::ErrorKind::UnexpectedEof {
        io::Error::other("desktop ctream agent closed the request without a result")
      } else {
        error
      })
    })?;
    let [tag, a, b, c, d] = header;
    let count = u32::from_be_bytes([a, b, c, d]) as usize;
    let chunk = bytes
      .get_mut(..count)
      .ok_or_else(|| lost(io::Error::other("oversized ctream agent response chunk")))?;
    input.read_exact(chunk).map_err(lost)?;
    match tag {
      DATA => {
        output.write_all(chunk)?;
        output.flush()?;
      }
      DONE if count == 0 => return Ok(()),
      FAILED => {
        return Err(io::Error::other(
          String::from_utf8_lossy(chunk).into_owned(),
        ));
      }
      _ => {
        return Err(lost(io::Error::other(
          "unknown ctream agent response chunk",
        )));
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn replies_preserve_data_and_a_terminal_failure_after_startup() {
    let (mut client, server) = UnixStream::pair().unwrap();
    let server = thread::spawn(move || {
      serve_connection(server, |arguments, delegation, input, output| {
        assert!(delegation.is_none());
        assert_eq!(arguments, ["version", "operation"]);
        let mut data = Vec::new();
        input.read_to_end(&mut data)?;
        assert_eq!(data, b"input");
        output.write_all(b"result")?;
        Err("capture failed".into())
      })
      .unwrap();
    });
    client
      .write_all(b"[\"version\",\"operation\"]\ninput")
      .unwrap();
    client.shutdown(Shutdown::Write).unwrap();
    let mut output = Vec::new();
    assert_eq!(
      receive(client, &mut output, false).unwrap_err().to_string(),
      "capture failed"
    );
    assert_eq!(output, b"result");
    server.join().unwrap();
  }

  #[test]
  fn a_missing_terminal_result_is_a_failure() {
    let error = receive([].as_slice(), io::sink(), true).unwrap_err();
    assert!(ctream_driver::UnconfirmedDelivery::is(&error));
  }
}
