$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_OS -ne 'Windows') {
  throw 'This fixture configures an ephemeral GitHub Windows runner; do not run on a workstation.'
}

function Invoke-Native {
  param([string]$Program, [string[]]$Arguments)
  & $Program @Arguments
  if ($LASTEXITCODE -ne 0) { throw "$Program failed with exit code $LASTEXITCODE" }
}

$openssh = Join-Path $env:WINDIR 'System32\OpenSSH'
if (-not (Test-Path "$openssh\sshd.exe")) {
  Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0 | Out-Null
}
$root = Join-Path $env:ProgramData 'ctl-ssh-test'
New-Item -ItemType Directory -Force $root | Out-Null
$sshDirectory = Join-Path $env:USERPROFILE '.ssh'
New-Item -ItemType Directory -Force $sshDirectory | Out-Null
$config = Join-Path $sshDirectory 'config'
$originalConfig = if (Test-Path $config) { [IO.File]::ReadAllText($config) } else { $null }
$service = Get-Service sshd
$wasRunning = $service.Status -eq 'Running'
$serverConfig = Join-Path $env:ProgramData 'ssh\sshd_config'
$originalServerConfig = if (Test-Path $serverConfig) { [IO.File]::ReadAllText($serverConfig) } else { $null }
$shellKey = 'HKLM:\SOFTWARE\OpenSSH'
$originalShell = (Get-ItemProperty $shellKey -Name DefaultShell -ErrorAction SilentlyContinue).DefaultShell
$installed = @()
try {
  Stop-Service sshd -ErrorAction SilentlyContinue
  # The fixed remote command resolves installed executables through the server's PATH.
  foreach ($binary in @('ctld.exe', 'rmuxd.exe')) {
    $destination = Join-Path $env:WINDIR "System32\$binary"
    if (Test-Path $destination) { throw "Refusing to replace $destination" }
    Copy-Item "target\debug\$binary" $destination
    $installed += $destination
  }
  New-Item -Path $shellKey -Force | Out-Null
  New-ItemProperty $shellKey -Name DefaultShell -Value "$env:WINDIR\System32\cmd.exe" -PropertyType String -Force | Out-Null
  Invoke-Native -Program "$openssh\ssh-keygen.exe" -Arguments @('-q', '-t', 'ed25519', '-N', '', '-f', "$root\host")
  Invoke-Native -Program "$openssh\ssh-keygen.exe" -Arguments @('-q', '-t', 'ed25519', '-N', '', '-f', "$root\client")
  Copy-Item "$root\client.pub" "$root\authorized_keys"
  # SYSTEM runs sshd; only SYSTEM and Administrators may change its host key/config.
  Invoke-Native -Program 'icacls.exe' -Arguments @($root, '/inheritance:r', '/grant:r', '*S-1-5-18:(OI)(CI)F', '*S-1-5-32-544:(OI)(CI)F', '/T')
  $rootSsh = $root.Replace('\', '/')
  $user = $env:USERNAME.ToLowerInvariant()
  New-Item -ItemType Directory -Force (Split-Path $serverConfig) | Out-Null
  @"
Port 22222
ListenAddress 127.0.0.1
HostKey $rootSsh/host
AuthorizedKeysFile $rootSsh/authorized_keys
PubkeyAuthentication yes
PasswordAuthentication no
KbdInteractiveAuthentication no
AllowUsers $user
AllowTcpForwarding no
AllowAgentForwarding no
PermitTTY no
LogLevel DEBUG1
"@ | Set-Content $serverConfig -Encoding ascii
  Invoke-Native -Program "$openssh\sshd.exe" -Arguments @('-t', '-f', $serverConfig)
  $hostKey = (Get-Content "$root\host.pub").Split(' ')
  "[127.0.0.1]:22222 $($hostKey[0]) $($hostKey[1])" | Set-Content "$root\known_hosts" -Encoding ascii
  @"
Host ctl-windows-ci
  HostName 127.0.0.1
  Port 22222
  User $user
  IdentityFile $rootSsh/client
  IdentitiesOnly yes
  BatchMode yes
  StrictHostKeyChecking yes
  UserKnownHostsFile $rootSsh/known_hosts
  ConnectTimeout 10

$originalConfig
"@ | Set-Content $config -Encoding ascii
  Start-Service sshd
  $env:CTL_TEST_SSH_HOST = 'ctl-windows-ci'
  Invoke-Native -Program 'cargo' -Arguments @('test', '--locked', '-p', 'ctld', '--test', 'windows_ssh', '--', '--ignored', '--nocapture')
  Invoke-Native -Program '.\target\debug\ctl.exe' -Arguments @('--host', 'ctl-windows-ci', '--remote-platform', 'windows', 'rmux', 'list')
} catch {
  Get-WinEvent -LogName 'OpenSSH/Operational' -MaxEvents 20 -ErrorAction SilentlyContinue | Format-List TimeCreated, Message
  throw
} finally {
  Stop-Service sshd -ErrorAction SilentlyContinue
  Get-CimInstance Win32_Process -Filter "Name = 'rmuxd.exe'" | Where-Object { $_.ExecutablePath -in $installed } | ForEach-Object {
    Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue
  }
  foreach ($binary in $installed) { Remove-Item $binary -Force -ErrorAction SilentlyContinue }
  if ($null -eq $originalConfig) { Remove-Item $config -ErrorAction SilentlyContinue }
  else { [IO.File]::WriteAllText($config, $originalConfig) }
  if ($null -eq $originalServerConfig) { Remove-Item $serverConfig -ErrorAction SilentlyContinue }
  else { [IO.File]::WriteAllText($serverConfig, $originalServerConfig) }
  if ($null -eq $originalShell) { Remove-ItemProperty $shellKey -Name DefaultShell -ErrorAction SilentlyContinue }
  else { Set-ItemProperty $shellKey -Name DefaultShell -Value $originalShell }
  if ($wasRunning) { Start-Service sshd }
  Remove-Item Env:CTL_TEST_SSH_HOST -ErrorAction SilentlyContinue
  Remove-Item $root -Recurse -Force -ErrorAction SilentlyContinue
}
