use ctl_core::bundle_layout::{ALL, Layout};
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

#[cfg(test)]
const UNIX_INSTALL_COMMAND: &str =
  include_str!("ssh_install/test_fixtures/legacy_bundle_install.sh");
const UPDATER_BOOTSTRAP_COMMAND: &str = include_str!("ssh_updater_bootstrap.sh");
#[cfg(all(test, unix))]
const AGENT_INSTALL_COMMAND: &str =
  include_str!("ssh_install/test_fixtures/legacy_agent_install.sh");
const MAX_PROGRESS_LINE_BYTES: u64 = 256;
const INITIAL_PROGRESS_MARKER: &[u8] = b"ctl-install-progress-v1 receiving 0\n";

mod progress;
mod windows;
pub use progress::{
  RemoteInstallPhase, RemoteInstallProgress, RemoteInstallStalled, RemoteInstallWatchdog,
};
pub(crate) use windows::account_guard as windows_account_guard;
pub use windows::probe_ssh_windows_platform;
pub(crate) use windows::{read_remote_id, windows_ssh_command};

/// SSH bootstrap and receiver-confirmed runtime installation progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteInstallEvent {
  /// Bytes written to SSH stdin while bootstrapping the target updater.
  SendingUpdater {
    sent_bytes: u64,
    total_bytes: u64,
  },
  /// The updater input has closed and the remote shell is checking it.
  CheckingUpdater,
  Receiving {
    received_bytes: u64,
  },
  Extracting,
  Checking {
    file_name: &'static str,
  },
  Activating,
  Complete,
}

#[cfg(all(test, unix))]
pub(crate) async fn install_local_agent(
  home: &std::path::Path,
  companions: Option<&std::path::Path>,
  archive: &[u8],
) -> Result<(), CoreError> {
  let script = agent_script(archive, companions)?;
  let mut command = Command::new("sh");
  command.args(["-c", &script]).env("HOME", home);
  run_package_install(command, archive, &["ctl-agent"], |_| {}).await
}

#[cfg(all(test, unix))]
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

pub(crate) struct UpdaterUpload<'a> {
  pub updater: &'a [u8],
  pub bundle_id: &'a str,
  pub archive: &'a [u8],
  pub expected_account: Option<&'a str>,
  pub origin: Option<&'a ctl_core::installations::CatalogOrigin>,
}

/// Installs a complete build through the independent target-native updater.
/// The receiver verifies a retained installation before deciding whether any
/// runtime archive bytes need to cross SSH.
///
/// # Errors
/// Rejects invalid packages, unsafe bootstrap paths, or failed SSH operations.
pub(crate) async fn install_ssh_bundle_with_updater(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  upload: UpdaterUpload<'_>,
  on_progress: impl Fn(RemoteInstallEvent) + Send + Sync,
) -> Result<(), CoreError> {
  match options.remote_platform {
    RemotePlatform::Unix => {
      install_unix_bundle_with_updater(destination, options, interaction, upload, on_progress).await
    }
    RemotePlatform::Windows => {
      windows::install(destination, options, interaction, upload, on_progress).await
    }
  }
}

/// Installs a complete bundle on a POSIX host through its native updater.
///
/// # Errors
/// Rejects an invalid package, unexpected platform, or failed SSH operation.
#[cfg(unix)]
pub async fn install_ssh_unix_bundle_with_updater(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  updater: &[u8],
  bundle_id: &str,
  archive: &[u8],
  on_progress: impl Fn(RemoteInstallEvent) + Send + Sync,
) -> Result<(), CoreError> {
  let upload = UpdaterUpload {
    updater,
    bundle_id,
    archive,
    expected_account: None,
    origin: None,
  };
  install_unix_bundle_with_updater(destination, options, interaction, upload, on_progress).await
}

