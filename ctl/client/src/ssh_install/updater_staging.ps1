function Get-UpdaterStaging {
  $path = $env:USERPROFILE
  foreach ($name in @('.tokn', 'ctl', 'ctlup', 'staging')) {
    $path = Join-Path $path $name
    if (![IO.Directory]::Exists($path)) {
      $null = [IO.Directory]::CreateDirectory($path)
    }
    if ([IO.File]::GetAttributes($path) -band [IO.FileAttributes]::ReparsePoint) {
      throw 'Installation staging path contains a reparse point.'
    }
  }
  return $path
}

function New-UpdaterStaging {
  $dir = Join-Path (Get-UpdaterStaging) ('.ctlup-fetch-' + [guid]::NewGuid().ToString('N'))
  $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User
  $acl = New-Object Security.AccessControl.DirectorySecurity
  $acl.SetAccessRuleProtection($true, $false)
  $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($sid, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow'))
  if ([IO.Directory]::Exists($dir)) { throw 'Updater staging path already exists.' }
  $null = [IO.Directory]::CreateDirectory($dir, $acl)
  return $dir
}

function Copy-UpdaterOutput($program, $arguments, $file, [long]$size) {
  $start = [Diagnostics.ProcessStartInfo]::new()
  $start.FileName = $program
  $start.Arguments = $arguments
  $start.UseShellExecute = $false
  $start.RedirectStandardInput = $true
  $start.RedirectStandardOutput = $true
  $process = [Diagnostics.Process]::Start($start)
  try {
    $process.StandardInput.Close()
    $stream = $process.StandardOutput.BaseStream
    $dest = [IO.File]::Open($file, 'Create', 'Write', 'None')
    try {
      $clock = [Diagnostics.Stopwatch]::StartNew()
      $buffer = [byte[]]::new(65536)
      while ($true) {
        $read = $stream.ReadAsync($buffer, 0, $buffer.Length)
        if (!$read.Wait([int][Math]::Max(0, 120000 - $clock.ElapsedMilliseconds))) { return -1 }
        $count = $read.Result
        if (!$count) { break }
        if ($dest.Length + $count -gt $size) { throw 'ctlup exceeds its expected length.' }
        $dest.Write($buffer, 0, $count)
      }
      if (!$process.WaitForExit([int][Math]::Max(0, 120000 - $clock.ElapsedMilliseconds))) { return -1 }
      return $process.ExitCode
    } finally { $dest.Dispose() }
  } finally {
    if (!$process.HasExited) { $process.Kill(); $process.WaitForExit() }
    $process.Dispose()
  }
}

function Assert-PlainUpdaterPath($path) {
  if ([IO.File]::GetAttributes($path) -band [IO.FileAttributes]::ReparsePoint) {
    throw 'Installed updater path contains a reparse point.'
  }
}

function Find-InstalledUpdater {
  $path = $env:USERPROFILE
  foreach ($segment in @('.tokn', 'ctl', 'bin')) {
    $path = Join-Path $path $segment
    if (![IO.Directory]::Exists($path)) { return }
    Assert-PlainUpdaterPath $path
  }
  $path = Join-Path $path 'ctlup.exe'
  if (![IO.File]::Exists($path)) { return }
  Assert-PlainUpdaterPath $path
  return $path
}

function Test-TrackedUpdater($manager) {
  $preference = $ErrorActionPreference
  $ErrorActionPreference = 'Continue'
  try {
    $null | & $manager updater-channel-tracked *> $null
    return ($LASTEXITCODE -eq 0)
  } finally { $ErrorActionPreference = $preference }
}

function Test-StagedUpdater($file, [long]$size, $sha) {
  return ((Get-Item -LiteralPath $file).Length -eq $size -and (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash -eq $sha)
}

function Copy-RetainedUpdater($file, [long]$size, $sha) {
  $manager = Find-InstalledUpdater
  if ($manager) {
    $ErrorActionPreference = 'Continue'
    $help = $null | & $manager export-manager --help 2>&1
    $code = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    if ($code -eq 0) {
      $code = Copy-UpdaterOutput $manager ('export-manager --sha256 ' + $sha) $file $size
      if ($code -eq 10) { return $false }
      if ($code -ne 0) { throw 'Could not export the retained ctlup.' }
      if (!(Test-StagedUpdater $file $size $sha)) { throw 'Installed ctlup size or checksum mismatch.' }
      return $true
    }
    # Older launchers dispatch before parsing and can fail on a missing selection.
    $details = $help -join "`n"
    $unsupported = $code -eq 2 -and $details.Contains("unrecognized subcommand 'export-manager'")
    $missing = $code -eq 1 -and $details -match '(?m)^ctlup: (no managed updater is installed|selected updater payload is missing|[^\r\n]* \(os error [23]\))\r?$'
    if (!$unsupported -and !$missing) {
      throw 'Remote ctlup export capability check failed.'
    }
  }
  # Older managers retained the digest in the object path.
  $managed = Join-Path $env:USERPROFILE '.tokn\ctl'
  foreach ($segment in @('ctlup', 'store', 'updater', $sha)) {
    $managed = Join-Path $managed $segment
    if (![IO.Directory]::Exists($managed)) { return $false }
    Assert-PlainUpdaterPath $managed
  }
  $managed = Join-Path $managed 'ctlup.exe'
  if (![IO.File]::Exists($managed)) { return $false }
  Assert-PlainUpdaterPath $managed
  $source = [IO.File]::Open($managed, 'Open', 'Read', 'Read')
  try {
    if ($source.Length -ne $size) { throw 'Installed ctlup size or checksum mismatch.' }
    $dest = [IO.File]::Open($file, 'CreateNew', 'Write', 'None')
    try { $source.CopyTo($dest) } finally { $dest.Dispose() }
  } finally { $source.Dispose() }
  if (!(Test-StagedUpdater $file $size $sha)) { throw 'Installed ctlup size or checksum mismatch.' }
  return $true
}
