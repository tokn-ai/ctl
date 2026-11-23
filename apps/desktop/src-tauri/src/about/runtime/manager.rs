//! Select a compatible native ctlup and run it without a terminal.

use super::SavedTarget;
use crate::error::{CommandErrorDto, CommandResult};
use ctl_core::component_update::BundleChannel;
use ctl_core::installations::{Installations, Lease};
use process_wrap::tokio::{CommandWrap, KillOnDrop};
use std::{
  io,
  path::{Path, PathBuf},
  process::Stdio,
  time::Duration,
};
use tokio::{
  io::{AsyncRead, AsyncReadExt as _},
  process::Command,
  sync::watch,
};

// The first storage layout exposed by the source-tracking ctlup commands.
const MIN_TRACKED_UPDATE_LAYOUT: u16 = 7;
const MAX_DIAGNOSTIC_BYTES: usize = 8 * 1024;

#[derive(Clone, Copy)]
pub(super) enum ManagerAction<'a> {
  Status(&'a SavedTarget),
  Update(Option<&'a SavedTarget>),
  Fetch {
    target_triple: &'a str,
    channel: BundleChannel,
  },
}

pub(super) async fn run_manager(
  home: &Path,
  cancelled: watch::Receiver<bool>,
  action: ManagerAction<'_>,
  deadline: Duration,
) -> CommandResult<Vec<u8>> {
  let (path, _lease) = selected_manager(home, cancelled.clone(), deadline).await?;
  run_compatible_manager(&path, action, deadline, cancelled)
    .await
    .map_err(|error| {
      if error.kind() == io::ErrorKind::Interrupted {
        CommandErrorDto::new("runtime_operation_cancelled", error.to_string())
      } else {
        CommandErrorDto::backend(error)
      }
    })
}

async fn selected_manager(
  home: &Path,
  mut cancelled: watch::Receiver<bool>,
  deadline: Duration,
) -> CommandResult<(PathBuf, Option<Lease>)> {
  let root = std::env::var_os("CARGO_INSTALL_ROOT")
    .or_else(|| std::env::var_os("CARGO_HOME"))
    .map_or_else(|| home.join(".cargo"), PathBuf::from);
  let explicit = std::env::var_os("CTLUP_BIN").map(PathBuf::from);
  let home = home.to_owned();
  let manager = tokio::task::spawn_blocking(move || resolve_manager(&home, explicit, &root));
  tokio::select! {
    result = manager => result.map_err(CommandErrorDto::backend)?,
    _ = cancelled.wait_for(|stop| *stop) => Err(CommandErrorDto::new("runtime_operation_cancelled", "The runtime operation was cancelled.")),
    () = tokio::time::sleep(deadline) => Err(CommandErrorDto::backend("Timed out waiting for the local ctlup installation.")),
  }
}

async fn run_compatible_manager(
  path: &Path,
  action: ManagerAction<'_>,
  deadline: Duration,
  cancelled: watch::Receiver<bool>,
) -> io::Result<Vec<u8>> {
  inspect_manager(path, &cancelled).await?;
  let mut command = Command::new(path);
  let target = match action {
    ManagerAction::Status(target) => {
      command.arg("status");
      Some(target)
    }
    ManagerAction::Update(target) => {
      command.arg("update");
      target
    }
    ManagerAction::Fetch {
      target_triple,
      channel,
    } => {
      command.args([
        "bundle",
        "fetch",
        "--target",
        target_triple,
        "--channel",
        channel.name(),
      ]);
      None
    }
  };
  command.arg("--json");
  if let Some(target) = target {
    command
      .arg(format!("--host={}", target.host))
      .arg(format!("--method={}", target.method))
      .env("CTL_HOSTS_PATH", target.catalog.path());
  }
  let update_host =
    matches!(action, ManagerAction::Update(_)).then(|| target.map_or("local", |t| t.host.as_str()));
  run(command, deadline, cancelled, update_host).await
}

fn resolve_manager(
  home: &Path,
  explicit: Option<PathBuf>,
  cargo_root: &Path,
) -> CommandResult<(PathBuf, Option<Lease>)> {
  let candidate = if let Some(path) = explicit {
    path
  } else {
    if let Some((path, lease)) = Installations::new(home)
      .manager_and_lease()
      .map_err(CommandErrorDto::backend)?
    {
      return Ok((path, Some(lease)));
    }
    let path = cargo_root
      .join("bin")
      .join(format!("ctlup{}", std::env::consts::EXE_SUFFIX));
    if !path.try_exists().map_err(CommandErrorDto::backend)? {
      return Err(CommandErrorDto::new(
        "ctlup_unavailable",
        "Install ctlup with cargo install ctlup, or set CTLUP_BIN to its absolute path, then reopen the app.",
      ));
    }
    path
  };
  if !candidate.is_absolute() {
    return Err(CommandErrorDto::backend(
      "Select an absolute path to ctlup.",
    ));
  }
  let path = candidate.canonicalize().map_err(CommandErrorDto::backend)?;
  if !path.is_file() {
    return Err(CommandErrorDto::backend(
      "The selected ctlup is not a regular file.",
    ));
  }
  Ok((path, None))
}

async fn probe(path: &Path, flag: &str, cancelled: &watch::Receiver<bool>) -> io::Result<Vec<u8>> {
  let mut command = Command::new(path);
  command.arg(flag);
  run(command, Duration::from_secs(5), cancelled.clone(), None).await
}

pub(super) async fn selected_version(home: &Path) -> CommandResult<String> {
  let (_stop, cancelled) = watch::channel(false);
  let (path, _lease) = selected_manager(home, cancelled.clone(), Duration::from_secs(5)).await?;
  read_version(&path, &cancelled)
    .await
    .map_err(CommandErrorDto::backend)
}

async fn read_version(path: &Path, cancelled: &watch::Receiver<bool>) -> io::Result<String> {
  let output = probe(path, "--version", cancelled).await?;
  crate::about::local::reported_binary_version(&output, "ctlup").map_err(io::Error::other)
}

async fn inspect_manager(path: &Path, cancelled: &watch::Receiver<bool>) -> io::Result<()> {
  let version = probe(path, "--version", cancelled).await?;
  if !version.starts_with(b"ctlup ") {
    return Err(io::Error::other(
      "The selected executable does not identify as ctlup.",
    ));
  }
  let unsupported = || {
    io::Error::other(format!(
      "This operation requires ctlup storage layout {MIN_TRACKED_UPDATE_LAYOUT} or later. Upgrade the managed ctlup, or set CTLUP_BIN to the absolute path of a compatible ctlup and reopen the app."
    ))
  };
  let output = probe(path, "installation-layout-version", cancelled)
    .await
    .map_err(|error| {
      if error.kind() == io::ErrorKind::TimedOut || *cancelled.borrow() {
        error
      } else {
        unsupported()
      }
    })?;
  if std::str::from_utf8(&output)
    .ok()
    .and_then(|s| s.trim().parse::<u16>().ok())
    .is_none_or(|layout| layout < MIN_TRACKED_UPDATE_LAYOUT)
  {
    return Err(unsupported());
  }
  Ok(())
}

async fn run(
  mut command: Command,
  deadline: Duration,
  mut cancelled: watch::Receiver<bool>,
  update_host: Option<&str>,
) -> io::Result<Vec<u8>> {
  command
    .env_remove("CTLD_ASKPASS")
    .env_remove("CTLD_IDENTITY_ASKPASS")
    .env("SSH_ASKPASS_REQUIRE", "never")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
  let mut wrapped = CommandWrap::from(command);
  wrapped.wrap(KillOnDrop);
  // A desktop command must never prompt on its launcher's controlling terminal.
  #[cfg(unix)]
  wrapped.wrap(process_wrap::tokio::ProcessSession);
  #[cfg(windows)]
  wrapped
    .wrap(process_wrap::tokio::CreationFlags(
      windows::Win32::System::Threading::CREATE_NO_WINDOW,
    ))
    .wrap(process_wrap::tokio::JobObject);
  if *cancelled.borrow() {
    return Err(io::Error::new(
      io::ErrorKind::Interrupted,
      "The runtime operation was cancelled.",
    ));
  }
  let mut child = wrapped.spawn()?;
  let stdout = child.stdout().take().expect("piped stdout");
  let stderr = child.stderr().take().expect("piped stderr");
  let output = async {
    tokio::try_join!(
      bounded_read(stdout, 4 * 1024 * 1024),
      diagnostic_tail(stderr),
      child.wait()
    )
  };
  let result = tokio::select! {
    result = output => result,
    () = tokio::time::sleep(deadline) => Err(io::Error::new(io::ErrorKind::TimedOut, "ctlup timed out. Refresh runtime status before retrying.")),
    _ = cancelled.changed() => Err(io::Error::new(io::ErrorKind::Interrupted, "The runtime operation was cancelled. Refresh status before retrying.")),
  };
  let (stdout, stderr, status) = match result {
    Ok(output) => output,
    Err(error) => {
      let _ = child.start_kill();
      return Err(error);
    }
  };
  if !status.success() {
    if let Some(detail) = update_host
      .and_then(|host| super::host_result(&stdout, host).ok()?.error)
      .filter(|detail| !detail.trim().is_empty() && detail.len() <= MAX_DIAGNOSTIC_BYTES)
    {
      return Err(io::Error::other(detail));
    }
    let detail = String::from_utf8_lossy(&stderr);
    return Err(io::Error::other(if detail.trim().is_empty() {
      format!("ctlup exited with {status}")
    } else {
      detail.trim().to_owned()
    }));
  }
  Ok(stdout)
}

async fn bounded_read(reader: impl AsyncRead + Unpin, limit: usize) -> io::Result<Vec<u8>> {
  let mut output = Vec::new();
  reader
    .take(limit as u64 + 1)
    .read_to_end(&mut output)
    .await?;
  if output.len() > limit {
    return Err(io::Error::other("ctlup exceeded its output limit"));
  }
  Ok(output)
}

async fn diagnostic_tail(mut reader: impl AsyncRead + Unpin) -> io::Result<Vec<u8>> {
  let mut buffer = [0; 2048];
  let mut tail = Vec::new();
  loop {
    let count = reader.read(&mut buffer).await?;
    if count == 0 {
      return Ok(tail);
    }
    tail.drain(..(tail.len() + count).saturating_sub(MAX_DIAGNOSTIC_BYTES));
    tail.extend_from_slice(&buffer[..count]);
  }
}

#[cfg(test)]
mod tests;
