use std::io;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, ChildStderr};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout, timeout_at};

use crate::CoreError;

const MAX_DIAGNOSTICS: usize = 8192;
const MAX_STARTUP_BYTES: usize = 64 * 1024;
const MAX_STARTUP_PREVIEW: usize = 256;
const MAX_MARKER_BYTES: usize = 128;

#[derive(Debug, thiserror::Error)]
#[error("unsupported readiness marker {0:?}")]
struct UnsupportedMarker(String);

/// Reads only the startup prelude. Callers retain their buffered reader so any
/// identity or service bytes read ahead remain available after the marker.
pub(crate) struct Preface {
  read_bytes: usize,
  preview: Vec<u8>,
  idle_timeout: Duration,
}

impl Default for Preface {
  fn default() -> Self {
    Self {
      read_bytes: 0,
      preview: Vec::new(),
      idle_timeout: Duration::from_secs(30),
    }
  }
}

impl Preface {
  pub async fn read_marker(
    &mut self,
    reader: &mut (impl AsyncRead + Unpin),
    markers: &[&[u8]],
    reserved_prefix: &[u8],
  ) -> io::Result<usize> {
    let mut pending = Vec::new();
    // Before stdout begins, OpenSSH may still be waiting for a human to answer
    // authentication prompts. Once authenticated output arrives, a silent
    // startup must not keep CLI connections pending forever. Starting a new
    // marker read also excludes time spent in the authentication callback.
    let mut deadline = (self.read_bytes != 0).then(|| Instant::now() + self.idle_timeout);
    loop {
      if self.read_bytes == MAX_STARTUP_BYTES {
        return Err(self.invalid("remote startup output exceeded 64 KiB before readiness"));
      }
      let read = if let Some(deadline) = deadline {
        timeout_at(deadline, reader.read_u8()).await.map_err(|_| {
          io::Error::new(
            io::ErrorKind::TimedOut,
            self.detail("remote startup stopped sending stdout before readiness"),
          )
        })?
      } else {
        reader.read_u8().await
      };
      let byte = match read {
        Ok(byte) => byte,
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof && self.read_bytes != 0 => {
          return Err(self.invalid("remote stdout closed before the expected readiness marker"));
        }
        Err(error) => return Err(error),
      };
      self.read_bytes += 1;
      deadline = Some(Instant::now() + self.idle_timeout);
      if self.preview.len() < MAX_STARTUP_PREVIEW {
        self.preview.push(byte);
      }
      pending.push(byte);
      if let Some(index) = markers.iter().position(|marker| *marker == pending) {
        return Ok(index);
      }
      if pending.starts_with(reserved_prefix)
        && (byte == b'\n' || pending.len() == MAX_MARKER_BYTES)
      {
        return Err(io::Error::new(
          io::ErrorKind::InvalidData,
          UnsupportedMarker(String::from_utf8_lossy(&pending).trim_end().to_owned()),
        ));
      }
      while !pending.is_empty()
        && !markers.iter().any(|marker| marker.starts_with(&pending))
        && !reserved_prefix.starts_with(&pending)
        && !pending.starts_with(reserved_prefix)
      {
        pending.remove(0);
      }
    }
  }

  fn invalid(&self, reason: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, self.detail(reason))
  }

  fn detail(&self, reason: &str) -> String {
    format!("{reason}; startup stdout: {}", preview(&self.preview))
  }
}

fn preview(bytes: &[u8]) -> String {
  // Debug escaping keeps terminal control sequences from becoming active when
  // a CLI displays the bounded preview, including invalid UTF-8 replacement.
  format!("{:?}", String::from_utf8_lossy(bytes))
}

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

pub fn supervise(
  mut child: Child,
  diagnostics: Diagnostics,
  mut shutdown_requested: watch::Receiver<bool>,
) {
  tokio::spawn(async move {
    // Service callers own presentation, including full-screen terminal UIs.
    // A failed child is observed through its service stream; this cleanup task
    // must never write directly to the caller's terminal.
    tokio::select! {
      _ = child.wait() => {}
      changed = shutdown_requested.changed() => {
        if changed.is_ok() && *shutdown_requested.borrow() {
          let _ignored = child.start_kill();
        }
        let _ignored = child.wait().await;
      }
    }
    drop(diagnostics);
  });
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
  let unsupported = source
    .get_ref()
    .and_then(|source| source.downcast_ref::<UnsupportedMarker>())
    .map(|marker| marker.0.clone());
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
  if let Some(marker) = unsupported {
    CoreError::UnsupportedSshProtocol { marker }
  } else if source.kind() == io::ErrorKind::InvalidData {
    let detail = if message.is_empty() {
      source.to_string()
    } else {
      format!("{source}; SSH stderr: {message}")
    };
    CoreError::InvalidSshPreface(detail)
  } else if !message.is_empty() {
    CoreError::SshStartup(message)
  } else if let Some(status) = status {
    CoreError::SshStartup(format!("ssh exited with {status}; {source}"))
  } else {
    CoreError::ReadSshPreface(source)
  }
}

