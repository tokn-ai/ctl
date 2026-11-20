$ErrorActionPreference = 'Stop'
$env:WGET_ASKPASS = $null
$env:SSH_ASKPASS = $null
__ARGUMENTS__
__ACCOUNT_GUARD__
function Phase($name) { [Console]::Out.Write("ctl-published-stage-v1 $name`n") }
function Unavailable { [Console]::Out.Write("ctl-published-unavailable-v1`n"); exit 12 }
Phase 'checking-updater'
$dir = Join-Path $env:USERPROFILE ('.ctlup-fetch-' + [guid]::NewGuid().ToString('N'))
$sid = [Security.Principal.WindowsIdentity]::GetCurrent().User
$acl = New-Object Security.AccessControl.DirectorySecurity
$acl.SetAccessRuleProtection($true, $false)
$acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($sid, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow'))
if ([IO.Directory]::Exists($dir)) { throw 'Updater staging path already exists.' }
$null = [IO.Directory]::CreateDirectory($dir, $acl)
$file = Join-Path $dir 'ctlup.exe'
function Fetch($program, $arguments) {
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
        if ($dest.Length + $count -gt $size) { throw 'Published ctlup exceeds its expected length.' }
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
try {
  $managed = Join-Path $env:USERPROFILE ".tokn\ctl\updaters\$sha\ctlup.exe"
  $ready = $false
  if ([IO.File]::Exists($managed)) {
    $source = [IO.File]::Open($managed, 'Open', 'Read', 'Read')
    try {
      if ($source.Length -eq $size) {
        $dest = [IO.File]::Open($file, 'CreateNew', 'Write', 'None')
        try { $source.CopyTo($dest) } finally { $dest.Dispose() }
        $ready = (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash -eq $sha
      }
    } finally { $source.Dispose() }
    if (!$ready) { throw 'Installed ctlup differs from its immutable digest.' }
  }
  if ($ready) {
    Phase 'using-installed-updater'
  } elseif ($localUpdater -eq '1') {
    Phase 'uploading-updater'
    Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public static class CtlIn { [DllImport("kernel32.dll")] public static extern IntPtr GetStdHandle(int n); }'
    $handle = [Microsoft.Win32.SafeHandles.SafeFileHandle]::new([CtlIn]::GetStdHandle(-10), $false)
    $stream = [IO.FileStream]::new($handle, [IO.FileAccess]::Read, 1)
    $dest = [IO.File]::Open($file, 'Create', 'Write', 'None')
    try {
      $remaining = [long]$size
      $buffer = [byte[]]::new(65536)
      while ($remaining) {
        $count = $stream.Read($buffer, 0, [Math]::Min($remaining, $buffer.Length))
        if (!$count) { throw 'Incomplete cached updater upload.' }
        $dest.Write($buffer, 0, $count)
        $remaining -= $count
      }
    } finally { $dest.Dispose(); $stream.Dispose() }
    if ((Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash -ne $sha) { throw 'Uploaded ctlup checksum mismatch.' }
  } else {
    Phase 'downloading-updater'
    $curl = Get-Command curl.exe -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    $code = -1
    if ($curl) {
      $code = Fetch $curl.Source ('-q --globoff --location --max-redirs 0 --proto =https --fail --silent --show-error --connect-timeout 10 --max-time 120 --max-filesize ' + $size + ' -- "' + $url + '"')
      if ($code -notin -1,0,1,2,5,6,7,18,22,28,52,56) { throw "Remote curl failed with exit $code." }
    }
    if ($code -ne 0) {
      $wget = Get-Command wget.exe -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
      if (!$wget) { Unavailable }
      $code = Fetch $wget.Source ('--no-config --no-netrc --no-hsts --max-redirect=0 --timeout=10 --tries=1 -q -O - -- "' + $url + '"')
      if ($code -in -1,2,4,8) { Unavailable }
      if ($code -ne 0) { throw "Remote wget failed with exit $code." }
    }
    if ((Get-Item -LiteralPath $file).Length -ne $size -or (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash -ne $sha) { throw 'Published ctlup size or checksum mismatch.' }
  }
  $ErrorActionPreference = 'Continue'
  $help = $null | & $file install-publication --help 2>&1
  $code = $LASTEXITCODE
  $ErrorActionPreference = 'Stop'
  if ($code -eq 2 -and ($help | Out-String).Contains("unrecognized subcommand 'install-publication'")) { Unavailable }
  if ($code -ne 0) { throw 'Remote ctlup capability check failed.' }
  $start = [Diagnostics.ProcessStartInfo]::new()
  $start.FileName = $file
  $start.Arguments = 'install-publication --origin-hex ' + $origin + ' --archive-url "' + $archive + '" --target ' + $target + ' --manifest-bytes ' + $manifestBytes
  if ($localRuntime -eq '1') {
    Phase 'uploading-runtime'
    $start.Arguments += ' --archive-stdin'
  } else { Phase 'installing-runtime' }
  $start.UseShellExecute = $false
  $start.RedirectStandardOutput = $true
  # Inherit the SSH pipe for the helper's exact bundle-set JSON on stdin.
  $process = [Diagnostics.Process]::Start($start)
  try {
    $out = [Console]::OpenStandardOutput()
    $process.StandardOutput.BaseStream.CopyTo($out)
    $out.Flush()
    $process.WaitForExit()
    exit $process.ExitCode
  } finally { $process.Dispose() }
} finally {
  if ([IO.File]::Exists($file)) { [IO.File]::Delete($file) }
  [IO.Directory]::Delete($dir)
}
