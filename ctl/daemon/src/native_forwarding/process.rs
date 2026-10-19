//! Own the whole Windows SSH/ProxyCommand process tree, including on ctld death.

use std::io;
use std::process::ExitStatus;
use std::time::Duration;

#[cfg(windows)]
use process_wrap::tokio::{ChildWrapper, CommandWrap, CreationFlags, JobObject, KillOnDrop};
use tokio::process::{ChildStdin, ChildStdout, Command};
use tokio::task::JoinHandle;

pub(super) struct Process {
  #[cfg(windows)]
  inner: Box<dyn ChildWrapper>,
  #[cfg(not(windows))]
  inner: tokio::process::Child,
  diagnostics: Option<JoinHandle<String>>,
}

impl Process {
  pub(super) fn spawn(mut command: Command) -> io::Result<Self> {
    command.kill_on_drop(true);
    #[cfg(windows)]
    let mut inner = CommandWrap::from(command)
      .wrap(KillOnDrop)
      .wrap(CreationFlags(
        windows::Win32::System::Threading::CREATE_NO_WINDOW,
      ))
      // Creation stays suspended until job assignment. A ProxyCommand cannot
      // escape ownership by starting a descendant before assignment completes.
      .wrap(JobObject)
      .spawn()?;
    #[cfg(not(windows))]
    let mut inner = command.spawn()?;
    #[cfg(windows)]
    let stderr = inner.stderr().take();
    #[cfg(not(windows))]
    let stderr = inner.stderr.take();
    let diagnostics =
      stderr.map(|stderr| tokio::spawn(super::super::master_diagnostics::collect(stderr)));
    Ok(Self { inner, diagnostics })
  }

  pub(super) fn stdout(&mut self) -> Option<ChildStdout> {
    #[cfg(windows)]
    return self.inner.stdout().take();
    #[cfg(not(windows))]
    self.inner.stdout.take()
  }

  pub(super) fn stdin(&mut self) -> Option<ChildStdin> {
    #[cfg(windows)]
    return self.inner.stdin().take();
    #[cfg(not(windows))]
    self.inner.stdin.take()
  }

  pub(super) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
    self.inner.try_wait()
  }

  pub(super) async fn wait(&mut self) -> io::Result<ExitStatus> {
    // Observe the root, not all descendants: a failed ProxyCommand may leave
    // children alive. Root exit must first cause the owning job to be killed.
    #[cfg(windows)]
    return self.inner.inner_mut().wait().await;
    #[cfg(not(windows))]
    self.inner.wait().await
  }

  pub(super) async fn failure(&mut self, reason: &str) -> String {
    let _ = self.inner.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(2), self.wait()).await;
    let diagnostics = if let Some(mut task) = self.diagnostics.take() {
      if let Ok(Ok(message)) = tokio::time::timeout(Duration::from_secs(1), &mut task).await {
        message
      } else {
        task.abort();
        String::new()
      }
    } else {
      String::new()
    };
    if diagnostics.is_empty() {
      reason.to_owned()
    } else {
      format!("{reason}: {diagnostics}")
    }
  }
}

impl Drop for Process {
  fn drop(&mut self) {
    // KillOnDrop also places KILL_ON_JOB_CLOSE on the Windows Job Object;
    // kernel handle cleanup therefore kills descendants after an abrupt crash.
    let _ = self.inner.start_kill();
    if let Some(task) = &self.diagnostics {
      task.abort();
    }
  }
}
