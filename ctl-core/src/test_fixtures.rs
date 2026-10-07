//! Opt-in support for tests that create commands or copy native executables.
//!
//! Prefer `shell_command` for fake commands. Tests that need real executable
//! files must hold a [`ProcessGuard`] from before the first write until every
//! child has exited. All other subprocess tests in the same test binary must
//! participate too: fork can inherit another test's writable file descriptor.

use std::io;
use std::path::Path;
use tokio::sync::{Mutex, MutexGuard};

static PROCESSES: Mutex<()> = Mutex::const_new(());

/// Coordinates executable writes and subprocess lifetimes in one test process.
pub struct ProcessGuard {
  _guard: MutexGuard<'static, ()>,
}

impl ProcessGuard {
  /// Acquire without blocking an asynchronous test's executor.
  pub async fn acquire() -> Self {
    Self {
      _guard: PROCESSES.lock().await,
    }
  }

  /// Acquire in a synchronous test. Do not call inside a Tokio runtime.
  pub fn acquire_blocking() -> Self {
    Self {
      _guard: PROCESSES.blocking_lock(),
    }
  }

  /// Copy a real executable while subprocess creation is excluded.
  ///
  /// # Errors
  /// Returns filesystem errors from reading the source or writing the destination.
  pub fn copy(&self, source: impl AsRef<Path>, destination: impl AsRef<Path>) -> io::Result<u64> {
    std::fs::copy(source, destination)
  }
}

/// Create a fake Unix command without writing an executable inode.
///
/// The immutable launcher sources a separate, non-executable script, preserving
/// the command's `$0`, arguments, environment, stdin, and exit status. Do not use
/// this for tests of executable identity, canonical paths, or permissions: the
/// command is a symlink to a shared launcher and must never be chmodded.
///
/// # Errors
/// Returns filesystem errors from writing the script or creating the symlink.
#[cfg(unix)]
pub fn shell_command(path: impl AsRef<Path>, script: impl AsRef<[u8]>) -> io::Result<()> {
  let path = path.as_ref();
  let mut script_path = path.as_os_str().to_os_string();
  script_path.push(".script");
  std::fs::write(script_path, script)?;
  std::os::unix::fs::symlink(
    concat!(env!("CARGO_MANIFEST_DIR"), "/src/test-fixture-launcher.sh"),
    path,
  )
}

#[cfg(all(test, unix))]
mod tests {
  use super::*;
  use std::future::Future as _;
  use std::io::Write as _;
  use std::task::{Context, Poll, Waker};

  struct Directory(std::path::PathBuf);
  impl Directory {
    fn new() -> Self {
      Self(std::env::temp_dir().join(format!("ctl-fixture-{}", uuid::Uuid::new_v4())))
    }
    fn create() -> Self {
      let directory = Self::new();
      std::fs::create_dir(&directory.0).unwrap();
      directory
    }
  }
  impl Drop for Directory {
    fn drop(&mut self) {
      std::fs::remove_dir_all(&self.0).unwrap();
    }
  }

  #[test]
  fn immutable_launcher_runs_with_a_writable_script_and_preserves_command_semantics() {
    let _guard = ProcessGuard::acquire_blocking();
    let directory = Directory::create();
    let command = directory.0.join("command with ' spaces");
    shell_command(
      &command,
      b"printf '%s\\n' \"$0\" \"$1\" \"$FIXTURE_VALUE\"; cat; exit 17\n",
    )
    .unwrap();
    let mut script_path = command.as_os_str().to_os_string();
    script_path.push(".script");
    let _writer = std::fs::OpenOptions::new()
      .write(true)
      .open(script_path)
      .unwrap();
    let mut child = std::process::Command::new(&command)
      .arg("argument with spaces")
      .env("FIXTURE_VALUE", "value")
      .stdin(std::process::Stdio::piped())
      .stdout(std::process::Stdio::piped())
      .spawn()
      .unwrap();
    child.stdin.take().unwrap().write_all(b"input\n").unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(17));
    assert_eq!(
      String::from_utf8(output.stdout).unwrap(),
      format!(
        "{}\nargument with spaces\nvalue\ninput\n",
        command.display()
      )
    );
  }

  #[tokio::test]
  async fn native_launch_waits_until_the_writer_and_guard_are_released() {
    let directory = Directory::create();
    let executable = directory.0.join("echo");
    let guard = ProcessGuard::acquire().await;
    guard
      .copy(std::env::current_exe().unwrap(), &executable)
      .unwrap();
    let writer = std::fs::OpenOptions::new()
      .write(true)
      .open(&executable)
      .unwrap();
    let mut launch = Box::pin(async {
      let _guard = ProcessGuard::acquire().await;
      std::process::Command::new(&executable)
        .args([
          "--exact",
          "test_fixtures::tests::native_fixture_child",
          "--nocapture",
        ])
        .output()
        .unwrap()
    });
    let waker = Waker::noop();
    assert!(matches!(
      launch.as_mut().poll(&mut Context::from_waker(waker)),
      Poll::Pending
    ));
    drop(writer);
    drop(guard);
    let output = tokio::time::timeout(std::time::Duration::from_secs(5), launch)
      .await
      .expect("launch did not resume after the write guard was released");
    assert!(output.status.success(), "{output:?}");
    assert!(
      String::from_utf8(output.stdout)
        .unwrap()
        .contains("native fixture ready")
    );
  }

  #[test]
  fn native_fixture_child() {
    println!("native fixture ready");
  }

  #[cfg(target_os = "linux")]
  #[tokio::test]
  async fn bypassing_coordination_with_a_writable_native_binary_causes_etxtbsy() {
    let guard = ProcessGuard::acquire().await;
    let directory = Directory::create();
    let executable = directory.0.join("echo");
    guard.copy("/bin/echo", &executable).unwrap();
    let _writer = std::fs::OpenOptions::new()
      .write(true)
      .open(&executable)
      .unwrap();
    let error = std::process::Command::new(&executable)
      .output()
      .unwrap_err();
    assert_eq!(error.raw_os_error(), Some(26), "{error}");
  }
}