async fn install_unix_bundle_with_updater(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  upload: UpdaterUpload<'_>,
  on_progress: impl Fn(RemoteInstallEvent) + Send + Sync,
) -> Result<(), CoreError> {
  let UpdaterUpload {
    updater,
    bundle_id,
    archive,
    expected_account,
    origin,
  } = upload;
  validate_ssh_target(destination, options)?;
  if options.remote_platform != RemotePlatform::Unix {
    return Err(CoreError::InvalidSshOption("remote_platform".into()));
  }
  // This verifies the entire managed archive before executing anything remotely.
  let (manifest, layout) = managed_installation(archive, bundle_id)?;
  let guard = unix_account_guard(expected_account)?;
  let preflight = format!("{guard}{}", updater_bootstrap_script(updater, true)?);
  let command = updater_ssh_command(destination, options, interaction, &preflight).await?;
  let (status, stdout, stderr) = run_updater_command(command, &[], |_| {}).await?;
  if status.code() == Some(10) {
    let bootstrap = format!("{guard}{}", updater_bootstrap_script(updater, false)?);
    let command = updater_ssh_command(destination, options, interaction, &bootstrap).await?;
    let (status, stdout, stderr) = run_updater_command(command, updater, &on_progress).await?;
    require_updater_success(status, &stdout, &stderr, b"ctlup-bootstrap-v1\n")?;
  } else {
    require_updater_success(status, &stdout, &stderr, b"ctlup-bootstrap-v1\n")?;
  }
  let activation = unix_updater_invocation(
    &format!("activate-installed '{}'", manifest.bundle_id),
    origin,
  )?;
  let script = format!("{guard}{activation}");
  let command = updater_ssh_command(destination, options, interaction, &script).await?;
  let (status, stdout, stderr) = run_updater_command(command, &[], |_| {}).await?;
  if status.success() {
    require_updater_success(status, &stdout, &stderr, b"ctl-install-v1\n")?;
    report_reused(archive, layout, &on_progress);
    return Ok(());
  }
  if status.code() != Some(10) {
    return require_updater_success(status, &stdout, &stderr, b"ctl-install-v1\n");
  }
  let command = updater_ssh_command(
    destination,
    options,
    interaction,
    &format!(
      "{guard}{}",
      unix_updater_invocation("install-archive", origin)?
    ),
  )
  .await?;
  run_package_install(command, archive, layout.names(), on_progress).await
}

fn unix_updater_invocation(
  operation: &str,
  origin: Option<&ctl_core::installations::CatalogOrigin>,
) -> Result<String, CoreError> {
  let mut script = format!("manager=\"$HOME/.tokn/ctl/bin/ctlup\"\nset -- {operation}\n");
  if let Some(origin) = origin {
    let encoded = crate::installation::encode_catalog_origin(origin)
      .map_err(|error| CoreError::InvalidComponentBundle(error.to_string()))?;
    script = format!(
      "{script}if \"$manager\" catalog-json </dev/null >/dev/null; then\n  set -- \"$@\" --origin-hex '{encoded}'\nelse\n  status=$?\n  [ \"$status\" -eq 2 ] || exit \"$status\"\nfi\n"
    );
  }
  script.push_str("exec \"$manager\" \"$@\"");
  Ok(script)
}

pub(crate) fn unix_account_guard(expected: Option<&str>) -> Result<String, CoreError> {
  let Some(expected) = expected else {
    return Ok(String::new());
  };
  if !ctl_proto::valid_remote_id(expected) {
    return Err(CoreError::InvalidSshCommandOutput);
  }
  // Bound the read and preserve trailing newlines during command substitution.
  Ok(format!(
    "identity=\"$HOME/.tokn/ctl/remote-id\"\nif ! [ -f \"$identity\" ] || ! [ \"$(wc -c < \"$identity\")\" -eq 36 ] || [ \"$(dd if=\"$identity\" bs=1 count=36 2>/dev/null; printf x)\" != '{expected}x' ]; then printf 'The SSH account identity changed.\\n' >&2; exit 1; fi\n"
  ))
}

fn report_reused(archive: &[u8], layout: Layout, on_progress: &impl Fn(RemoteInstallEvent)) {
  on_progress(RemoteInstallEvent::Receiving { received_bytes: 0 });
  on_progress(RemoteInstallEvent::Receiving {
    received_bytes: archive.len() as u64,
  });
  on_progress(RemoteInstallEvent::Extracting);
  for &file_name in layout.names() {
    on_progress(RemoteInstallEvent::Checking { file_name });
  }
  on_progress(RemoteInstallEvent::Activating);
  on_progress(RemoteInstallEvent::Complete);
}

fn managed_installation(
  archive: &[u8],
  bundle_id: &str,
) -> Result<(ctl_core::bundles::Manifest, Layout), CoreError> {
  let invalid = |error: io::Error| CoreError::InvalidComponentBundle(error.to_string());
  let mut files = crate::remote_bundle::archive::read_archive(archive).map_err(invalid)?;
  let manifest = crate::components::inspect_upload_archive(&mut files)
    .map_err(|error| CoreError::InvalidComponentBundle(error.to_string()))?
    .ok_or_else(|| invalid(io::Error::other("ctlup requires a complete managed bundle")))?;
  if manifest.bundle_id != bundle_id && manifest.distribution_id.as_deref() != Some(bundle_id) {
    return Err(CoreError::InvalidAgentBundleId(bundle_id.into()));
  }
  let layout = Layout::managed(manifest.schema_version).expect("verified managed schema");
  Ok((manifest, layout))
}

