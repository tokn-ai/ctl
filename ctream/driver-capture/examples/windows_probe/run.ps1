#requires -Version 5.1
[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [string]$ProbePath,
  [switch]$Interactive
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Set-StrictMode -Version Latest
$probeItem = Get-Item -LiteralPath $ProbePath
if (-not ($probeItem -is [IO.FileInfo]) -or $probeItem.Extension -ine '.exe') {
  throw 'ProbePath must name the compiled windows-probe executable.'
}
$probe = $probeItem.FullName

if (-not $Interactive) {
  & $probe
  exit $LASTEXITCODE
}

# Explicit opt-in: use the current account's existing interactive login. This
# does not request elevation, store credentials, or launch a system service.
Import-Module ScheduledTasks
$taskName = 'ctream-windows-probe-' + [guid]::NewGuid().ToString('N')
$directory = Join-Path ([IO.Path]::GetTempPath()) $taskName
$registered = $false
$exitCode = 1
try {
  New-Item -ItemType Directory -Path $directory | Out-Null
  $outputPath = Join-Path $directory 'result.json'
  $errorPath = Join-Path $directory 'error.txt'
  $exitPath = Join-Path $directory 'exit.txt'
  # Single-quoted PowerShell literals escape apostrophes by doubling them.
  $probeLiteral = $probe.Replace("'", "''")
  $outputLiteral = $outputPath.Replace("'", "''")
  $errorLiteral = $errorPath.Replace("'", "''")
  $exitLiteral = $exitPath.Replace("'", "''")
  $wrapper = @"
`$ErrorActionPreference = 'Stop'
`$ProgressPreference = 'SilentlyContinue'
`$code = 1
try {
  & '$probeLiteral' 1> '$outputLiteral' 2> '$errorLiteral'
  `$code = `$LASTEXITCODE
} catch {
  [IO.File]::WriteAllText('$errorLiteral', `$_.Exception.Message)
} finally {
  [IO.File]::WriteAllText('$exitLiteral.pending', [string]`$code)
  [IO.File]::Move('$exitLiteral.pending', '$exitLiteral')
}
exit `$code
"@
  $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($wrapper))
  $powershell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
  $arguments = '-NoLogo -NoProfile -NonInteractive -WindowStyle Hidden -EncodedCommand ' + $encoded
  $action = New-ScheduledTaskAction -Execute $powershell -Argument $arguments
  $principal = New-ScheduledTaskPrincipal `
    -UserId ([Security.Principal.WindowsIdentity]::GetCurrent().User.Value) `
    -LogonType Interactive -RunLevel Limited
  $settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::FromSeconds(30)) `
    -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
  Register-ScheduledTask -TaskName $taskName -Action $action -Principal $principal `
    -Settings $settings | Out-Null
  $registered = $true
  Start-ScheduledTask -TaskName $taskName
  $deadline = [DateTime]::UtcNow.AddSeconds(20)
  while (-not (Test-Path -LiteralPath $exitPath)) {
    if ([DateTime]::UtcNow -ge $deadline) {
      $info = Get-ScheduledTaskInfo -TaskName $taskName
      throw ('Interactive probe timed out (task result 0x{0:X8}). The current account must have an interactive login.' -f $info.LastTaskResult)
    }
    Start-Sleep -Milliseconds 100
  }
  $exitCode = [int]([IO.File]::ReadAllText($exitPath))
  if (Test-Path -LiteralPath $outputPath) {
    [Console]::Out.Write([IO.File]::ReadAllText($outputPath))
  }
  if (Test-Path -LiteralPath $errorPath) {
    [Console]::Error.Write([IO.File]::ReadAllText($errorPath))
  }
} finally {
  try {
    if ($registered) {
      Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
      Unregister-ScheduledTask -TaskName $taskName -Confirm:$false
    }
  } finally {
    if (Test-Path -LiteralPath $directory) {
      Remove-Item -LiteralPath $directory -Recurse -Force
    }
  }
}
exit $exitCode
