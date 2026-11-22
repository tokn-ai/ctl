$ErrorActionPreference = 'Stop'
__ACCOUNT_GUARD__
__UPDATER_STAGING__
# ConsoleStream blocks on OpenSSH pipes; keep raw stdin open for the child.
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class CtlStdin {
  [DllImport("kernel32.dll")]
  public static extern IntPtr GetStdHandle(int kind);
}
'@
$handle = [Microsoft.Win32.SafeHandles.SafeFileHandle]::new([CtlStdin]::GetStdHandle(-10), $false)
$inputStream = [IO.FileStream]::new($handle, [IO.FileAccess]::Read, 1)
$remaining = __UPDATER_PREFIX_BYTES__
$size = __UPDATER_SIZE__
$sha = '__UPDATER_SHA256__'
$dir = $null
$stage = $null
try {
  $dir = New-UpdaterStaging
  $stage = Join-Path $dir 'ctlup.exe'
  if ($remaining) {
    $file = [IO.File]::Open($stage, 'CreateNew', 'Write', 'None')
    try {
      $buffer = [byte[]]::new(65536)
      while ($remaining -gt 0) {
        $count = $inputStream.Read($buffer, 0, [Math]::Min($remaining, $buffer.Length))
        if (!$count) { throw 'Incomplete ctlup executable.' }
        $file.Write($buffer, 0, $count)
        $remaining -= $count
      }
    } finally { $file.Dispose() }
    if (!(Test-StagedUpdater $stage $size $sha)) {
      throw 'ctlup executable failed checksum verification.'
    }
  } elseif (!(Copy-RetainedUpdater $stage $size $sha)) {
    throw 'The selected ctlup changed during the update.'
  }
  $manager = $stage
  __UPDATE_PREPARATION__
  $start = New-Object Diagnostics.ProcessStartInfo
  $start.FileName = $manager
  $start.Arguments = ('install-archive ' + ($originArgs -join ' ')).TrimEnd()
  $start.UseShellExecute = $false
  # Redirect output only so the child inherits the original binary input handle.
  $start.RedirectStandardOutput = $true
  $process = [Diagnostics.Process]::Start($start)
  try {
    $outputStream = [Console]::OpenStandardOutput()
    $process.StandardOutput.BaseStream.CopyTo($outputStream)
    $outputStream.Flush()
    $process.WaitForExit()
    exit $process.ExitCode
  } finally { $process.Dispose() }
} finally {
  $inputStream.Dispose()
  if ($stage) { Remove-Item -LiteralPath $stage -Force -ErrorAction SilentlyContinue }
  if ($dir) { [IO.Directory]::Delete($dir) }
}
