use super::CHECK_INTERVAL;
use std::{
  collections::VecDeque,
  io::{self, Read},
  process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio},
  sync::Mutex,
  thread::{self, ScopedJoinHandle},
};

pub(super) struct Process(Mutex<Child>);

impl Process {
  pub(super) fn start(
    mut command: Command,
  ) -> io::Result<(Self, ChildStdin, ChildStdout, ChildStderr)> {
    let mut child = command
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(Stdio::piped())
      .spawn()?;
    let input = child.stdin.take().expect("piped FFmpeg stdin");
    let output = child.stdout.take().expect("piped FFmpeg stdout");
    let stderr = child.stderr.take().expect("piped FFmpeg stderr");
    Ok((Self(Mutex::new(child)), input, output, stderr))
  }

  pub(super) fn wait(&self, stop: impl Fn() -> bool) -> io::Result<ExitStatus> {
    loop {
      {
        let mut child = self.0.lock().unwrap();
        if let Some(status) = child.try_wait()? {
          return Ok(status);
        }
        if stop() {
          if let Err(error) = child.kill() {
            return child.try_wait()?.ok_or(error);
          }
          return child.wait();
        }
      }
      thread::sleep(CHECK_INTERVAL);
    }
  }
}

impl Drop for Process {
  fn drop(&mut self) {
    let child = self.0.get_mut().unwrap_or_else(|error| error.into_inner());
    let _ = child.kill();
    let _ = child.wait();
  }
}

pub(super) fn diagnostics(mut reader: ChildStderr) -> String {
  const LIMIT: usize = 4096;
  let mut tail = VecDeque::with_capacity(LIMIT);
  let mut bytes = [0; LIMIT];
  loop {
    match reader.read(&mut bytes) {
      Ok(0) => break,
      Ok(count) => {
        let excess = (tail.len() + count).saturating_sub(LIMIT);
        tail.drain(..excess);
        tail.extend(&bytes[..count]);
      }
      Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
      Err(_) => break,
    }
  }
  String::from_utf8_lossy(tail.make_contiguous())
    .chars()
    .filter(|&character| !character.is_control() || matches!(character, '\n' | '\t'))
    .collect::<String>()
    .trim()
    .to_owned()
}

pub(super) fn check_exit(status: ExitStatus, mode: &str, diagnostics: &str) -> io::Result<()> {
  if status.success() {
    return Ok(());
  }
  let mut message = format!("FFmpeg {mode} exited with {status}");
  if !diagnostics.is_empty() {
    message.push_str(": ");
    message.push_str(diagnostics);
  }
  Err(io::Error::other(message))
}

pub(super) fn join<T>(worker: ScopedJoinHandle<'_, T>) -> io::Result<T> {
  worker
    .join()
    .map_err(|_| io::Error::other("FFmpeg worker panicked"))
}
