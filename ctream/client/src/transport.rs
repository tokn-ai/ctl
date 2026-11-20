use std::{
  io::{self, Read},
  path::PathBuf,
  process::{Child, ChildStdin, ChildStdout, Command, Stdio},
  sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
  },
  thread,
  time::{Duration, Instant},
};

pub struct Channel<R, W> {
  pub reader: R,
  pub writer: W,
  pub cancellation: Cancellation,
}

/// Requests cleanup after closing the channel input. Process transports allow a
/// bounded grace period for the agent to release its resources before killing it.
#[derive(Clone, Default)]
pub struct Cancellation(Option<Arc<CancellationState>>);

struct CancellationState {
  requested: AtomicBool,
  cancel: Box<dyn Fn() + Send + Sync>,
}

impl Cancellation {
  pub fn new(cancel: impl Fn() + Send + Sync + 'static) -> Self {
    Self(Some(Arc::new(CancellationState {
      requested: AtomicBool::new(false),
      cancel: Box::new(cancel),
    })))
  }

  pub fn cancel(&self) {
    if let Some(state) = &self.0
      && !state.requested.swap(true, Ordering::AcqRel)
    {
      (state.cancel)();
    }
  }
}

struct Process {
  child: Mutex<Child>,
  closing: AtomicBool,
}

impl Process {
  fn close(self: &Arc<Self>) {
    if self.closing.swap(true, Ordering::AcqRel) {
      return;
    }
    let process = Arc::clone(self);
    thread::spawn(move || {
      let deadline = Instant::now() + Duration::from_secs(1);
      loop {
        let mut child = process
          .child
          .lock()
          .unwrap_or_else(|error| error.into_inner());
        if matches!(child.try_wait(), Ok(Some(_))) {
          return;
        }
        if Instant::now() >= deadline {
          let _ = child.kill();
          let _ = child.wait();
          return;
        }
        drop(child);
        thread::sleep(Duration::from_millis(10));
      }
    });
  }

  fn wait(&self) -> io::Result<()> {
    loop {
      let status = self
        .child
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .try_wait()?;
      if let Some(status) = status {
        return if status.success() {
          Ok(())
        } else {
          Err(io::Error::other(format!(
            "agent channel exited with {status}"
          )))
        };
      }
      thread::sleep(Duration::from_millis(10));
    }
  }
}

/// Opens a duplex agent channel. Arguments are chosen by the protocol client.
pub trait Transport {
  type Reader: Read + Send + 'static;
  type Writer: io::Write + Send + 'static;

  fn open(&self, arguments: &[String]) -> io::Result<Channel<Self::Reader, Self::Writer>>;
}

pub struct Ssh(pub String);

/// Agent subprocess connected through local standard-input/output pipes.
pub struct Local {
  pub agent: PathBuf,
}

impl Default for Local {
  fn default() -> Self {
    let name = format!("ctream-agent{}", std::env::consts::EXE_SUFFIX);
    let agent = std::env::current_exe()
      .ok()
      .map(|executable| executable.with_file_name(&name))
      .filter(|agent| agent.is_file())
      .unwrap_or_else(|| PathBuf::from(name));
    Self { agent }
  }
}

pub struct ProcessReader {
  process: Arc<Process>,
  stdout: ChildStdout,
}

impl Read for ProcessReader {
  fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
    if buffer.is_empty() {
      return Ok(0);
    }
    let count = self.stdout.read(buffer)?;
    if count == 0 {
      self.process.close();
      self.process.wait()?;
    }
    Ok(count)
  }
}

impl Drop for ProcessReader {
  fn drop(&mut self) {
    self.process.close();
  }
}

fn spawn(command: &mut Command) -> io::Result<Channel<ProcessReader, ChildStdin>> {
  let mut child = command
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .spawn()?;
  let writer = child.stdin.take().expect("piped agent input");
  let stdout = child.stdout.take().expect("piped agent output");
  let process = Arc::new(Process {
    child: Mutex::new(child),
    closing: AtomicBool::new(false),
  });
  let cleanup = Arc::clone(&process);
  Ok(Channel {
    reader: ProcessReader { process, stdout },
    writer,
    cancellation: Cancellation::new(move || cleanup.close()),
  })
}

fn shell_argument(value: &str) -> String {
  format!("'{}'", value.replace('\'', "'\\''"))
}

impl Transport for Ssh {
  type Reader = ProcessReader;
  type Writer = ChildStdin;

  fn open(&self, arguments: &[String]) -> io::Result<Channel<Self::Reader, Self::Writer>> {
    if self.0.is_empty() || self.0.starts_with('-') {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid SSH destination",
      ));
    }
    let arguments = arguments
      .iter()
      .map(|arg| shell_argument(arg))
      .collect::<Vec<_>>()
      .join(" ");
    let remote = format!(r#"exec env PATH="$PATH:$HOME/.local/bin" ctream-agent {arguments}"#);
    spawn(Command::new("ssh").args([
      "-T",
      "-C",
      "-o",
      "BatchMode=yes",
      "-o",
      "ConnectTimeout=10",
      "--",
      &self.0,
      &remote,
    ]))
  }
}

impl Transport for Local {
  type Reader = ProcessReader;
  type Writer = ChildStdin;

  fn open(&self, arguments: &[String]) -> io::Result<Channel<Self::Reader, Self::Writer>> {
    spawn(Command::new(&self.agent).args(arguments))
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  #[cfg(unix)]
  use std::io::Write;

  #[test]
  fn shell_arguments_preserve_metacharacters_as_literal_text() {
    assert_eq!(
      shell_argument("a'b $(command); value"),
      "'a'\\''b $(command); value'"
    );
  }

  #[cfg(unix)]
  #[test]
  fn local_channel_is_duplex_and_reports_process_failure() {
    let local = Local {
      agent: PathBuf::from("/bin/sh"),
    };
    let Channel {
      mut reader,
      mut writer,
      ..
    } = local
      .open(&["-c".into(), "read value; printf '%s' \"$value\"".into()])
      .unwrap();
    writer.write_all(b"literal input\n").unwrap();
    drop(writer);
    let mut response = String::new();
    reader.read_to_string(&mut response).unwrap();
    assert_eq!(response, "literal input");
    let Channel {
      mut reader, writer, ..
    } = local.open(&["-c".into(), "exit 9".into()]).unwrap();
    drop(writer);
    assert!(reader.read_to_string(&mut String::new()).is_err());
  }

  #[cfg(unix)]
  #[test]
  fn output_eof_reaps_a_process_that_keeps_running() {
    let local = Local {
      agent: PathBuf::from("/bin/sh"),
    };
    let Channel {
      mut reader, writer, ..
    } = local
      .open(&["-c".into(), "exec 1>&-; exec sleep 30".into()])
      .unwrap();
    drop(writer);
    let started = Instant::now();
    assert!(reader.read_to_end(&mut Vec::new()).is_err());
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(
      reader
        .process
        .child
        .lock()
        .unwrap()
        .try_wait()
        .unwrap()
        .is_some()
    );
  }

  #[test]
  fn cancellation_callback_runs_once_across_clones() {
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = Arc::clone(&count);
    let cancellation = Cancellation::new(move || {
      observed.fetch_add(1, Ordering::SeqCst);
    });
    cancellation.clone().cancel();
    cancellation.cancel();
    assert_eq!(count.load(Ordering::SeqCst), 1);
  }
}
