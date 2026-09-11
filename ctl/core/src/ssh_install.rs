use std::io;
use std::process::Stdio;

use tokio::io::{
  AsyncBufReadExt as _, AsyncRead, AsyncReadExt as _, AsyncWriteExt as _, BufReader,
};
use tokio::process::Command;

use crate::{
  CoreError, RemotePlatform, SSH_PROGRAM, SshConnectionOptions, SshInteraction,
  configure_ssh_interaction, read_bounded_output, ssh_base_arguments, validate_ssh_target,
};

const UNIX_INSTALL_COMMAND: &str = include_str!("ssh_install.sh");
const MAX_PROGRESS_LINE_BYTES: u64 = 256;

/// Receiver-confirmed installation progress from the fixed remote script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteInstallEvent {
  Receiving { received_bytes: u64 },
  Extracting,
  Checking { file_name: &'static str },
  Activating,
  Complete,
}

/// Installs a trusted bundle into the fixed per-user Unix location.
///
/// # Errors
/// Returns validation, SSH startup, remote-command, or output failures.
pub async fn install_ssh_unix_agent_interactive(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  bundle_id: &str,
  archive: &[u8],
) -> Result<(), CoreError> {
  install_ssh_unix_agent_interactive_with_progress(
    destination,
    options,
    interaction,
    bundle_id,
    archive,
    |_| {},
  )
  .await
}

/// Installs a trusted bundle and reports bytes received and remote stages.
///
/// Bundle IDs are path-safe build identifiers; callers cannot supply a remote
/// command or installation path. Dropping the future terminates the SSH child.
///
/// # Errors
/// Returns validation, SSH startup, remote-command, or malformed-progress errors.
pub async fn install_ssh_unix_agent_interactive_with_progress(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  bundle_id: &str,
  archive: &[u8],
  on_progress: impl Fn(RemoteInstallEvent) + Send + Sync,
) -> Result<(), CoreError> {
  validate_ssh_target(destination, options)?;
  if options.remote_platform != RemotePlatform::Unix {
    return Err(CoreError::InvalidSshOption("remote_platform".into()));
  }
  let script = install_script(bundle_id, archive.len())?;
  let mut command = Command::new(SSH_PROGRAM);
  let extra = configure_ssh_interaction(&mut command, interaction);
  command
    .args(extra)
    .args(ssh_base_arguments(destination, options))
    .arg(script);
  run_install_command(command, archive, on_progress).await
}

fn install_script(bundle_id: &str, archive_bytes: usize) -> Result<String, CoreError> {
  if bundle_id.is_empty()
    || bundle_id.len() > 128
    || !bundle_id
      .bytes()
      .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'+'))
  {
    return Err(CoreError::InvalidAgentBundleId(bundle_id.into()));
  }
  Ok(
    UNIX_INSTALL_COMMAND
      .replace("__BUNDLE_ID__", bundle_id)
      .replace("__ARCHIVE_BYTES__", &archive_bytes.to_string()),
  )
}

async fn run_install_command(
  mut command: Command,
  archive: &[u8],
  on_progress: impl Fn(RemoteInstallEvent),
) -> Result<(), CoreError> {
  command
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
  let mut child = command.spawn().map_err(CoreError::StartSsh)?;
  let mut stdin = child.stdin.take().ok_or(CoreError::MissingSshStdin)?;
  let stdout = child.stdout.take().ok_or(CoreError::MissingSshStdout)?;
  let mut stderr = child.stderr.take().ok_or(CoreError::MissingSshStderr)?;
  let write = async move {
    stdin.write_all(archive).await?;
    stdin.shutdown().await
  };
  let (write, progress, stderr, status) = tokio::join!(
    write,
    read_progress(stdout, archive.len() as u64, &on_progress),
    read_bounded_output(&mut stderr),
    child.wait(),
  );
  let stderr = stderr.map_err(CoreError::ReadSshCommand)?;
  let status = status.map_err(CoreError::WaitSshCommand)?;
  // Prefer remote diagnostics to a broken stdin pipe after an early SSH exit.
  if !status.success() {
    let diagnostic = String::from_utf8_lossy(&stderr)
      .chars()
      .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
      .collect::<String>()
      .trim()
      .to_owned();
    return Err(CoreError::SshCommandFailed {
      status: status.to_string(),
      diagnostic,
    });
  }
  write.map_err(CoreError::WriteSshCommand)?;
  progress.map_err(CoreError::ReadSshCommand)?;
  Ok(())
}

async fn read_progress(
  stdout: impl AsyncRead + Unpin,
  total_bytes: u64,
  on_progress: &impl Fn(RemoteInstallEvent),
) -> io::Result<()> {
  let mut reader = BufReader::new(stdout);
  let mut received_bytes = 0;
  let stages = [
    RemoteInstallEvent::Extracting,
    RemoteInstallEvent::Checking {
      file_name: "ctl-agent",
    },
    RemoteInstallEvent::Checking { file_name: "rmuxd" },
    RemoteInstallEvent::Checking { file_name: "taskd" },
    RemoteInstallEvent::Activating,
    RemoteInstallEvent::Complete,
  ];
  let mut next_stage = 0;
  let mut invalid = false;
  loop {
    let mut line = Vec::new();
    let count = (&mut reader)
      .take(MAX_PROGRESS_LINE_BYTES)
      .read_until(b'\n', &mut line)
      .await?;
    if count == 0 {
      break;
    }
    let event = std::str::from_utf8(&line).ok().and_then(parse_progress);
    match event {
      Some(RemoteInstallEvent::Receiving {
        received_bytes: next,
      }) if !invalid && next_stage == 0 && next >= received_bytes && next <= total_bytes => {
        received_bytes = next;
        on_progress(RemoteInstallEvent::Receiving { received_bytes });
      }
      Some(event)
        if !invalid && received_bytes == total_bytes && stages.get(next_stage) == Some(&event) =>
      {
        next_stage += 1;
        on_progress(event);
      }
      _ => invalid = true,
    }
    // Keep draining bounded chunks even after malformed output so the child
    // cannot block on a full stdout pipe while we collect its exit diagnostics.
  }
  if next_stage != stages.len() || invalid {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "invalid remote installation progress",
    ));
  }
  Ok(())
}

fn parse_progress(line: &str) -> Option<RemoteInstallEvent> {
  match line {
    "ctl-install-v1\n" => Some(RemoteInstallEvent::Complete),
    "ctl-install-progress-v1 extracting\n" => Some(RemoteInstallEvent::Extracting),
    "ctl-install-progress-v1 activating\n" => Some(RemoteInstallEvent::Activating),
    "ctl-install-progress-v1 checking ctl-agent\n" => Some(RemoteInstallEvent::Checking {
      file_name: "ctl-agent",
    }),
    "ctl-install-progress-v1 checking rmuxd\n" => {
      Some(RemoteInstallEvent::Checking { file_name: "rmuxd" })
    }
    "ctl-install-progress-v1 checking taskd\n" => {
      Some(RemoteInstallEvent::Checking { file_name: "taskd" })
    }
    _ => line
      .strip_prefix("ctl-install-progress-v1 receiving ")?
      .strip_suffix('\n')?
      .parse()
      .ok()
      .map(|received_bytes| RemoteInstallEvent::Receiving { received_bytes }),
  }
}

#[cfg(test)]
mod tests;
