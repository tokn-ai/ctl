use super::{
  UpdaterUpload, managed_installation, report_reused, require_updater_success,
  run_package_install_with_prefix, run_updater_command,
};
use crate::{
  CoreError, RemoteInstallEvent, RemotePlatform, SSH_PROGRAM, SshConnectionOptions, SshInteraction,
  configure_ssh_interaction, prepare_ssh_base_arguments, validate_ssh_target,
};
use base64::Engine as _;
use sha2::{Digest as _, Sha256};
use std::io::{self, Write as _};
use std::time::Duration;
use tokio::process::Command;

const MANAGER: &str = r"$manager = Join-Path $env:USERPROFILE '.tokn\ctl\bin\ctlup.exe'";
const INSTALL_SCRIPT: &str = include_str!("windows_install.ps1");
const MARKER: &[u8] = b"ctl-command-v1\n";
const READ_ACCOUNT_ID: &str = r"
$file = [IO.File]::OpenRead((Join-Path $env:USERPROFILE '.tokn\ctl\remote-id'))
try {
  if ($file.Length -ne 36) { throw 'Invalid ctl account identity.' }
  $reader = [IO.StreamReader]::new($file, [Text.Encoding]::ASCII, $false)
  $account = $reader.ReadToEnd()
} finally { $file.Dispose() }
";

fn powershell_command(script: &str) -> io::Result<String> {
  let command = encoded_powershell_command(script);
  if command.len() < 8000 {
    return Ok(command);
  }
  // Windows OpenSSH commonly starts commands through cmd.exe. Compress long
  // scripts rather than exceeding its command-line limit or changing stdin,
  // which must remain the original binary installation stream.
  let mut compressor = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
  compressor.write_all(script.as_bytes())?;
  let encoded = base64::engine::general_purpose::STANDARD.encode(compressor.finish()?);
  let wrapper = format!(
    "$b=[Convert]::FromBase64String('{encoded}');$s=[IO.Compression.GZipStream]::new([IO.MemoryStream]::new($b),[IO.Compression.CompressionMode]::Decompress);$r=[IO.StreamReader]::new($s,[Text.Encoding]::UTF8);try{{&([ScriptBlock]::Create($r.ReadToEnd()))}}finally{{$r.Dispose()}}"
  );
  let command = encoded_powershell_command(&wrapper);
  if command.len() >= 8000 {
    return Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      "Windows SSH command exceeds its size limit",
    ));
  }
  Ok(command)
}

fn encoded_powershell_command(script: &str) -> String {
  let bytes: Vec<_> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
  let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
  format!("powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand {encoded}")
}

pub(crate) async fn windows_ssh_command(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  script: &str,
) -> Result<Command, CoreError> {
  validate_ssh_target(destination, options)?;
  if options.remote_platform != RemotePlatform::Windows {
    return Err(CoreError::InvalidSshOption("remote_platform".into()));
  }
  let mut command = Command::new(SSH_PROGRAM);
  let extra = configure_ssh_interaction(&mut command, interaction);
  command
    .args(extra)
    .args(prepare_ssh_base_arguments(destination, options, interaction).await?)
    .arg(
      powershell_command(script)
        .map_err(|error| CoreError::InvalidComponentBundle(error.to_string()))?,
    );
  Ok(command)
}

