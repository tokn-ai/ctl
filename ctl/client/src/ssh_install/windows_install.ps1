$ErrorActionPreference = 'Stop'
__ACCOUNT_GUARD__
__MANAGER__
$inputStream = [Console]::OpenStandardInput()
$remaining = __UPDATER_SIZE__
$stage = $null
try {
  if ($remaining) {
    $path = Join-Path $env:USERPROFILE ('ctlup-' + [Guid]::NewGuid().ToString('N') + '.exe')
    $file = [IO.File]::Open($path, 'CreateNew', 'Write', 'None')
    $stage = $path
    try {
      $buffer = [byte[]]::new(65536)
      while ($remaining -gt 0) {
        $count = $inputStream.Read($buffer, 0, [Math]::Min($remaining, $buffer.Length))
        if (!$count) { throw 'Incomplete ctlup executable.' }
        $file.Write($buffer, 0, $count)
        $remaining -= $count
      }
    } finally { $file.Dispose() }
    if ((Get-FileHash -LiteralPath $stage -Algorithm SHA256).Hash -ne '__UPDATER_SHA256__') {
      throw 'ctlup executable failed checksum verification.'
    }
    $manager = $stage
  } else {
    $manager = Join-Path $env:USERPROFILE '.tokn\ctl\updaters\__UPDATER_SHA256__\ctlup.exe'
    if ((Get-FileHash -LiteralPath $manager -Algorithm SHA256).Hash -ne '__UPDATER_SHA256__') {
      throw 'The selected ctlup changed during the update.'
    }
  }
  $start = New-Object Diagnostics.ProcessStartInfo
  $start.FileName = $manager
  $start.Arguments = 'install-archive'
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
  if ($stage) { Remove-Item -LiteralPath $stage -Force -ErrorAction SilentlyContinue }
}
