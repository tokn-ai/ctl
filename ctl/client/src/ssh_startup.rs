use std::io;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStderr};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::CoreError;

const MAX_DIAGNOSTICS: usize = 8192;

/// Cancelling startup must not leave a detached stderr-draining task behind.
pub struct Diagnostics(JoinHandle<String>);

impl Diagnostics {
  pub fn start(stderr: Option<ChildStderr>) -> Self {
    Self(tokio::spawn(read_diagnostics(stderr)))
  }
}

impl Drop for Diagnostics {
  fn drop(&mut self) {
    self.0.abort();
  }
}

pub async fn read_diagnostics(stderr: Option<ChildStderr>) -> String {
  let Some(mut stderr) = stderr else {
    return String::new();
  };
  let mut retained = Vec::new();
  let mut buffer = [0; 1024];
  while let Ok(count) = stderr.read(&mut buffer).await {
    if count == 0 {
      break;
    }
    let keep = count.min(MAX_DIAGNOSTICS.saturating_sub(retained.len()));
    retained.extend_from_slice(&buffer[..keep]);
    // Keep draining after the cap so a verbose SSH process cannot deadlock.
  }
  String::from_utf8_lossy(&retained)
    .chars()
    .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
    .collect::<String>()
    .trim()
    .to_owned()
}

pub async fn startup_error(
  mut child: Child,
  mut diagnostics: Diagnostics,
  source: io::Error,
) -> CoreError {
  let status = if let Ok(Ok(status)) = timeout(Duration::from_secs(1), child.wait()).await {
    Some(status)
  } else {
    let _ = child.kill().await;
    None
  };
  let message = if let Ok(Ok(message)) = timeout(Duration::from_secs(1), &mut diagnostics.0).await {
    message
  } else {
    String::new()
  };
  if !message.is_empty() {
    CoreError::SshStartup(message)
  } else if let Some(status) = status {
    CoreError::SshStartup(format!("ssh exited with {status}; {source}"))
  } else {
    CoreError::ReadSshPreface(source)
  }
}

#[cfg(all(test, unix))]
mod tests {
  use super::*;
  use std::process::Stdio;
  use tokio::process::Command;

  #[tokio::test]
  async fn preserves_ssh_diagnostics_instead_of_only_reporting_eof() {
    let mut child = Command::new("sh")
      .args([
        "-c",
        "printf 'Host key verification failed.\\n' >&2; exit 255",
      ])
      .stderr(Stdio::piped())
      .spawn()
      .unwrap();
    let diagnostics = Diagnostics::start(child.stderr.take());
    let error = startup_error(child, diagnostics, io::ErrorKind::UnexpectedEof.into()).await;
    assert!(error.to_string().contains("Host key verification failed."));
  }

  #[tokio::test]
  async fn drains_large_diagnostics_without_unbounded_retention() {
    let mut child = Command::new("sh")
      .args([
        "-c",
        "i=0; while [ $i -lt 20000 ]; do printf 'diagnostic line\\n' >&2; i=$((i+1)); done",
      ])
      .stderr(Stdio::piped())
      .kill_on_drop(true)
      .spawn()
      .unwrap();
    let diagnostics = Diagnostics::start(child.stderr.take());
    let error = timeout(
      Duration::from_secs(5),
      startup_error(child, diagnostics, io::ErrorKind::UnexpectedEof.into()),
    )
    .await
    .unwrap();
    let CoreError::SshStartup(message) = error else {
      panic!("missing diagnostics")
    };
    assert!(message.len() <= MAX_DIAGNOSTICS);
    assert!(message.starts_with("diagnostic line"));
  }
}
