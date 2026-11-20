//! Fetch an immutable publication on the destination using its installed HTTPS tools.

use std::io;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt as _;
use tokio::process::Command;
use tokio::time::{Instant, timeout, timeout_at};

const IDLE_LIMIT: Duration = Duration::from_secs(20);
const DOWNLOAD_LIMIT: Duration = Duration::from_mins(5);
// Missing tool features, DNS, connection, HTTP, incomplete transfers and
// timeouts allow a fallback. TLS verification and local I/O errors stop it.
const CURL_UNAVAILABLE: &[i32] = &[1, 2, 5, 6, 7, 18, 22, 28, 52, 56];
const WGET_UNAVAILABLE: &[i32] = &[2, 4, 8]; // unsupported flags, network, HTTP

/// The caller validates the publication URL and verifies the returned bytes.
/// A host without a working downloader can receive the same asset over SSH.
pub(crate) async fn download(url: &str, maximum: usize) -> io::Result<Option<Vec<u8>>> {
  download_with(url, maximum, Path::new("curl"), Path::new("wget")).await
}

async fn download_with(
  url: &str,
  maximum: usize,
  curl: &Path,
  wget: &Path,
) -> io::Result<Option<Vec<u8>>> {
  if !url.starts_with("https://") {
    return Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      "remote downloads require HTTPS",
    ));
  }
  // --disable must come first. A zero redirect limit makes curl reject a
  // redirect instead of accepting the redirect page as the artifact.
  let curl_args = [
    "--disable",
    "--silent",
    "--fail",
    "--globoff",
    "--proto",
    "=https",
    "--location",
    "--max-redirs",
    "0",
    "--connect-timeout",
    "8",
    "--",
    url,
  ];
  let deadline = Instant::now() + DOWNLOAD_LIMIT;
  if let Some(bytes) = fetch(curl, &curl_args, maximum, CURL_UNAVAILABLE, deadline).await? {
    return Ok(Some(bytes));
  }
  let wget_args = [
    "--no-config",
    "--no-netrc",
    "--no-hsts",
    "--quiet",
    "--tries=1",
    "--timeout=8",
    "--max-redirect=0",
    "--output-document=-",
    "--",
    url,
  ];
  if Instant::now() >= deadline {
    return Ok(None);
  }
  fetch(wget, &wget_args, maximum, WGET_UNAVAILABLE, deadline).await
}

