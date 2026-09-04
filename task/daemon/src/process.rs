//! Background process ownership. PTYs belong to rmuxd, never taskd.
use std::{
  io,
  process::{ExitStatus, Stdio},
};
use task_proto::TaskDefinition;
use tokio::process::{ChildStderr, ChildStdout, Command};

#[cfg(windows)]
use process_wrap::tokio::{ChildWrapper, CommandWrap, CreationFlags, JobObject, KillOnDrop};
#[cfg(unix)]
use rustix::process::{Pid, Signal, kill_process_group};

pub struct Child {
  #[cfg(unix)]
  inner: tokio::process::Child,
  #[cfg(unix)]
  group: Option<Pid>,
  #[cfg(windows)]
  inner: Box<dyn ChildWrapper>,
}

pub fn spawn(definition: &TaskDefinition) -> io::Result<Child> {
  let mut command = Command::new(&definition.program);
  command
    .args(&definition.arguments)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
  if let Some(directory) = &definition.working_directory {
    command.current_dir(directory);
  }
  #[cfg(unix)]
  {
    let inner = command.process_group(0).kill_on_drop(true).spawn()?;
    let group = inner
      .id()
      .and_then(|id| i32::try_from(id).ok())
      .and_then(Pid::from_raw);
    Ok(Child { inner, group })
  }
  #[cfg(windows)]
  {
    // JobObject suspends creation until assignment, preventing escaping children.
    // CREATE_NO_WINDOW keeps background console programs out of the desktop.
    let inner = CommandWrap::from(command)
      .wrap(KillOnDrop)
      .wrap(CreationFlags(
        windows::Win32::System::Threading::CREATE_NO_WINDOW,
      ))
      .wrap(JobObject)
      .spawn()?;
    Ok(Child { inner })
  }
}

impl Child {
  pub fn stdout(&mut self) -> Option<ChildStdout> {
    #[cfg(unix)]
    {
      self.inner.stdout.take()
    }
    #[cfg(windows)]
    {
      self.inner.stdout().take()
    }
  }
  pub fn stderr(&mut self) -> Option<ChildStderr> {
    #[cfg(unix)]
    {
      self.inner.stderr.take()
    }
    #[cfg(windows)]
    {
      self.inner.stderr().take()
    }
  }
  pub fn start_kill(&mut self) -> io::Result<()> {
    #[cfg(unix)]
    if let Some(group) = self.group {
      let _ = kill_process_group(group, Signal::KILL);
    }
    self.inner.start_kill()
  }
  pub async fn wait(&mut self) -> io::Result<ExitStatus> {
    #[cfg(unix)]
    {
      self.inner.wait().await
    }
    #[cfg(windows)]
    {
      self.inner.inner_mut().wait().await
    }
  }
  pub fn finish(&mut self) {
    // A completed root must not leave background descendants holding log pipes.
    let _ = self.start_kill();
  }
  pub async fn terminate(&mut self) -> io::Result<ExitStatus> {
    #[cfg(unix)]
    {
      if let Some(group) = self.group {
        let _ = kill_process_group(group, Signal::TERM);
      }
      if let Ok(status) = tokio::time::timeout(std::time::Duration::from_secs(3), self.wait()).await
      {
        // The parent may exit before its children; clean up remaining group members.
        if let Some(group) = self.group {
          let _ = kill_process_group(group, Signal::KILL);
        }
        return status;
      }
    }
    self.start_kill()?;
    self.wait().await
  }
}
