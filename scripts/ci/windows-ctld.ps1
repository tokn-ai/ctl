$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$ctl = (Resolve-Path 'target/debug/ctl.exe').Path
$builtCtld = (Resolve-Path 'target/debug/ctld.exe').Path
$fixtureRoot = Join-Path ([IO.Path]::GetTempPath()) ('ctl owner smoke ' + [guid]::NewGuid())
$fixtureCtld = Join-Path $fixtureRoot 'bin\ctld.exe'
$catalog = Join-Path $fixtureRoot 'hosts.json'
$profiles = Join-Path $fixtureRoot 'vpns.json'
$owner = $null
$successor = $null
$ownerStdout = $null
$ownerStderr = $null

# Environment overrides belong only to fixture subprocesses. Copying ctld into
# a unique directory also gives cleanup an unambiguous owner after CLI restart.
$fixtureEnvironment = @{
  CTLD_BIN = $fixtureCtld
  CTLD_RUNTIME_DIR = Join-Path $fixtureRoot 'runtime'
  CTL_HOSTS_PATH = $catalog
  CTL_VPNS_PATH = $profiles
  CTMUX_RUNTIME_DIR = Join-Path $fixtureRoot 'ctmux-runtime'
  CTL_TASKD_RUNTIME_DIR = Join-Path $fixtureRoot 'task-runtime'
  CTL_TASKD_DATA_DIR = Join-Path $fixtureRoot 'task-data'
  CTMUXD_BIN = (Resolve-Path 'target/debug/ctmuxd.exe').Path
  CTL_TASKD_BIN = (Resolve-Path 'target/debug/ctl-taskd.exe').Path
  USERPROFILE = Join-Path $fixtureRoot 'profile'
  LOCALAPPDATA = Join-Path $fixtureRoot 'local'
  APPDATA = Join-Path $fixtureRoot 'roaming'
  # Missing Docker/Podman must not prevent owner startup or status requests.
  PATH = Join-Path $fixtureRoot 'empty-path'
}

function New-FixtureStartInfo {
  param([string]$Program, [string[]]$Arguments)
  $start = [Diagnostics.ProcessStartInfo]::new($Program)
  $start.UseShellExecute = $false
  $start.CreateNoWindow = $true
  $start.WorkingDirectory = $fixtureRoot
  $start.RedirectStandardInput = $true
  $start.RedirectStandardOutput = $true
  $start.RedirectStandardError = $true
  foreach ($entry in $fixtureEnvironment.GetEnumerator()) {
    $start.Environment[$entry.Key] = $entry.Value
  }
  foreach ($name in @('CTLD_SOCKET_PATH', 'CTLD_ASKPASS', 'CTLD_IDENTITY_ASKPASS')) {
    $start.Environment.Remove($name) | Out-Null
  }
  foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
  return $start
}

function Invoke-Ctl {
  param([string[]]$Arguments, [int]$TimeoutSeconds = 45)
  $process = [Diagnostics.Process]::new()
  $process.StartInfo = New-FixtureStartInfo $ctl $Arguments
  $started = $false
  try {
    $started = $process.Start()
    if (-not $started) { throw 'Could not start ctl' }
    $stdout = $process.StandardOutput.ReadToEndAsync()
    $stderr = $process.StandardError.ReadToEndAsync()
    $process.StandardInput.Close()
    if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
      throw "ctl $($Arguments -join ' ') timed out"
    }
    if (-not $stdout.Wait(5000) -or -not $stderr.Wait(5000)) {
      throw "ctl $($Arguments -join ' ') left its output streams open"
    }
    if ($process.ExitCode -ne 0) {
      throw "ctl $($Arguments -join ' ') exited $($process.ExitCode):`n$($stdout.Result)`n$($stderr.Result)"
    }
    return ConvertFrom-Json $stdout.Result
  } finally {
    if ($started -and -not $process.HasExited) {
      $process.Kill($true)
      $process.WaitForExit(5000) | Out-Null
    }
    $process.Dispose()
  }
}

function Get-CtldRow {
  $status = Invoke-Ctl -Arguments @('components', 'status', '--json') -TimeoutSeconds 15
  $rows = @($status.components | Where-Object { $_.component -eq 'ctld' })
  if ($status.host -ne 'local' -or $rows.Count -ne 1) {
    throw 'Component status did not identify exactly one local ctld'
  }
  if (@($rows[0].errors).Count -ne 0) {
    throw "ctld status errors: $($rows[0].errors -join '; ')"
  }
  return $rows[0]
}

function Assert-EmptyVpnInventory {
  param($Inventory)
  if (@($Inventory.connections).Count -ne 0 -or @($Inventory.entries).Count -ne 0) {
    throw 'Isolated VPN inventory contains unexpected connections or profiles'
  }
}

