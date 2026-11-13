use std::io;
use std::path::Path;
use std::process::Stdio;

use ctl_proto::files::{GitEntry, Response};
use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::process::Command;

const MAX_GIT_BYTES: usize = 8 * 1024 * 1024;
const MAX_GIT_ENTRIES: usize = 100_000;
static GIT_PROCESSES: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(8);

pub(super) async fn status(path: &Path) -> io::Result<Response> {
  let path = super::canonical(path).await?;
  let directory = if tokio::fs::metadata(&path).await?.is_file() {
    path
      .parent()
      .ok_or_else(|| invalid("File has no parent directory"))?
  } else {
    &path
  };
  let root = run(
    directory,
    &["rev-parse", "--path-format=absolute", "--show-toplevel"],
  )
  .await?;
  let root = root.strip_suffix(b"\n").unwrap_or(&root);
  let root = std::str::from_utf8(root).map_err(|_| unsupported_path())?;
  if !Path::new(root).is_absolute() {
    return Err(invalid("Git returned a relative repository path"));
  }
  let output = run(
    Path::new(root),
    &[
      "status",
      "--porcelain=v1",
      "-z",
      "--untracked-files=normal",
      "--ignore-submodules=all",
    ],
  )
  .await?;
  let entries = parse(&output)?;
  Ok(Response::GitStatus {
    path: super::path_text(Path::new(root))?,
    entries,
  })
}

async fn run(directory: &Path, arguments: &[&str]) -> io::Result<Vec<u8>> {
  let mut command = Command::new("git");
  command
    .args([
      "--no-optional-locks",
      "-c",
      "core.fsmonitor=false",
      "-c",
      "core.untrackedCache=false",
      "-C",
    ])
    .arg(directory)
    .args(arguments);
  capture(command).await
}

async fn capture(mut command: Command) -> io::Result<Vec<u8>> {
  let permit = GIT_PROCESSES.try_acquire().map_err(|_| {
    io::Error::new(
      io::ErrorKind::WouldBlock,
      "The file service is still finishing eight Git processes; retry after one exits",
    )
  })?;
  let mut child = command
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true)
    .spawn()?;
  let stdout = child
    .stdout
    .take()
    .ok_or_else(|| invalid("Git stdout is unavailable"))?;
  let stderr = child
    .stderr
    .take()
    .ok_or_else(|| invalid("Git stderr is unavailable"))?;
  // A detached supervisor retains ownership until kill AND wait finish. Merely
  // aborting a request future cannot abandon a child awaiting OS reaping.
  let (cancel, cancelled) = tokio::sync::oneshot::channel::<()>();
  let cleanup = tokio::spawn(async move {
    // Cancellation releases request admission, but process admission remains
    // held until this supervisor completes kill and reap.
    let _permit = permit;
    let result = tokio::select! {
      biased;
      _ = cancelled => Err(io::Error::new(io::ErrorKind::Interrupted, "Git request was cancelled")),
      result = async {
        let (stdout, stderr) = tokio::try_join!(bounded(stdout, MAX_GIT_BYTES), bounded(stderr, 64 * 1024))?;
        let status = child.wait().await?;
        if !status.success() {
          return Err(io::Error::other(format!("Git status is unavailable: {}", String::from_utf8_lossy(&stderr).trim())));
        }
        Ok(stdout)
      } => result,
    };
    if result.is_err() {
      let _ = child.kill().await;
      let _ = child.wait().await;
    }
    result
  });
  let result = cleanup.await.map_err(io::Error::other)?;
  // Keep the sender alive through the await. Dropping the caller while awaiting
  // drops this sender and wakes the supervisor's cancellation branch.
  drop(cancel);
  result
}

async fn bounded(reader: impl AsyncRead + Unpin, limit: usize) -> io::Result<Vec<u8>> {
  let mut output = Vec::new();
  reader
    .take((limit + 1) as u64)
    .read_to_end(&mut output)
    .await?;
  if output.len() > limit {
    return Err(invalid("Git status exceeds its output byte limit"));
  }
  Ok(output)
}

