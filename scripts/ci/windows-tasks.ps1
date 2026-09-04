$ErrorActionPreference = 'Stop'
$ctl = (Resolve-Path 'target/debug/ctl.exe').Path
$taskd = (Resolve-Path 'target/debug/taskd.exe').Path
$existing = @(Get-Process taskd -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
$root = Join-Path ([System.IO.Path]::GetTempPath()) ('ctl-task-smoke-' + [guid]::NewGuid())
$env:TASKD_RUNTIME_DIR = Join-Path $root 'runtime'
$env:TASKD_DATA_DIR = Join-Path $root 'data'
# Leave TASKD_BIN unset to exercise sibling taskd.exe discovery and auto-start.
Remove-Item Env:TASKD_BIN -ErrorAction SilentlyContinue

function Invoke-Ctl {
  param([string[]] $Arguments)
  & $ctl @Arguments
  if ($LASTEXITCODE -ne 0) { throw "ctl failed with exit code $LASTEXITCODE" }
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
  Get-Process taskd -ErrorAction SilentlyContinue |
    Where-Object { $_.Id -notin $existing -and $_.Path -eq $taskd } |
    Stop-Process -Force
  Remove-Item -Recurse -Force $root -ErrorAction SilentlyContinue
}