fn updater_bootstrap_script(updater: &[u8], check_only: bool) -> Result<String, CoreError> {
  use sha2::{Digest as _, Sha256};
  if updater.is_empty() || updater.len() > ctl_core::bundle_layout::MAX_FILE_BYTES {
    return Err(CoreError::InvalidComponentBundle(
      "invalid ctlup executable size".into(),
    ));
  }
  Ok(
    UPDATER_BOOTSTRAP_COMMAND
      .replace("__CHECK_ONLY__", if check_only { "yes" } else { "no" })
      .replace("__TOKEN__", &uuid::Uuid::new_v4().to_string())
      .replace("__UPDATER_BYTES__", &updater.len().to_string())
      .replace(
        "__UPDATER_SHA256__",
        &format!("{:x}", Sha256::digest(updater)),
      ),
  )
}

async fn updater_ssh_command(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  script: &str,
) -> Result<Command, CoreError> {
  let mut command = Command::new(SSH_PROGRAM);
  let extra = configure_ssh_interaction(&mut command, interaction);
  command
    .args(extra)
    .args(prepare_ssh_base_arguments(destination, options, interaction).await?)
    .arg(posix_install_command(script));
  Ok(command)
}

async fn run_updater_command(
  mut command: Command,
  input: &[u8],
  on_progress: impl Fn(RemoteInstallEvent),
) -> Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>), CoreError> {
  command
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
  let mut child = command.spawn().map_err(CoreError::StartSsh)?;
  let mut stdin = child.stdin.take().ok_or(CoreError::MissingSshStdin)?;
  let mut stdout = child.stdout.take().ok_or(CoreError::MissingSshStdout)?;
  let mut stderr = child.stderr.take().ok_or(CoreError::MissingSshStderr)?;
  let write = async move { write_updater(&mut stdin, input, &on_progress).await };
  let (write, stdout, stderr, status) = tokio::join!(
    write,
    read_updater_output(&mut stdout),
    read_bounded_output(&mut stderr),
    child.wait()
  );
  let status = status.map_err(CoreError::WaitSshCommand)?;
  if status.success() {
    write.map_err(CoreError::WriteSshCommand)?;
  }
  Ok((
    status,
    stdout.map_err(CoreError::ReadSshCommand)?,
    stderr.map_err(CoreError::ReadSshCommand)?,
  ))
}

async fn write_updater(
  stdin: &mut (impl tokio::io::AsyncWrite + Unpin),
  bytes: &[u8],
  on_progress: &impl Fn(RemoteInstallEvent),
) -> io::Result<()> {
  let total_bytes = bytes.len() as u64;
  let mut sent = 0;
  while sent < bytes.len() {
    on_progress(RemoteInstallEvent::SendingUpdater {
      sent_bytes: sent as u64,
      total_bytes,
    });
    let end = (sent + 64 * 1024).min(bytes.len());
    let count = stdin.write(&bytes[sent..end]).await?;
    if count == 0 {
      return Err(io::ErrorKind::WriteZero.into());
    }
    sent += count;
  }
  if total_bytes != 0 {
    on_progress(RemoteInstallEvent::SendingUpdater {
      sent_bytes: total_bytes,
      total_bytes,
    });
    on_progress(RemoteInstallEvent::CheckingUpdater);
  }
  Ok(())
}

async fn read_updater_output(reader: &mut (impl AsyncRead + Unpin)) -> io::Result<Vec<u8>> {
  let mut retained = Vec::new();
  let mut buffer = [0; 1024];
  let mut oversized = false;
  loop {
    let count = reader.read(&mut buffer).await?;
    if count == 0 {
      break;
    }
    let keep = count.min((64 * 1024_usize).saturating_sub(retained.len()));
    retained.extend_from_slice(&buffer[..keep]);
    oversized |= keep != count;
  }
  if oversized {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "updater startup output exceeded 64 KiB",
    ));
  }
  Ok(retained)
}