async fn fetch(
  program: &Path,
  args: &[&str],
  maximum: usize,
  unavailable_codes: &[i32],
  deadline: Instant,
) -> io::Result<Option<Vec<u8>>> {
  let mut command = Command::new(program);
  #[cfg(windows)]
  command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
  let mut child = match command
    .args(args)
    .env_remove("WGET_ASKPASS")
    .env_remove("SSH_ASKPASS")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null())
    .kill_on_drop(true)
    .spawn()
  {
    Ok(child) => child,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error),
  };
  let mut stdout = child.stdout.take().expect("requested piped stdout");
  let result = timeout_at(deadline, async {
    let mut bytes = Vec::new();
    let mut chunk = [0; 8192];
    loop {
      let Ok(read) = timeout(IDLE_LIMIT, stdout.read(&mut chunk)).await else {
        return Ok(None);
      };
      let read = read?;
      if read == 0 {
        break;
      }
      if read > maximum - bytes.len() {
        return Err(io::Error::new(
          io::ErrorKind::InvalidData,
          "remote download exceeds its size limit",
        ));
      }
      bytes.extend_from_slice(&chunk[..read]);
    }
    let Ok(status) = timeout(IDLE_LIMIT, child.wait()).await else {
      return Ok(None);
    };
    let status = status?;
    if status.success() {
      Ok(Some(bytes))
    } else if status
      .code()
      .is_some_and(|code| unavailable_codes.contains(&code))
    {
      Ok(None)
    } else {
      Err(io::Error::other(format!(
        "remote downloader could not securely retrieve the publication ({status})"
      )))
    }
  })
  .await
  .unwrap_or(Ok(None));
  // Reap on early exit for errors and timeouts. An externally cancelled
  // future also terminates the process via kill_on_drop.
  if child.try_wait()?.is_none() {
    child.kill().await?;
  }
  result
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::installation::TemporaryDirectory;

  const URL: &str = "https://example.invalid/immutable/components.tgz";

  #[tokio::test]
  async fn unavailable_tools_allow_ssh_fallback() {
    let directory = TemporaryDirectory::new("download-test").unwrap();
    let missing = directory.0.join("missing");
    assert_eq!(
      download_with(URL, 6, &missing, &missing).await.unwrap(),
      None
    );
    assert_eq!(
      download_with("http://example.invalid", 6, &missing, &missing)
        .await
        .unwrap_err()
        .kind(),
      io::ErrorKind::InvalidInput
    );
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn wget_replaces_a_missing_or_unsuccessful_curl_and_discards_partial_output() {
    let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
    let directory = TemporaryDirectory::new("download-test").unwrap();
    let curl = directory.0.join("curl");
    let wget = directory.0.join("wget");
    ctl_core::test_fixtures::shell_command(&wget, "printf 'bundle'").unwrap();
    assert_eq!(
      download_with(URL, 6, &curl, &wget)
        .await
        .unwrap()
        .as_deref(),
      Some(&b"bundle"[..])
    );
    ctl_core::test_fixtures::shell_command(&curl, "printf 'part'; exit 7").unwrap();
    assert_eq!(
      download_with(URL, 6, &curl, &wget)
        .await
        .unwrap()
        .as_deref(),
      Some(&b"bundle"[..])
    );
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn an_oversized_response_fails_without_trying_another_downloader() {
    let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
    let directory = TemporaryDirectory::new("download-test").unwrap();
    let curl = directory.0.join("curl");
    let wget = directory.0.join("wget");
    ctl_core::test_fixtures::shell_command(&curl, "printf 'too large'").unwrap();
    ctl_core::test_fixtures::shell_command(&wget, "printf ok").unwrap();
    assert_eq!(
      download_with(URL, 3, &curl, &wget)
        .await
        .unwrap_err()
        .kind(),
      io::ErrorKind::InvalidData
    );
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn certificate_failure_stops_without_falling_back() {
    let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
    let directory = TemporaryDirectory::new("download-test").unwrap();
    let curl = directory.0.join("curl");
    let wget = directory.0.join("wget");
    ctl_core::test_fixtures::shell_command(&curl, "exit 60").unwrap();
    ctl_core::test_fixtures::shell_command(&wget, "printf 'bundle'").unwrap();
    assert!(download_with(URL, 6, &curl, &wget).await.is_err());
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn cancelling_a_download_reaps_the_tool() {
    use rustix::process::{Pid, test_kill_process};
    use tokio::time::sleep;

    let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
    let directory = TemporaryDirectory::new("download-test").unwrap();
    let curl = directory.0.join("curl");
    let marker = curl.with_extension("pid");
    let wget = directory.0.join("wget");
    ctl_core::test_fixtures::shell_command(&curl, r#"printf '%s' "$$" > "$0.pid"; exec sleep 60"#)
      .unwrap();
    let task = tokio::spawn(async move { download_with(URL, 10, &curl, &wget).await });
    let pid = timeout(Duration::from_secs(5), async {
      loop {
        if let Ok(value) = std::fs::read_to_string(&marker)
          && let Ok(raw) = value.parse()
          && let Some(pid) = Pid::from_raw(raw)
        {
          return pid;
        }
        sleep(Duration::from_millis(10)).await;
      }
    })
    .await
    .expect("downloader started");
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    timeout(Duration::from_secs(5), async {
      while test_kill_process(pid) != Err(rustix::io::Errno::SRCH) {
        sleep(Duration::from_millis(10)).await;
      }
    })
    .await
    .expect("cancelled downloader reaped");
  }
}
