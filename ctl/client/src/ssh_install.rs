use std::io;
use std::process::Stdio;

use tokio::io::{
  AsyncBufReadExt as _, AsyncRead, AsyncReadExt as _, AsyncWriteExt as _, BufReader,
};
use tokio::process::Command;

use crate::{
  CoreError, RemotePlatform, SSH_PROGRAM, SshConnectionOptions, SshInteraction,
  configure_ssh_interaction, prepare_ssh_base_arguments, read_bounded_output, validate_ssh_target,
};

const UNIX_INSTALL_COMMAND: &str = include_str!("ssh_install.sh");
#[cfg(unix)]
const AGENT_INSTALL_COMMAND: &str = include_str!("ssh_agent_install.sh");
const MAX_PROGRESS_LINE_BYTES: u64 = 256;
const INITIAL_PROGRESS_MARKER: &[u8] = b"ctl-install-progress-v1 receiving 0\n";

mod progress;
pub use progress::{
  RemoteInstallPhase, RemoteInstallProgress, RemoteInstallStalled, RemoteInstallWatchdog,
};

/// Receiver-confirmed installation progress from the fixed remote script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteInstallEvent {
  Receiving { received_bytes: u64 },
  Extracting,
  Checking { file_name: &'static str },
  Activating,
  Complete,
}

/// Installs only a verified agent, retaining the active companion paths.
///
/// # Errors
/// Rejects invalid bytes, unsafe paths, SSH failures or invalid progress.
#[cfg(unix)]
pub async fn install_ssh_agent_only_with_progress(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  archive: &[u8],
  on_progress: impl Fn(RemoteInstallEvent) + Send + Sync,
) -> Result<(), CoreError> {
  validate_ssh_target(destination, options)?;
  if options.remote_platform != RemotePlatform::Unix {
    return Err(CoreError::InvalidSshOption("remote_platform".into()));
  }
  let script = agent_script(archive, None)?;
  let mut command = Command::new(SSH_PROGRAM);
  let extra = configure_ssh_interaction(&mut command, interaction);
  command
    .args(extra)
    .args(prepare_ssh_base_arguments(destination, options, interaction).await?)
    .arg(posix_install_command(&script));
  run_package_install(command, archive, true, on_progress).await
}

#[cfg(unix)]
pub(crate) async fn install_local_agent(
  home: &std::path::Path,
  companions: Option<&std::path::Path>,
  archive: &[u8],
) -> Result<(), CoreError> {
  let script = agent_script(archive, companions)?;
  let mut command = Command::new("sh");
  command.args(["-c", &script]).env("HOME", home);
  run_package_install(command, archive, true, |_| {}).await
}

#[cfg(unix)]
pub(crate) async fn install_local_bundle(
  home: &std::path::Path,
  bundle: &ctl_core::bundles::Bundle,
) -> Result<(), CoreError> {
  let bundle = bundle.clone();
  let upload = tokio::task::spawn_blocking(move || crate::components::package_bundle(&bundle))
    .await
    .map_err(|error| CoreError::InvalidComponentBundle(error.to_string()))?
    .map_err(|error| CoreError::InvalidComponentBundle(error.to_string()))?;
  let script = installation_script(&upload.bundle_id, &upload.archive)?;
  let mut command = Command::new("sh");
  command.args(["-c", &script]).env("HOME", home);
  run_install_command(command, &upload.archive, |_| {}).await
}

#[cfg(unix)]
fn agent_script(archive: &[u8], companions: Option<&std::path::Path>) -> Result<String, CoreError> {
  use sha2::{Digest as _, Sha256};
  let source = crate::component_update::inspect_agent_archive(archive)
    .map_err(|error| CoreError::InvalidComponentBundle(error.to_string()))?;
  let companions = companions.map_or_else(
    || Ok(String::new()),
    |path| {
      path
        .to_str()
        .map(str::to_owned)
        .ok_or(CoreError::InvalidSshOption("companion_directory".into()))
    },
  )?;
  let quoted = format!("'{}'", companions.replace('\'', "'\\''"));
  Ok(
    AGENT_INSTALL_COMMAND
      .replace("__BUNDLE_TARGET__", &source.source_bundle.target_triple)
      .replace("__STORE_ID__", &source.source_bundle.bundle_id)
      .replace("__ARCHIVE_BYTES__", &archive.len().to_string())
      .replace(
        "__ARCHIVE_SHA256__",
        &format!("{:x}", Sha256::digest(archive)),
      )
      .replace("__COMPANION_DIRECTORY__", &quoted),
  )
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
  let script = installation_script(bundle_id, archive)?;
  let mut command = Command::new(SSH_PROGRAM);
  let extra = configure_ssh_interaction(&mut command, interaction);
  command
    .args(extra)
    .args(prepare_ssh_base_arguments(destination, options, interaction).await?)
    .arg(posix_install_command(&script));
  run_install_command(command, archive, on_progress).await
}

fn posix_install_command(script: &str) -> String {
  // OpenSSH passes its command to the account's login shell. Only let that
  // shell parse this launcher: the installers rely on POSIX octal arithmetic,
  // traps and redirections, which differ in shells such as zsh and fish.
  format!("exec sh -c '{}'", script.replace('\'', "'\\''"))
}

fn install_script(bundle_id: &str, archive_bytes: usize) -> Result<String, CoreError> {
  build_install_script(bundle_id, archive_bytes, None)
}