fn require_updater_success(
  status: std::process::ExitStatus,
  stdout: &[u8],
  stderr: &[u8],
  marker: &[u8],
) -> Result<(), CoreError> {
  if !status.success() {
    let diagnostic = String::from_utf8_lossy(stderr)
      .chars()
      .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
      .collect::<String>();
    let diagnostic = diagnostic.trim();
    return Err(CoreError::SshCommandFailed {
      status: status.to_string(),
      diagnostic: if diagnostic.is_empty() {
        "remote updater exited without reporting a reason".into()
      } else {
        diagnostic.to_owned()
      },
    });
  }
  if !stdout.ends_with(marker) {
    return Err(CoreError::InvalidComponentBundle(
      "ctlup did not confirm the requested operation".into(),
    ));
  }
  Ok(())
}

fn posix_install_command(script: &str) -> String {
  // OpenSSH passes its command to the account's login shell. Only let that
  // shell parse this launcher: the installers rely on POSIX octal arithmetic,
  // traps and redirections, which differ in shells such as zsh and fish.
  format!("exec sh -c '{}'", script.replace('\'', "'\\''"))
}

#[cfg(test)]
fn install_script(bundle_id: &str, archive_bytes: usize) -> Result<String, CoreError> {
  build_install_script(bundle_id, archive_bytes, Layout::Services, None)
}

#[cfg(test)]
fn build_install_script(
  bundle_id: &str,
  archive_bytes: usize,
  layout: Layout,
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
      .replace("__COMPONENTS__", &layout.names().join(" "))
      .replace("__ARCHIVE_BYTES__", &archive_bytes.to_string())
      .replace("__MANAGED__", if managed.is_some() { "yes" } else { "no" })
      .replace("__BUNDLE_TARGET__", target)
      .replace("__STORE_ID__", store_id)
      .replace("__ARCHIVE_SHA256__", digest),
  )
}

#[cfg(all(test, unix))]
fn installation_script(bundle_id: &str, archive: &[u8]) -> Result<(String, Layout), CoreError> {
  let invalid = |error: io::Error| CoreError::InvalidComponentBundle(error.to_string());
  let mut files = crate::remote_bundle::archive::read_archive(archive).map_err(invalid)?;
  if let Some(manifest) = crate::components::inspect_upload_archive(&mut files)
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
    let layout = Layout::managed(manifest.schema_version).expect("validated managed schema");
    let script = build_install_script(
      bundle_id,
      archive.len(),
      layout,
      Some((&manifest.target_triple, &manifest.bundle_id, &digest)),
    )?;
    return Ok((script, layout));
  }
  let layout = crate::remote_bundle::archive::public_layout(&mut files).map_err(invalid)?;
  Ok((
    build_install_script(bundle_id, archive.len(), layout, None)?,
    layout,
  ))
}

#[cfg(all(test, unix))]
async fn run_install_command(
  command: Command,
  archive: &[u8],
  on_progress: impl Fn(RemoteInstallEvent),
) -> Result<(), CoreError> {
  run_package_install(command, archive, Layout::Services.names(), on_progress).await
}

async fn run_package_install(
  command: Command,
  archive: &[u8],
  components: &[&'static str],
  on_progress: impl Fn(RemoteInstallEvent),
) -> Result<(), CoreError> {
  run_package_install_with_prefix(command, &[], archive, components, on_progress).await
}

async fn run_package_install_with_prefix(
  mut command: Command,
  updater: &[u8],
  archive: &[u8],
  components: &[&'static str],
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
  let report = &on_progress;
  let write = async move {
    write_updater(&mut stdin, updater, report).await?;
    let result = stdin.write_all(archive).await;
    drop(stdin);
    result
  };
  let (write, progress, stderr, status) = tokio::join!(
    write,
    read_package_progress(stdout, archive.len() as u64, components, &on_progress),
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
  read_package_progress(stdout, total_bytes, Layout::Services.names(), on_progress).await
}

async fn read_package_progress(
  stdout: impl AsyncRead + Unpin,
  total_bytes: u64,
  components: &[&'static str],
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
  let stages: Vec<_> = std::iter::once(RemoteInstallEvent::Extracting)
    .chain(
      components
        .iter()
        .map(|&file_name| RemoteInstallEvent::Checking { file_name }),
    )
    .chain([RemoteInstallEvent::Activating, RemoteInstallEvent::Complete])
    .collect();
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
    _ if line.starts_with("ctl-install-progress-v1 checking ") => {
      let name = line
        .strip_prefix("ctl-install-progress-v1 checking ")?
        .strip_suffix('\n')?;
      ALL
        .into_iter()
        .find(|&file_name| file_name == name)
        .map(|file_name| RemoteInstallEvent::Checking { file_name })
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
