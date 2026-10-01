$ErrorActionPreference = 'Stop'
$ctmux = (Resolve-Path 'target/debug/ctmux.exe').Path
$ctl = (Resolve-Path 'target/debug/ctl.exe').Path
$ctmuxd = (Resolve-Path 'target/debug/ctmuxd.exe').Path
$existing = @(Get-Process ctmuxd -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
$env:CTMUX_RUNTIME_DIR = Join-Path ([System.IO.Path]::GetTempPath()) ('ctmux-smoke-' + [guid]::NewGuid())
Remove-Item Env:CTMUXD_BIN -ErrorAction SilentlyContinue

function Invoke-Ctmux {
  param([string[]] $Arguments)
  & $ctmux @Arguments
  if ($LASTEXITCODE -ne 0) { throw "ctmux failed with exit code $LASTEXITCODE" }
}

try {
  Invoke-Ctmux -Arguments @('new', '-d', '--name', 'smoke', '--', 'cmd.exe', '/D', '/Q')
  $sessions = & $ctl ctmux list
  if ($LASTEXITCODE -ne 0 -or ($sessions -join "`n") -notmatch 'smoke') {
    throw 'ctl ctmux did not find the session created by ctmux'
  }
  Invoke-Ctmux -Arguments @('state', 'smoke')
  Invoke-Ctmux -Arguments @('kill', 'smoke')
} finally {
  Get-Process ctmuxd -ErrorAction SilentlyContinue |
    Where-Object { $_.Id -notin $existing -and $_.Path -eq $ctmuxd } |
    Stop-Process -Force
}