#[cfg(test)]
mod preface_tests {
  use super::*;
  use tokio::io::{AsyncWriteExt as _, BufReader};

  const MARKERS: &[&[u8]] = &[b"ctl-ssh-v1\n", b"ctl-ssh-v3\n", b"ctl-ssh-nf\n"];

  #[tokio::test]
  async fn idle_timeout_starts_after_stdout_and_excludes_authentication_waiting() {
    let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
    let (reader, mut writer) = tokio::io::duplex(4096);
    let task = tokio::spawn(async move {
      tokio::time::sleep(Duration::from_millis(40)).await;
      writer.write_all(MARKERS[0]).await.unwrap();
    });
    let mut preface = Preface {
      idle_timeout: Duration::from_millis(10),
      ..Preface::default()
    };
    let mut reader = BufReader::new(reader);
    assert_eq!(
      preface
        .read_marker(&mut reader, MARKERS, b"ctl-ssh-")
        .await
        .unwrap(),
      0
    );
    task.await.unwrap();

    let (reader, mut writer) = tokio::io::duplex(4096);
    writer.write_all(b"banner without readiness").await.unwrap();
    let mut reader = BufReader::new(reader);
    let mut preface = Preface {
      idle_timeout: Duration::from_millis(10),
      ..Preface::default()
    };
    let error = preface
      .read_marker(&mut reader, MARKERS, b"ctl-ssh-")
      .await
      .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(error.to_string().contains("banner without readiness"));
  }

  #[tokio::test]
  async fn startup_noise_and_read_ahead_leave_binary_payload_untouched() {
    let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
    for (index, marker) in MARKERS.iter().enumerate() {
      let noise = b"\xff\x1b[32mWelcome\x1b[0m\r\nctl-sshctl-sshno final newline: ";
      let payload = b"\x00\xff\x80\nctl-ssh-nf\n\x1b";
      let output = [noise.as_slice(), marker, payload].concat();
      let mut reader = BufReader::new(output.as_slice());
      let mut preface = Preface::default();
      assert_eq!(
        preface
          .read_marker(&mut reader, MARKERS, b"ctl-ssh-")
          .await
          .unwrap(),
        index
      );
      let mut remaining = Vec::new();
      reader.read_to_end(&mut remaining).await.unwrap();
      assert_eq!(remaining, payload);
    }
  }

  #[tokio::test]
  async fn fragmented_markers_are_recognized_without_consuming_service_bytes() {
    let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
    let (reader, mut writer) = tokio::io::duplex(1);
    let task = tokio::spawn(async move {
      for byte in b"bannerctl-ssh-v3\n\x00\xff" {
        writer.write_all(&[*byte]).await.unwrap();
      }
    });
    let mut reader = BufReader::new(reader);
    assert_eq!(
      Preface::default()
        .read_marker(&mut reader, MARKERS, b"ctl-ssh-")
        .await
        .unwrap(),
      1
    );
    let mut remaining = Vec::new();
    reader.read_to_end(&mut remaining).await.unwrap();
    assert_eq!(remaining, b"\x00\xff");
    task.await.unwrap();
  }

  #[tokio::test]
  async fn startup_budget_is_shared_across_authentication_and_service_markers() {
    let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
    let half = vec![b'x'; MAX_STARTUP_BYTES / 2];
    let output = [
      half.as_slice(),
      b"ctl-ssh-auth-v1\n",
      half.as_slice(),
      MARKERS[0],
    ]
    .concat();
    let mut reader = BufReader::new(output.as_slice());
    let mut preface = Preface::default();
    preface
      .read_marker(&mut reader, &[b"ctl-ssh-auth-v1\n"], b"ctl-ssh-")
      .await
      .unwrap();
    let error = preface
      .read_marker(&mut reader, MARKERS, b"ctl-ssh-")
      .await
      .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("64 KiB"));
    assert!(error.to_string().len() < MAX_STARTUP_PREVIEW + 200);
  }

  #[tokio::test]
  async fn failed_startup_preview_escapes_terminal_controls_and_is_bounded() {
    let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
    let mut output = b"\x1b]52;c;secret\x07\xff".to_vec();
    output.extend(vec![b'x'; 4096]);
    let error = Preface::default()
      .read_marker(&mut output.as_slice(), MARKERS, b"ctl-ssh-")
      .await
      .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    let message = error.to_string();
    assert!(!message.contains('\x1b'));
    assert!(!message.contains('\x07'));
    assert!(message.contains("startup stdout"));
    assert!(message.len() < MAX_STARTUP_PREVIEW + 200);
  }
}

#[cfg(all(test, unix))]
mod tests {
  use super::*;
  use std::process::Stdio;
  use tokio::process::Command;

  #[tokio::test]
  async fn preserves_ssh_diagnostics_instead_of_only_reporting_eof() {
    let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
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
    let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
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
