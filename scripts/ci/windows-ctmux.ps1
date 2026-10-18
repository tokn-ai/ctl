$ErrorActionPreference = 'Stop'
$ctmux = (Resolve-Path 'target/debug/ctmux.exe').Path
$ctl = (Resolve-Path 'target/debug/ctl.exe').Path
$ctmuxd = (Resolve-Path 'target/debug/ctmuxd.exe').Path
$existing = @(Get-Process ctmuxd -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
$env:CTMUX_RUNTIME_DIR = Join-Path ([System.IO.Path]::GetTempPath()) ('ctmux-smoke-' + [guid]::NewGuid())
Remove-Item Env:CTMUXD_BIN -ErrorAction SilentlyContinue

function Invoke-Native {
  param([string]$Program, [string[]]$Arguments)
  # Capturing the short-lived CLI also checks that its detached daemon does not
  # inherit unrelated output handles and keep these streams open after exit.
  $start = [Diagnostics.ProcessStartInfo]::new($Program)
  $start.UseShellExecute = $false
  $start.WorkingDirectory = (Get-Location).ProviderPath
  $start.RedirectStandardInput = $true
  $start.RedirectStandardOutput = $true
  $start.RedirectStandardError = $true
  foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
  $process = [Diagnostics.Process]::new()
  $process.StartInfo = $start
  $started = $false
  try {
    $started = $process.Start()
    if (-not $started) { throw "Could not start $Program" }
    $stdout = $process.StandardOutput.ReadToEndAsync()
    $stderr = $process.StandardError.ReadToEndAsync()
    $process.StandardInput.Close()
    if (-not $process.WaitForExit(45000)) { throw "$Program $($Arguments -join ' ') timed out" }
    if (-not $stdout.Wait(5000) -or -not $stderr.Wait(5000)) {
      throw "$Program $($Arguments -join ' ') left its captured streams open after exit"
    }
    if ($process.ExitCode -ne 0) {
      throw "$Program exited $($process.ExitCode):`n$($stdout.Result)`n$($stderr.Result)"
    }
    if ($stderr.Result.Length -gt 0) { [Console]::Error.Write($stderr.Result) }
    if ($stdout.Result.Length -gt 0) { return $stdout.Result -split '\r?\n' }
  } finally {
    if ($started -and -not $process.HasExited) {
      $process.Kill($true)
      $process.WaitForExit(5000) | Out-Null
    }
    $process.Dispose()
  }
}

function Invoke-Ctmux {
  param([string[]]$Arguments)
  Invoke-Native -Program $ctmux -Arguments $Arguments
}

try {
  Invoke-Ctmux -Arguments @('new', '-d', '--name', 'smoke', '--', 'cmd.exe', '/D', '/Q')
  $sessions = Invoke-Native -Program $ctl -Arguments @('ctmux', 'list')
  if (($sessions -join "`n") -notmatch 'smoke') {
    throw 'ctl ctmux did not find the session created by ctmux'
  }
  Invoke-Ctmux -Arguments @('state', 'smoke')
  Invoke-Ctmux -Arguments @('kill', 'smoke')
} finally {
  Get-Process ctmuxd -ErrorAction SilentlyContinue |
    Where-Object { $_.Id -notin $existing -and $_.Path -eq $ctmuxd } |
    Stop-Process -Force
}