/// Detects the OS architecture of a Windows OpenSSH account.
///
/// # Errors
/// Reports SSH failures and unexpected Windows platform metadata.
pub async fn probe_ssh_windows_platform(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
) -> Result<crate::remote_bundle::Platform, CoreError> {
  let script = r#"
$arch = $env:PROCESSOR_ARCHITEW6432
if (!$arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
[Console]::Out.Write("ctl-command-v1`nctl-platform-v1`nWindows`n$arch`n")
"#;
  let command = windows_ssh_command(destination, options, interaction, script).await?;
  let output = crate::run_marked_fixed_command(command, &[], MARKER).await?;
  let output = std::str::from_utf8(&output).map_err(|_| CoreError::InvalidSshCommandOutput)?;
  crate::remote_bundle::Platform::parse_probe(output)
    .map_err(|_| CoreError::InvalidSshCommandOutput)
}

pub(crate) async fn read_remote_id(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
) -> Result<String, CoreError> {
  let script = format!(
    "$ErrorActionPreference = 'Stop'\n{READ_ACCOUNT_ID}\n[Console]::Out.Write(\"ctl-command-v1`n\" + $account)"
  );
  let command = windows_ssh_command(destination, options, interaction, &script).await?;
  let output = tokio::time::timeout(
    Duration::from_secs(8),
    crate::run_marked_fixed_command(command, &[], MARKER),
  )
  .await
  .map_err(|_| CoreError::InvalidSshCommandOutput)??;
  let id = String::from_utf8(output).map_err(|_| CoreError::InvalidSshCommandOutput)?;
  if !ctl_proto::valid_remote_id(&id) {
    return Err(CoreError::InvalidSshCommandOutput);
  }
  Ok(id)
}

pub(crate) fn account_guard(expected: Option<&str>) -> Result<String, CoreError> {
  let Some(expected) = expected else {
    return Ok(String::new());
  };
  if !ctl_proto::valid_remote_id(expected) {
    return Err(CoreError::InvalidSshCommandOutput);
  }
  Ok(format!(
    "{READ_ACCOUNT_ID}\nif ($account -cne '{expected}') {{ throw 'The Windows SSH account identity changed.' }}"
  ))
}

pub(super) async fn install(
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
  let (manifest, layout) = managed_installation(archive, bundle_id)?;
  if manifest.target_triple != crate::remote_bundle::WINDOWS_TARGET
    || updater.is_empty()
    || updater.len() > ctl_core::bundle_layout::MAX_FILE_BYTES
  {
    return Err(CoreError::InvalidComponentBundle(
      "expected a complete Windows bundle and updater".into(),
    ));
  }
  let digest = format!("{:x}", Sha256::digest(updater));
  let account_guard = account_guard(expected_account)?;
  let origin_arguments = origin_arguments(origin)?;
  let preflight = format!(
    r#"$ErrorActionPreference = 'Stop'
{account_guard}
{MANAGER}
if (![IO.File]::Exists($manager)) {{ exit 11 }}
if ((& $manager updater-digest) -ne '{digest}') {{ exit 11 }}
{origin_arguments}
$confirmation = & $manager activate-installed '{}' @originArgs
$code = $LASTEXITCODE
if ($code -eq 0) {{
  if ($confirmation -cne 'ctl-install-v1') {{ throw 'ctlup did not confirm activation.' }}
  [Console]::Out.Write("ctl-install-v1`n")
}}
exit $code"#,
    manifest.bundle_id,
  );
  let command = windows_ssh_command(destination, options, interaction, &preflight).await?;
  let (status, stdout, stderr) = run_updater_command(command, &[], |_| {}).await?;
  let prefix = match status.code() {
    Some(0) => {
      require_updater_success(status, &stdout, &stderr, b"ctl-install-v1\n")?;
      report_reused(archive, layout, &on_progress);
      return Ok(());
    }
    Some(10) => &[],
    Some(11) => updater,
    _ => return require_updater_success(status, &stdout, &stderr, b"ctl-install-v1\n"),
  };
  let script = install_script(&digest, prefix.len(), &account_guard, &origin_arguments);
  let command = windows_ssh_command(destination, options, interaction, &script).await?;
  run_package_install_with_prefix(command, prefix, archive, layout.names(), on_progress).await
}

fn origin_arguments(
  origin: Option<&ctl_core::installations::CatalogOrigin>,
) -> Result<String, CoreError> {
  let Some(origin) = origin else {
    return Ok("$originArgs = @()".into());
  };
  let encoded = crate::installation::encode_catalog_origin(origin)
    .map_err(|error| CoreError::InvalidComponentBundle(error.to_string()))?;
  Ok(format!(
    r#"$originArgs = @()
$origin = '{encoded}'
$savedPreference = $ErrorActionPreference
$ErrorActionPreference = 'Continue'
$probe = & $manager catalog-json 2>&1
$probeCode = $LASTEXITCODE
$ErrorActionPreference = $savedPreference
if ($probeCode -eq 0) {{ $originArgs = @('--origin-hex', $origin) }}
elseif ($probeCode -ne 2) {{ throw ('ctlup catalog probe failed: ' + ($probe -join "`n")) }}"#
  ))
}

fn install_script(
  digest: &str,
  prefix_size: usize,
  account_guard: &str,
  origin_arguments: &str,
) -> String {
  INSTALL_SCRIPT
    .replace("__ACCOUNT_GUARD__", account_guard)
    .replace("__MANAGER__", MANAGER)
    .replace("__UPDATER_SIZE__", &prefix_size.to_string())
    .replace("__UPDATER_SHA256__", digest)
    .replace("__ORIGIN_ARGUMENTS__", origin_arguments)
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::io::Read as _;

  #[test]
  fn installer_fits_in_the_default_windows_ssh_shell_command_line() {
    let guard = account_guard(Some("aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa")).unwrap();
    let script = install_script(
      &"a".repeat(64),
      ctl_core::bundle_layout::MAX_FILE_BYTES,
      &guard,
      &origin_arguments(None).unwrap(),
    );
    let command = powershell_command(&script).unwrap();
    assert!(command.len() < 8000);
    assert!(!script.contains("__"));
  }

  #[test]
  fn maximal_provenance_uses_lossless_compression_without_changing_installer_input() {
    let origin = ctl_core::installations::CatalogOrigin {
      channel: "pr-18446744073709551615".into(),
      publication_id: Some("a".repeat(256)),
      built_at: Some("2026-01-02T03:04:05.123456789Z".into()),
    };
    let guard = account_guard(Some("aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa")).unwrap();
    let script = install_script(
      &"a".repeat(64),
      ctl_core::bundle_layout::MAX_FILE_BYTES,
      &guard,
      &origin_arguments(Some(&origin)).unwrap(),
    );
    assert!(encoded_powershell_command(&script).len() >= 8000);
    let command = powershell_command(&script).unwrap();
    assert!(command.len() < 8000);
    let encoded = command.rsplit_once(' ').unwrap().1;
    let bytes = base64::engine::general_purpose::STANDARD
      .decode(encoded)
      .unwrap();
    let utf16: Vec<_> = bytes
      .as_chunks::<2>()
      .0
      .iter()
      .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
      .collect();
    let wrapper = String::from_utf16(&utf16).unwrap();
    let compressed = wrapper
      .split_once("FromBase64String('")
      .unwrap()
      .1
      .split_once('\'')
      .unwrap()
      .0;
    let compressed = base64::engine::general_purpose::STANDARD
      .decode(compressed)
      .unwrap();
    let mut decoded = String::new();
    flate2::read::GzDecoder::new(compressed.as_slice())
      .read_to_string(&mut decoded)
      .unwrap();
    assert_eq!(decoded, script);
  }
}
