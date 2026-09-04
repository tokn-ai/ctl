$ErrorActionPreference = 'Stop'
$rmux = (Resolve-Path 'target/debug/rmux.exe').Path
$ctl = (Resolve-Path 'target/debug/ctl.exe').Path
$rmuxd = (Resolve-Path 'target/debug/rmuxd.exe').Path
$existing = @(Get-Process rmuxd -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
$env:RMUX_RUNTIME_DIR = Join-Path ([System.IO.Path]::GetTempPath()) ('rmux-smoke-' + [guid]::NewGuid())
Remove-Item Env:RMUXD_BIN -ErrorAction SilentlyContinue

function Invoke-Rmux {
  param([string[]] $Arguments)
  & $rmux @Arguments
  if ($LASTEXITCODE -ne 0) { throw "rmux failed with exit code $LASTEXITCODE" }
}

try {
  Invoke-Rmux -Arguments @('new', '--name', 'smoke', '--', 'cmd.exe', '/D', '/Q')
  $sessions = & $ctl rmux list
  if ($LASTEXITCODE -ne 0 -or ($sessions -join "`n") -notmatch 'smoke') {
    throw 'ctl rmux did not find the session created by rmux'
  }
  Invoke-Rmux -Arguments @('state', 'smoke')
  Invoke-Rmux -Arguments @('kill', 'smoke')
} finally {
  Get-Process rmuxd -ErrorAction SilentlyContinue |
    Where-Object { $_.Id -notin $existing -and $_.Path -eq $rmuxd } |
    Stop-Process -Force
}
