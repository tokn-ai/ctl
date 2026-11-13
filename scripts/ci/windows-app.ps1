[CmdletBinding()]
param(
  [Parameter(Mandatory)]
  [ValidateSet('x86_64-pc-windows-msvc', 'aarch64-pc-windows-msvc')]
  [string]$Target,
  [string]$Archive
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if (-not $IsWindows) { throw 'The desktop smoke test requires native Windows.' }
$rustVersion = & rustc --version --verbose
if ($LASTEXITCODE -ne 0) { throw 'Could not inspect the native Rust toolchain.' }
$nativeTarget = ($rustVersion | Where-Object { $_ -like 'host: *' }) -replace '^host: ', ''
if ($nativeTarget -ne $Target) {
  throw "This smoke test must execute natively: requested $Target, toolchain host $nativeTarget."
}

# Require one passing native fixture so a misspelled pattern cannot silently skip it.
function Test-NativeNodeFixture([string]$TestFile, [string]$TestName) {
  $output = & node --experimental-strip-types --disable-warning=ExperimentalWarning --test --test-reporter tap --test-name-pattern $TestName $TestFile 2>&1
  $code = $LASTEXITCODE
  $output | ForEach-Object { Write-Host $_ }
  if ($code -ne 0 -or @($output | Where-Object { $_ -match '^# pass 1$' }).Count -ne 1) {
    throw "The native fixture did not pass exactly once: $TestName"
  }
}

Test-NativeNodeFixture 'scripts/dev/build-desktop-app.test.mts' '^Windows desktop commands '
Test-NativeNodeFixture 'scripts/dev/windows-portable.test.mts' '^Windows portable archives use forward slash paths and preserve hidden files and empty directories$'

# GUI launch requires WebView2. The install fixture does not launch the GUI,
# install prerequisites, or change the runner's active app and daemon owners.
$runtimeId = '{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
$runtimeVersion = $null
foreach ($key in @(
  "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\$runtimeId",
  "HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\$runtimeId",
  "HKCU:\Software\Microsoft\EdgeUpdate\Clients\$runtimeId"
)) {
  $value = Get-ItemProperty -LiteralPath $key -Name pv -ErrorAction SilentlyContinue
  if ($null -ne $value -and $value.pv -match '^\d+\.\d+\.\d+\.\d+$' -and [version]$value.pv -gt [version]'0.0.0.0') {
    $runtimeVersion = $value.pv
    break
  }
}
if ($runtimeVersion) {
  Write-Host "WebView2 Runtime: $runtimeVersion"
} else {
  Write-Warning 'WebView2 Runtime is not installed. GUI launch requires the Microsoft Evergreen Runtime: https://developer.microsoft.com/microsoft-edge/webview2/'
}

# Run ordinary native activation/locking/recovery regressions as well as the
# explicitly ignored build or archive fixture below.
& cargo test --locked --target $Target -p ctl-client --lib development::windows_tests
if ($LASTEXITCODE -ne 0) { throw "The native desktop build cancellation tests failed with exit code $LASTEXITCODE." }
& cargo test --locked --target $Target -p ctl-client --lib desktop_storage::tests
if ($LASTEXITCODE -ne 0) { throw "The native desktop storage tests failed with exit code $LASTEXITCODE." }
& cargo test --locked --target $Target -p ctl-cli --lib updater::app
if ($LASTEXITCODE -ne 0) { throw "The native desktop updater tests failed with exit code $LASTEXITCODE." }

$previousArchive = [Environment]::GetEnvironmentVariable('CTLUP_TEST_DESKTOP_ARCHIVE', 'Process')
try {
  if ($Archive) {
    $archivePath = (Resolve-Path -LiteralPath $Archive).Path
    if (-not $archivePath.EndsWith('.zip', [StringComparison]::OrdinalIgnoreCase)) {
      throw 'The prebuilt desktop fixture requires a portable ZIP archive.'
    }
    $checksumPath = "$archivePath.sha256"
    $expected = (Get-FileHash -Algorithm SHA256 -LiteralPath $archivePath).Hash.ToLowerInvariant() + '  ' + [IO.Path]::GetFileName($archivePath)
    if ((Get-Content -LiteralPath $checksumPath -Raw).TrimEnd([char[]]"`r`n") -cne $expected) {
      throw 'The portable desktop archive checksum sidecar does not match its bytes and filename.'
    }
    $env:CTLUP_TEST_DESKTOP_ARCHIVE = $archivePath
    $fixture = 'updater::app::tests::prebuilt_windows_archive_install_update_and_repair'
  } else {
    Remove-Item Env:CTLUP_TEST_DESKTOP_ARCHIVE -ErrorAction SilentlyContinue
    $fixture = 'updater::app::tests::local_source_install_and_repair_preserve_the_saved_channel'
  }
  $available = & cargo test --locked --target $Target -p ctl-cli --lib $fixture -- --list
  if ($LASTEXITCODE -ne 0 -or @($available | Where-Object { $_ -ceq "${fixture}: test" }).Count -ne 1) {
    throw "The native desktop fixture is missing: $fixture."
  }
  & cargo test --locked --target $Target -p ctl-cli --lib $fixture -- --ignored --exact --nocapture
  if ($LASTEXITCODE -ne 0) { throw "The native desktop install/update/repair fixture failed with exit code $LASTEXITCODE." }
} finally {
  if ($null -eq $previousArchive) {
    Remove-Item Env:CTLUP_TEST_DESKTOP_ARCHIVE -ErrorAction SilentlyContinue
  } else {
    $env:CTLUP_TEST_DESKTOP_ARCHIVE = $previousArchive
  }
}
