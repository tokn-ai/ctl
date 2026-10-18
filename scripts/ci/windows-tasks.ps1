$ErrorActionPreference = 'Stop'
$ctl = (Resolve-Path 'target/debug/ctl.exe').Path
$ctlTaskd = (Resolve-Path 'target/debug/ctl-taskd.exe').Path
$existing = @(Get-Process ctl-taskd -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
$root = Join-Path ([System.IO.Path]::GetTempPath()) ('ctl-task-smoke-' + [guid]::NewGuid())
$env:CTL_TASKD_RUNTIME_DIR = Join-Path $root 'runtime'
$env:CTL_TASKD_DATA_DIR = Join-Path $root 'data'
# Leave CTL_TASKD_BIN unset to exercise sibling ctl-taskd.exe discovery and auto-start.
Remove-Item Env:CTL_TASKD_BIN -ErrorAction SilentlyContinue

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

function Invoke-Ctl {
  param([string[]]$Arguments)
  Invoke-Native -Program $ctl -Arguments $Arguments
}

try {
  New-Item -ItemType Directory -Path $root | Out-Null
  Push-Location $root
  try {
    Invoke-Ctl -Arguments @('taskd', 'restart')
    Invoke-Ctl -Arguments @('task', 'create', 'smoke', '--start', '--', 'cmd.exe', '/D', '/C', 'echo cli-output& cd')
    $logs = Invoke-Ctl -Arguments @('task', 'logs', 'smoke', '--follow')
    if (($logs -join "`n") -notmatch 'cli-output') { throw 'Missing task output' }
    if (($logs -join "`n") -notmatch [regex]::Escape($root)) { throw 'Wrong task working directory' }
    Invoke-Ctl -Arguments @('task', 'restart', 'smoke')
    Invoke-Ctl -Arguments @('task', 'logs', 'smoke', '--follow')
    Invoke-Ctl -Arguments @('taskd', 'restart')
    Invoke-Ctl -Arguments @('task', 'show', 'smoke')
    Invoke-Ctl -Arguments @('task', 'remove', 'smoke')
    Invoke-Ctl -Arguments @('task', 'create', 'long-running', '--start', '--', 'cmd.exe', '/D', '/C', 'ping -n 30 127.0.0.1 >nul')
    Invoke-Ctl -Arguments @('task', 'stop', 'long-running')
    Invoke-Ctl -Arguments @('task', 'remove', 'long-running')
    Invoke-Ctl -Arguments @('task', 'list')
  } finally {
    Pop-Location
  }
} finally {
  Get-Process ctl-taskd -ErrorAction SilentlyContinue |
    Where-Object { $_.Id -notin $existing -and $_.Path -eq $ctlTaskd } |
    Stop-Process -Force
  Remove-Item -Recurse -Force $root -ErrorAction SilentlyContinue
}
