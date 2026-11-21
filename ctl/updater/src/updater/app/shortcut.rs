use std::io;
use std::os::windows::process::CommandExt as _;
use std::path::Path;
use std::process::{Command, Stdio};

// Explorer follows the native shortcut straight into the installed GUI
// executable. A copied console ctlup executable would open a console window.
pub(super) fn create(destination: &Path, target: &Path) -> io::Result<()> {
  let script = r"
$ErrorActionPreference = 'Stop'
$destination = [IO.Path]::GetFullPath($env:CTLUP_DESKTOP_SHORTCUT_PATH)
$target = [IO.Path]::GetFullPath($env:CTLUP_DESKTOP_SHORTCUT_TARGET)
if (![IO.Directory]::Exists([IO.Path]::GetDirectoryName($destination))) {
  throw 'The staged shortcut directory does not exist.'
}
if (![IO.File]::Exists($target)) { throw 'The desktop executable does not exist.' }
$link = (New-Object -ComObject WScript.Shell).CreateShortcut($destination)
$link.TargetPath = $target
$link.WorkingDirectory = [IO.Path]::GetDirectoryName($target)
$link.Save()
";
  run(script, destination, target)?;
  let info = std::fs::symlink_metadata(destination)?;
  if !ctl_core::private_fs::is_owned_file(&info) {
    return Err(io::Error::other("desktop shortcut is not an owned file"));
  }
  Ok(())
}

pub(super) fn managed(shortcut: &Path, store: &Path) -> io::Result<bool> {
  let script = r"
$ErrorActionPreference = 'Stop'
$destination = [IO.Path]::GetFullPath($env:CTLUP_DESKTOP_SHORTCUT_PATH)
$link = (New-Object -ComObject WScript.Shell).CreateShortcut($destination)
if (!$link.TargetPath) { exit }
$target = [IO.Path]::GetFullPath($link.TargetPath)
$prefix = [IO.Path]::GetFullPath($env:CTLUP_DESKTOP_SHORTCUT_TARGET).TrimEnd('\') + '\'
if ($target.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase) -and
    $target.Substring($prefix.Length) -cmatch '^[a-f0-9]{64}\\ctmux\\ctmux-app\.exe$') {
  [Console]::Out.Write('managed')
}
";
  Ok(run(script, shortcut, store)? == b"managed")
}

fn run(script: &str, destination: &Path, target: &Path) -> io::Result<Vec<u8>> {
  const CREATE_NO_WINDOW: u32 = 0x0800_0000;
  let system_root = std::env::var_os("SystemRoot")
    .ok_or_else(|| io::Error::other("Windows system directory is unavailable"))?;
  let powershell = Path::new(&system_root).join("System32/WindowsPowerShell/v1.0/powershell.exe");
  let output = Command::new(powershell)
    .creation_flags(CREATE_NO_WINDOW)
    .args([
      "-NoLogo",
      "-NoProfile",
      "-NonInteractive",
      "-Command",
      script,
    ])
    .env("CTLUP_DESKTOP_SHORTCUT_PATH", destination)
    .env("CTLUP_DESKTOP_SHORTCUT_TARGET", target)
    .stdin(Stdio::null())
    .output()?;
  if output.status.success() {
    return Ok(output.stdout);
  }
  Err(io::Error::other(format!(
    "could not prepare the ctmux desktop shortcut: {}",
    String::from_utf8_lossy(&output.stderr).trim()
  )))
}