fn parse(output: &[u8]) -> io::Result<Vec<GitEntry>> {
  let mut records = output.split(|byte| *byte == 0).peekable();
  let mut entries = Vec::new();
  while let Some(record) = records.next() {
    if record.is_empty() {
      if records.peek().is_some() {
        return Err(invalid("Git returned an empty status record"));
      }
      break;
    }
    if entries.len() >= MAX_GIT_ENTRIES || record.len() < 4 || record[2] != b' ' {
      return Err(invalid(
        "Git returned an invalid or excessive status catalog",
      ));
    }
    let path = std::str::from_utf8(&record[3..]).map_err(|_| unsupported_path())?;
    if path.is_empty() || path.len() > ctl_proto::files::MAX_PATH_BYTES {
      return Err(unsupported_path());
    }
    let renamed = record[..2]
      .iter()
      .any(|status| matches!(status, b'R' | b'C'));
    let original_path = if renamed {
      let original = records
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid("Git rename has no original path"))?;
      let original = std::str::from_utf8(original).map_err(|_| unsupported_path())?;
      if original.len() > ctl_proto::files::MAX_PATH_BYTES {
        return Err(unsupported_path());
      }
      Some(original.into())
    } else {
      None
    };
    entries.push(GitEntry {
      path: path.into(),
      original_path,
      index_status: char::from(record[0]).to_string(),
      worktree_status: char::from(record[1]).to_string(),
    });
  }
  if !output.is_empty() && output.last() != Some(&0) {
    return Err(invalid("Git returned a truncated status record"));
  }
  Ok(entries)
}

fn invalid(message: &str) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, message)
}
fn unsupported_path() -> io::Error {
  io::Error::new(
    io::ErrorKind::Unsupported,
    "Git paths must be bounded valid UTF-8",
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parses_nul_records_and_rename_order_without_unquoting_paths() {
    let entries = parse(b" M path with space\0R  new\nname\0old name\0?? untracked\0").unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].path, "path with space");
    assert_eq!(entries[0].worktree_status, "M");
    assert_eq!(entries[1].path, "new\nname");
    assert_eq!(entries[1].original_path.as_deref(), Some("old name"));
    assert_eq!(entries[2].index_status, "?");
  }

  #[test]
  fn refuses_truncated_renames_and_invalid_unicode() {
    assert!(parse(b"R  new\0").is_err());
    assert!(parse(b"?? \xff\0").is_err());
    assert!(parse(b"?? truncated").is_err());
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn cancelling_a_process_request_kills_and_reaps_the_owned_child() {
    use std::time::Duration;
    let marker = std::env::temp_dir().join(format!("ctl-files-git-cancel-{}", std::process::id()));
    let _ = tokio::fs::remove_file(&marker).await;
    let mut command = Command::new("sh");
    command
      .args([
        "-c",
        "printf '%s' \"$$\" > \"$1\"; exec sleep 60",
        "fixture",
      ])
      .arg(&marker);
    let request = tokio::spawn(capture(command));
    let pid = tokio::time::timeout(Duration::from_secs(3), async {
      loop {
        if let Ok(pid) = tokio::fs::read_to_string(&marker).await
          && !pid.is_empty()
        {
          break pid;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
      }
    })
    .await
    .unwrap();
    request.abort();
    let _ = request.await;
    tokio::time::timeout(Duration::from_secs(3), async {
      loop {
        let alive = Command::new("kill")
          .args(["-0", &pid])
          .stdout(Stdio::null())
          .stderr(Stdio::null())
          .status()
          .await
          .unwrap()
          .success();
        if !alive {
          break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
      }
    })
    .await
    .unwrap();
    tokio::fs::remove_file(marker).await.unwrap();
  }
}