try {
  foreach ($directory in @('bin', 'profile', 'local', 'roaming', 'empty-path')) {
    New-Item -ItemType Directory -Path (Join-Path $fixtureRoot $directory) -Force | Out-Null
  }
  Copy-Item $builtCtld $fixtureCtld

  Write-Host 'Checking passive Windows owner and VPN observations'
  if ((Get-CtldRow).state -ne 'not_running') { throw 'Fresh ctld namespace already has an owner' }
  Assert-EmptyVpnInventory (Invoke-Ctl -Arguments @('vpn', 'list', '--json'))
  if ((Get-CtldRow).state -ne 'not_running') { throw 'Passive VPN listing started ctld' }

  $owner = [Diagnostics.Process]::new()
  $owner.StartInfo = New-FixtureStartInfo $fixtureCtld @('--detach-from-terminal')
  if (-not $owner.Start()) { throw 'Could not start isolated ctld' }
  $ownerStdout = $owner.StandardOutput.ReadToEndAsync()
  $ownerStderr = $owner.StandardError.ReadToEndAsync()
  $owner.StandardInput.Close()
  $deadline = [DateTime]::UtcNow.AddSeconds(30)
  do {
    if ($owner.HasExited) { throw "ctld exited during startup ($($owner.ExitCode))" }
    $row = Get-CtldRow
    if ($row.state -eq 'running') { break }
    Start-Sleep -Milliseconds 100
  } while ([DateTime]::UtcNow -lt $deadline)
  if ($row.state -ne 'running' -or -not $row.restart_supported -or -not $row.running_compatible) {
    throw 'Native ctld did not expose a compatible, restartable owner'
  }

  $inventory = Invoke-Ctl -Arguments @('vpn', 'list', '--json')
  Assert-EmptyVpnInventory $inventory
  if (-not $inventory.supports_multiple -or @($inventory.discovery_warnings).Count -eq 0) {
    throw 'VPN status did not reach the native owner and report its missing container engine'
  }

  Write-Host 'Checking owner-pinned Windows ctld restart through ctl'
  $plan = Invoke-Ctl -Arguments @('components', 'restart', 'ctld', '--dry-run', '--json')
  if (-not $plan.dry_run -or $plan.component -ne 'ctld' -or -not $plan.before.instance_id) {
    throw 'Restart dry run did not identify the running owner'
  }
  $unchanged = Invoke-Ctl -Arguments @('components', 'restart', 'ctld', '--dry-run', '--json')
  if ($unchanged.before.instance_id -ne $plan.before.instance_id -or $owner.HasExited) {
    throw 'Restart dry run changed the owner'
  }
  $restarted = Invoke-Ctl -Arguments @('components', 'restart', 'ctld', '--yes', '--json')
  $afterId = $restarted.result.after.instance_id
  if (-not $afterId -or $afterId -eq $plan.before.instance_id) {
    throw 'Restart did not publish a new ctld instance'
  }
  if (-not $owner.WaitForExit(10000) -or $owner.ExitCode -ne 0) {
    throw 'The replaced ctld owner did not exit cleanly'
  }
  $after = Invoke-Ctl -Arguments @('components', 'restart', 'ctld', '--dry-run', '--json')
  if ($after.before.instance_id -ne $afterId -or (Get-CtldRow).state -ne 'running') {
    throw 'The verified replacement did not remain available to the CLI'
  }
  $inventory = Invoke-Ctl -Arguments @('vpn', 'list', '--json')
  Assert-EmptyVpnInventory $inventory
  if (-not $inventory.supports_multiple) { throw 'VPN status did not reconnect after owner replacement' }
  Write-Host 'Checking graceful Windows ctld stop without a replacement'
  $successors = @(Get-Process ctld -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $fixtureCtld })
  if ($successors.Count -ne 1) { throw 'Expected exactly one fixture ctld successor' }
  $successor = $successors[0]
  # Open the process handle before stop, so its exit outcome remains observable.
  $null = $successor.Handle
  $stopPlan = Invoke-Ctl -Arguments @('components', 'stop', 'ctld', '--dry-run', '--json')
  if (-not $stopPlan.dry_run -or $stopPlan.component -ne 'ctld' -or $successor.HasExited) {
    throw 'Stop dry run changed the running owner'
  }
  $stopped = Invoke-Ctl -Arguments @('components', 'stop', 'ctld', '--yes', '--json')
  if ($stopped.result.before.instance_id -ne $afterId) {
    throw 'Stop did not identify the verified successor'
  }
  if (-not $successor.WaitForExit(10000) -or $successor.ExitCode -ne 0) {
    throw 'Stopped ctld did not exit cleanly'
  }
  if ((Get-CtldRow).state -ne 'not_running') { throw 'Stop left a running owner or started a replacement' }
  $alreadyStopped = Invoke-Ctl -Arguments @('components', 'stop', 'ctld', '--yes', '--json')
  if (-not $alreadyStopped.already_stopped -or (Get-CtldRow).state -ne 'not_running') {
    throw 'Stopping an absent owner was not idempotent'
  }
  if (Get-Process ctld -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $fixtureCtld }) {
    throw 'Cooperative stop left a fixture ctld process alive'
  }
  if ((Test-Path $catalog) -or (Test-Path $profiles)) {
    throw 'Read-only owner operations created a catalog or saved VPN profile'
  }
} finally {
  # The executable path is unique to this fixture, including the successor
  # launched by ctl. Never stop other ctld processes on the machine.
  foreach ($process in @(Get-Process ctld -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $fixtureCtld })) {
    try {
      $process.Kill($true)
      if (-not $process.WaitForExit(5000)) { throw 'Fixture ctld did not stop during cleanup' }
    } finally {
      $process.Dispose()
    }
  }
  if ($null -ne $owner) {
    if (-not $owner.HasExited) {
      $owner.Kill($true)
      $owner.WaitForExit(5000) | Out-Null
    }
    $owner.Dispose()
  }
  if ($null -ne $successor) { $successor.Dispose() }
  foreach ($output in @($ownerStdout, $ownerStderr)) {
    if ($null -ne $output -and -not $output.Wait(5000)) {
      Write-Warning 'Fixture ctld left an output stream open'
    }
  }
  Remove-Item -Recurse -Force $fixtureRoot -ErrorAction SilentlyContinue
}