fn build_install_script(
  bundle_id: &str,
  archive_bytes: usize,
  managed: Option<(&str, &str, &str)>,
) -> Result<String, CoreError> {
  if bundle_id.is_empty()
    || bundle_id.len() > 128
    || !bundle_id
      .bytes()
      .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'+'))
  {
    return Err(CoreError::InvalidAgentBundleId(bundle_id.into()));
  }
  let (target, store_id, digest) = managed.unwrap_or(("", "", ""));
  Ok(
    UNIX_INSTALL_COMMAND
      .replace("__BUNDLE_ID__", bundle_id)
      .replace("__ARCHIVE_BYTES__", &archive_bytes.to_string())
      .replace("__MANAGED__", if managed.is_some() { "yes" } else { "no" })
      .replace("__BUNDLE_TARGET__", target)
      .replace("__STORE_ID__", store_id)
      .replace("__ARCHIVE_SHA256__", digest),
  )
}

fn installation_script(bundle_id: &str, archive: &[u8]) -> Result<String, CoreError> {
  #[cfg(unix)]
  if let Some(manifest) = crate::components::inspect_upload_archive(archive)
    .map_err(|error| CoreError::InvalidComponentBundle(error.to_string()))?
  {
    use sha2::{Digest as _, Sha256};
    let expected = manifest
      .distribution_id
      .as_ref()
      .unwrap_or(&manifest.bundle_id);
    if expected != bundle_id {
      return Err(CoreError::InvalidAgentBundleId(bundle_id.into()));
    }
    let digest = format!("{:x}", Sha256::digest(archive));
    return build_install_script(
      bundle_id,
      archive.len(),
      Some((&manifest.target_triple, &manifest.bundle_id, &digest)),
    );
  }
  install_script(bundle_id, archive.len())
}

async fn run_install_command(
  command: Command,
  archive: &[u8],
  on_progress: impl Fn(RemoteInstallEvent),
) -> Result<(), CoreError> {
  run_package_install(command, archive, false, on_progress).await
}

async fn run_package_install(
  mut command: Command,
  archive: &[u8],
  agent_only: bool,
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
    let result = stdin.write_all(archive).await;
    drop(stdin);
    result
  };
  let (write, progress, stderr, status) = tokio::join!(
    write,
    read_package_progress(stdout, archive.len() as u64, agent_only, &on_progress),
    read_bounded_output(&mut stderr),
    child.wait(),
  );
  let stderr = stderr.map_err(CoreError::ReadSshCommand)?;
  let status = status.map_err(CoreError::WaitSshCommand)?;
  // Prefer remote diagnostics to a broken stdin pipe after an early SSH exit.
  if !status.success() {
    let mut diagnostic = String::from_utf8_lossy(&stderr)
      .chars()
      .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
      .collect::<String>()
      .trim()
      .to_owned();
    if diagnostic.is_empty() {
      diagnostic = "remote component installer exited without reporting a reason".into();
    }
    return Err(CoreError::SshCommandFailed {
      status: status.to_string(),
      diagnostic,
    });
  }
  write.map_err(CoreError::WriteSshCommand)?;
  progress.map_err(CoreError::ReadSshCommand)?;
  Ok(())
}

#[cfg(test)]
async fn read_progress(
  stdout: impl AsyncRead + Unpin,
  total_bytes: u64,
  on_progress: &impl Fn(RemoteInstallEvent),
) -> io::Result<()> {
  read_package_progress(stdout, total_bytes, false, on_progress).await
}

async fn read_package_progress(
  stdout: impl AsyncRead + Unpin,
  total_bytes: u64,
  agent_only: bool,
  on_progress: &impl Fn(RemoteInstallEvent),
) -> io::Result<()> {
  let mut reader = BufReader::new(stdout);
  let mut preface = crate::ssh_startup::Preface::default();
  if let Err(error) = preface
    .read_marker(&mut reader, &[INITIAL_PROGRESS_MARKER], b"ctl-install-")
    .await
  {
    // Preserve the framing error, but keep draining so a verbose remote script
    // cannot block on stdout while the caller waits for its exit diagnostics.
    let _ = tokio::io::copy(&mut reader, &mut tokio::io::sink()).await;
    return Err(error);
  }
  on_progress(RemoteInstallEvent::Receiving { received_bytes: 0 });
  let mut received_bytes = 0;
  let stages = [
    RemoteInstallEvent::Extracting,
    RemoteInstallEvent::Checking {
      file_name: "ctl-agent",
    },
    RemoteInstallEvent::Checking {
      file_name: "ctmuxd",
    },
    RemoteInstallEvent::Checking {
      file_name: "ctl-taskd",
    },
    RemoteInstallEvent::Checking { file_name: "ctld" },
    RemoteInstallEvent::Activating,
    RemoteInstallEvent::Complete,
  ];
  let agent_stages = [stages[0], stages[1], stages[5], stages[6]];
  let stages = if agent_only {
    &agent_stages[..]
  } else {
    &stages[..]
  };
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
    "ctl-install-progress-v1 checking ctmuxd\n" => Some(RemoteInstallEvent::Checking {
      file_name: "ctmuxd",
    }),
    "ctl-install-progress-v1 checking ctl-taskd\n" => Some(RemoteInstallEvent::Checking {
      file_name: "ctl-taskd",
    }),
    "ctl-install-progress-v1 checking ctld\n" => {
      Some(RemoteInstallEvent::Checking { file_name: "ctld" })
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
