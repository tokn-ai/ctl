#[cfg(unix)]
use std::ffi::OsString;
use std::process::Command;

#[cfg(not(unix))]
use ctl_client::cli::connection::prepared_target_arguments;
pub use ctl_client::cli::connection::{Error, shell_quote};
use ctl_client::cli::connection::{run_process, target_arguments};
use ctl_client::hosts::ConnectionTargetDto;

/// Keep the invocation-local askpass bridge alive while OpenSSH owns the terminal.
pub async fn caller_command(
  target: &ConnectionTargetDto,
  authentication: &crate::caller_auth::CallerAuth,
  arguments: Option<&[String]>,
  platform: ctl_client::RemotePlatform,
) -> Result<i32, Error> {
  let mut command = tokio::process::Command::new("ssh");
  authentication
    .authentication
    .configure(command.as_std_mut());
  command.args(target_arguments(&target.to_ssh_target()?)?);
  if arguments.is_some() {
    command.arg("-T");
  }
  command.arg("--").arg(target.label());
  if let Some(arguments) = arguments {
    match platform {
      ctl_client::RemotePlatform::Unix => {
        command.arg(
          arguments
            .iter()
            .map(|value| shell_quote(value))
            .collect::<Vec<_>>()
            .join(" "),
        );
      }
      ctl_client::RemotePlatform::Windows => {
        command.args(arguments);
      }
    }
  }
  command
    .stdin(std::process::Stdio::inherit())
    .stdout(std::process::Stdio::inherit())
    .stderr(std::process::Stdio::inherit())
    .kill_on_drop(true);
  let mut child = command.spawn()?;
  #[cfg(unix)]
  {
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    loop {
      use nix::{
        sys::signal::{Signal, kill},
        unistd::Pid,
      };
      let signal = tokio::select! {
        status = child.wait() => {
          use std::os::unix::process::ExitStatusExt as _;
          let status = status?;
          return Ok(status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(1)));
        },
        _ = interrupt.recv() => Signal::SIGINT,
        _ = terminate.recv() => Signal::SIGTERM,
        _ = hangup.recv() => Signal::SIGHUP,
      };
      if let Some(pid) = child.id().and_then(|pid| i32::try_from(pid).ok()) {
        let _ = kill(Pid::from_raw(pid), signal);
      }
    }
  }
  #[cfg(not(unix))]
  Ok(child.wait().await?.code().unwrap_or(1))
}

pub async fn plain_shell(target: &ConnectionTargetDto) -> Result<i32, Error> {
  if target.is_local() {
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| {
      if cfg!(windows) {
        "cmd.exe".into()
      } else {
        "/bin/sh".into()
      }
    });
    return run_process(Command::new(shell));
  }
  let mut command = ssh_command(target).await?;
  command.arg(target.label());
  run_process(command)
}

pub async fn execute(
  target: &ConnectionTargetDto,
  arguments: Vec<String>,
  platform: ctl_client::RemotePlatform,
) -> Result<i32, Error> {
  if target.is_local() {
    let mut command = Command::new(&arguments[0]);
    command.args(&arguments[1..]);
    return run_process(command);
  }
  let mut command = ssh_command(target).await?;
  command.arg("-T").arg(target.label());
  match platform {
    ctl_client::RemotePlatform::Unix => {
      command.arg(
        arguments
          .iter()
          .map(|arg| shell_quote(arg))
          .collect::<Vec<_>>()
          .join(" "),
      );
    }
    ctl_client::RemotePlatform::Windows => {
      command.args(arguments);
    }
  }
  run_process(command)
}

pub async fn ssh_command(target: &ConnectionTargetDto) -> Result<Command, Error> {
  crate::target::ensure_vpn(target).await?;
  let target = target.to_ssh_target()?;
  let mut command = Command::new("ssh");
  #[cfg(unix)]
  {
    let path = crate::ssh_broker::ensure_master(target.clone()).await?;
    command.args([OsString::from("-S"), path.into_os_string()]);
    command.args(["-o", "ControlMaster=no", "-o", "ProxyCommand=false"]);
    // The owner already opened this route. Reusing it needs no proxy helper
    // executable and must not install a new bundle for an existing connection.
    let mut defaults = target.clone();
    defaults.gateways.clear();
    command.args(target_arguments(&defaults)?);
  }
  #[cfg(not(unix))]
  command.args(prepared_target_arguments(&target).await?);
  Ok(command)
}
