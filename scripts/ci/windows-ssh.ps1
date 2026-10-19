$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_OS -ne 'Windows') {
  throw 'This fixture configures an ephemeral GitHub Windows runner; do not run on a workstation.'
}

function Invoke-Native {
  param([string]$Program, [string[]]$Arguments)
  & $Program @Arguments
  if ($LASTEXITCODE -ne 0) { throw "$Program failed with exit code $LASTEXITCODE" }
}

function Invoke-Ctl {
  param(
    [string[]]$Arguments,
    [int]$ExpectedExit = 0,
    [string]$InputText = '',
    [int]$TimeoutSeconds = 45
  )
  $start = [Diagnostics.ProcessStartInfo]::new($ctl)
  $start.UseShellExecute = $false
  $start.RedirectStandardInput = $true
  $start.RedirectStandardOutput = $true
  $start.RedirectStandardError = $true
  $start.Environment['CTL_HOSTS_PATH'] = $catalog
  # A local readiness probe must never start an accidental second task daemon.
  $start.Environment['CTL_TASKD_BIN'] = Join-Path $root 'must-not-start-taskd.exe'
  $start.Environment.Remove('CTL_TASKD_RUNTIME_DIR') | Out-Null
  foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
  $process = [Diagnostics.Process]::new()
  $process.StartInfo = $start
  $started = $false
  try {
    $started = $process.Start()
    if (-not $started) { throw 'Could not start ctl' }
    $stdout = $process.StandardOutput.ReadToEndAsync()
    $stderr = $process.StandardError.ReadToEndAsync()
    $process.StandardInput.Write($InputText)
    $process.StandardInput.Close()
    if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
      $process.Kill($true)
      throw "ctl $($Arguments -join ' ') timed out"
    }
    if (-not $stdout.Wait(5000) -or -not $stderr.Wait(5000)) {
      throw "ctl $($Arguments -join ' ') left its output streams open"
    }
    $result = [pscustomobject]@{
      stdout = $stdout.Result
      stderr = $stderr.Result
      exit_code = $process.ExitCode
    }
    if ($ExpectedExit -ge 0 -and $result.exit_code -ne $ExpectedExit) {
      throw "ctl $($Arguments -join ' ') exited $($result.exit_code), expected ${ExpectedExit}:`n$($result.stdout)`n$($result.stderr)"
    }
    return $result
  } finally {
    if ($started -and -not $process.HasExited) {
      $process.Kill($true)
      $process.WaitForExit(5000) | Out-Null
    }
    $process.Dispose()
  }
}

function Invoke-CallerFixture {
  param(
    [string]$Program,
    [string[]]$Arguments,
    [string]$InputText,
    [string]$Label,
    [string]$ExpectedTest = ''
  )
  $start = [Diagnostics.ProcessStartInfo]::new($Program)
  $start.UseShellExecute = $false
  $start.WorkingDirectory = (Get-Location).ProviderPath
  $start.RedirectStandardInput = $true
  $start.RedirectStandardOutput = $true
  $start.RedirectStandardError = $true
  foreach ($argument in $Arguments) {
    $start.ArgumentList.Add($argument)
  }
  $process = [Diagnostics.Process]::new()
  $process.StartInfo = $start
  $started = $false
  try {
    $started = $process.Start()
    if (-not $started) { throw "Could not start $Label" }
    $stdout = $process.StandardOutput.ReadToEndAsync()
    $stderr = $process.StandardError.ReadToEndAsync()
    # Credentials stay on stdin, outside argv, environment and captured logs.
    $process.StandardInput.Write($InputText)
    $process.StandardInput.Close()
    if (-not $process.WaitForExit(180000)) { throw "$Label timed out" }
    if (-not $stdout.Wait(5000) -or -not $stderr.Wait(5000)) {
      throw "$Label left output streams open"
    }
    if ($process.ExitCode -ne 0) {
      throw "$Label exited $($process.ExitCode):`n$($stdout.Result)`n$($stderr.Result)"
    }
    if ($ExpectedTest) {
      $testOutput = [regex]::Replace($stdout.Result, '\x1b\[[0-9;]*m', '')
      $expectedResult = '(?m)^test ' + [regex]::Escape($ExpectedTest) + ' \.\.\. ok\r?$'
      if (-not [regex]::IsMatch($testOutput, $expectedResult)) {
        throw "$Label did not report the expected test success: $ExpectedTest"
      }
    }
  } finally {
    if ($started -and -not $process.HasExited) {
      $process.Kill($true)
      $process.WaitForExit(5000) | Out-Null
    }
    $process.Dispose()
  }
}

function Invoke-CallerPassword {
  param([string]$Executable, [string]$Credentials)
  Invoke-CallerFixture -Program 'bun' -Arguments @(
    './scripts/ci/caller-password.mts', $Executable, 'ctl-windows-ci'
  ) -InputText $Credentials -Label 'Bun caller-password fixture'
}

function Invoke-CallerTrust {
  param([string]$Metadata)
  $testName = 'native_openssh_requires_host_trust_with_redirected_command_stdin'
  Invoke-CallerFixture -Program 'cargo' -Arguments @(
    'test', '--locked', '-p', 'ctl-cli', '--test', 'caller_host_trust',
    $testName,
    '--', '--exact', '--ignored', '--nocapture'
  ) -InputText $Metadata -Label 'Native caller host-trust fixture' -ExpectedTest $testName
  Write-Host 'Native caller host-trust key/password checks passed.'
}

function Assert-Equal {
  param([string]$Actual, [string]$Expected, [string]$Label)
  if ($Actual -cne $Expected) { throw "${Label}: expected '$Expected', got '$Actual'" }
}

function Get-TaskRow {
  param($Result, [string]$Name)
  $rows = @($Result.stdout.Trim() -split '\r?\n' | Where-Object { ($_ -split "`t")[1] -eq $Name })
  if ($rows.Count -ne 1) { throw "Expected exactly one task '$Name': $($Result.stdout)" }
  $fields = $rows[0] -split "`t"
  if ($fields.Count -ne 4) { throw "Invalid task row: $($rows[0])" }
  return [pscustomobject]@{ task_id = $fields[0]; name = $fields[1]; state = $fields[2]; program = $fields[3] }
}

function Wait-TaskState {
  param([string]$Name, [string]$State)
  $deadline = [DateTime]::UtcNow.AddSeconds(15)
  do {
    $row = Get-TaskRow (Invoke-Ctl -Arguments ($temporary + @('task', 'show', $Name))) $Name
    if ($row.state -eq $State) { return $row }
    Start-Sleep -Milliseconds 100
  } while ([DateTime]::UtcNow -lt $deadline)
  throw "Task '$Name' stayed '$($row.state)', expected '$State'"
}

$openssh = Join-Path $env:WINDIR 'System32\OpenSSH'
if (-not (Test-Path "$openssh\sshd.exe")) {
  Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0 | Out-Null
}
$root = Join-Path $env:ProgramData ('ctl-ssh-test-' + [guid]::NewGuid())
New-Item -ItemType Directory -Force $root | Out-Null
$ctl = (Resolve-Path 'target/debug/ctl.exe').Path
$catalog = Join-Path $root 'hosts.json'
$temporary = @('--destination', 'ctl-windows-ci', '--remote-platform', 'windows')
$saved = @('--host', 'saved-windows-ci', '--remote-platform', 'windows')
$sshDirectory = Join-Path $env:USERPROFILE '.ssh'
New-Item -ItemType Directory -Force $sshDirectory | Out-Null
$config = Join-Path $sshDirectory 'config'
$originalConfig = $null
if (Test-Path $config) { $originalConfig = [IO.File]::ReadAllBytes($config) }
$originalConfigText = if (Test-Path $config) { [IO.File]::ReadAllText($config) } else { '' }
$service = Get-Service sshd
$wasRunning = $service.Status -eq 'Running'
$originalStartType = $service.StartType
$originalServicePath = (Get-CimInstance Win32_Service -Filter "Name = 'sshd'").PathName
$serverConfig = Join-Path $env:ProgramData 'ssh\sshd_config'
$originalServerConfig = $null
if (Test-Path $serverConfig) { $originalServerConfig = [IO.File]::ReadAllBytes($serverConfig) }
$shellKey = 'HKLM:\SOFTWARE\OpenSSH'
$originalShell = (Get-ItemProperty $shellKey -Name DefaultShell -ErrorAction SilentlyContinue).DefaultShell
$authorizedKeys = Join-Path $env:ProgramData 'ssh\administrators_authorized_keys'
$originalAuthorizedKeys = $null
if (Test-Path $authorizedKeys) { $originalAuthorizedKeys = [IO.File]::ReadAllBytes($authorizedKeys) }
$installed = @()
$aclPaths = @((Split-Path $serverConfig), "$env:ProgramData\ssh\logs", $serverConfig, $authorizedKeys)
$originalAcls = @{}
foreach ($path in $aclPaths) {
  if (Test-Path $path) { $originalAcls[$path] = Get-Acl $path }
}
$originalTestHost = $env:CTL_TEST_SSH_HOST
$originalForwardHost = $env:CTL_TEST_SSH_FORWARD_HOST
$originalUnwantedPort = $env:CTL_TEST_SSH_UNWANTED_PORT
$originalCtlBin = $env:CTL_TEST_CTL_BIN
$taskDaemon = $null
$callerUser = 'ctlcallerci'
$callerCreated = $false
try {
  if (Get-LocalUser -Name $callerUser -ErrorAction SilentlyContinue) { throw "Refusing to replace $callerUser" }
  $callerPassword = [Guid]::NewGuid().ToString('N') + '!aA9'
  New-LocalUser -Name $callerUser -Password (ConvertTo-SecureString $callerPassword -AsPlainText -Force) | Out-Null
  $callerCreated = $true
  $usersGroup = Get-LocalGroup -SID 'S-1-5-32-545'
  Add-LocalGroupMember -Group $usersGroup.Name -Member $callerUser
  Stop-Service sshd -ErrorAction SilentlyContinue
  # The fixed remote command resolves installed executables through the server's PATH.
  foreach ($binary in @('ctl-agent.exe', 'ctmuxd.exe', 'ctl-taskd.exe')) {
    $destination = Join-Path $env:WINDIR "System32\$binary"
    if (Test-Path $destination) { throw "Refusing to replace $destination" }
    Copy-Item "target\debug\$binary" $destination
    $installed += $destination
  }
  New-Item -Path $shellKey -Force | Out-Null
  New-ItemProperty $shellKey -Name DefaultShell -Value "$env:WINDIR\System32\cmd.exe" -PropertyType String -Force | Out-Null
  Invoke-Native -Program "$openssh\ssh-keygen.exe" -Arguments @('-q', '-t', 'ed25519', '-N', '', '-f', "$root\host")
  Invoke-Native -Program "$openssh\ssh-keygen.exe" -Arguments @('-q', '-t', 'ed25519', '-N', '', '-f', "$root\client")
  Invoke-Native -Program "$openssh\ssh-keygen.exe" -Arguments @('-q', '-t', 'ed25519', '-N', '', '-f', "$root\wrong-client")
  # SYSTEM runs sshd; only SYSTEM and Administrators may change its host key/config.
  Invoke-Native -Program 'icacls.exe' -Arguments @($root, '/inheritance:r', '/grant:r', '*S-1-5-18:(OI)(CI)F', '*S-1-5-32-544:(OI)(CI)F', '/T')
  Invoke-Native -Program 'icacls.exe' -Arguments @("$root\host", '/setowner', '*S-1-5-18')
  # ssh-keygen adds an explicit creator ACE; /grant:r does not remove it.
  $creatorSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
  Invoke-Native -Program 'icacls.exe' -Arguments @("$root\host", '/remove:g', "*$creatorSid")
  $rootSsh = $root.Replace('\', '/')
  $user = $env:USERNAME.ToLowerInvariant()
  New-Item -ItemType Directory -Force (Split-Path $serverConfig) | Out-Null
  Copy-Item "$root\client.pub" $authorizedKeys -Force
  @"
Port 22222
ListenAddress 127.0.0.1
HostKey $rootSsh/host
AuthorizedKeysFile __PROGRAMDATA__/ssh/administrators_authorized_keys
PubkeyAuthentication yes
PasswordAuthentication no
KbdInteractiveAuthentication no
AllowUsers $user $callerUser
AllowTcpForwarding local
AllowAgentForwarding no
PermitTTY no
SyslogFacility LOCAL0
LogLevel DEBUG1
Match User $callerUser
  PasswordAuthentication yes
"@ | Set-Content $serverConfig -Encoding ascii
  # sshd validates these ACLs under SYSTEM, before it can initialize logging.
  foreach ($path in $aclPaths) {
    if (-not (Test-Path $path)) { New-Item -ItemType Directory -Path $path | Out-Null }
    $acl = if ((Get-Item $path).PSIsContainer) {
      [Security.AccessControl.DirectorySecurity]::new()
    } else { [Security.AccessControl.FileSecurity]::new() }
    $acl.SetOwner([Security.Principal.SecurityIdentifier]::new('S-1-5-18'))
    $acl.SetAccessRuleProtection($true, $false)
    foreach ($sid in @('S-1-5-18', 'S-1-5-32-544')) {
      $rule = [Security.AccessControl.FileSystemAccessRule]::new(
        [Security.Principal.SecurityIdentifier]::new($sid), 'FullControl', 'Allow')
      $acl.AddAccessRule($rule)
    }
    Set-Acl $path $acl
  }
  Invoke-Native -Program "$openssh\sshd.exe" -Arguments @('-t', '-f', $serverConfig)
  $hostKey = (Get-Content "$root\host.pub").Split(' ')
  "[127.0.0.1]:22222 $($hostKey[0]) $($hostKey[1])" | Set-Content "$root\known_hosts" -Encoding ascii
  $unwantedListener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
  try {
    $unwantedListener.Start()
    $unwantedPort = $unwantedListener.LocalEndpoint.Port
  } finally {
    $unwantedListener.Stop()
  }
  $proxySsh = (Join-Path $openssh 'ssh.exe').Replace('\', '/')
  $trustConfig = Join-Path $root 'caller-trust-config'
  [IO.File]::WriteAllText($trustConfig, '')
  @"
Include "$rootSsh/caller-trust-config"
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

Host ctl-windows-forward-ci
  HostName must-be-overridden.invalid
  Port 22222
  User $user
  IdentityFile $rootSsh/client
  IdentitiesOnly yes
  BatchMode yes
  StrictHostKeyChecking yes
  UserKnownHostsFile $rootSsh/known_hosts
  ConnectTimeout 10
  ProxyCommand "$proxySsh" -W %h:%p ctl-windows-ci
  ControlMaster auto
  ControlPath $rootSsh/must-not-create-control
  LocalForward 127.0.0.1:$unwantedPort 127.0.0.1:9

$originalConfigText
"@ | Set-Content $config -Encoding ascii
  Set-Service sshd -StartupType Manual
  Invoke-Native -Program 'sc.exe' -Arguments @('config', 'sshd', 'binPath=', "`"$openssh\sshd.exe`" -E `"$root\service.log`"")
  Start-Service sshd
  Invoke-Native -Program "$openssh\ssh.exe" -Arguments @('-vvv', '-E', "$root\client.log", 'ctl-windows-ci', 'echo', 'SSH_AUTHENTICATED')
  $env:CTL_TEST_SSH_HOST = 'ctl-windows-ci'
  Invoke-Native -Program 'cargo' -Arguments @('test', '--locked', '-p', 'ctl-agent', '--test', 'windows_ssh', '--', '--ignored', '--nocapture')

  Write-Host 'Checking ctld-owned forwarding with native SSH configuration and proxy processes'
  $env:CTL_TEST_SSH_FORWARD_HOST = 'ctl-windows-forward-ci'
  $env:CTL_TEST_SSH_UNWANTED_PORT = [string]$unwantedPort
  $env:CTL_TEST_CTL_BIN = $ctl
  Invoke-Native -Program 'cargo' -Arguments @('test', '--locked', '-p', 'ctld', '--test', 'windows_ssh_forwarding', '--', '--ignored', '--nocapture')

  Write-Host 'Checking temporary destinations, saved hosts, and remote exec streams'
  $result = Invoke-Ctl -Arguments ($temporary + @('exec', '--', 'cmd.exe', '/D', '/C', 'echo TEMPORARY_DESTINATION'))
  Assert-Equal $result.stdout.Trim() 'TEMPORARY_DESTINATION' 'SSH-config destination output'
  if (Test-Path $catalog) { throw 'A temporary destination created a host catalog' }
  $result = Invoke-Ctl -Arguments @('--host', 'ctl-windows-ci', 'exec', '--', 'echo', 'MUST_NOT_RUN') -ExpectedExit 1
  if (-not $result.stderr.Contains('No saved ctl host matches')) { throw "Unsaved --host was not rejected: $($result.stderr)" }
  if (Test-Path $catalog) { throw 'An unsaved --host created a host catalog' }

  $created = (Invoke-Ctl -Arguments @('host', 'create', 'saved-windows-ci', 'ctl-windows-ci', '--ssh-config', '--hostname', '127.0.0.1', '--json')).stdout | ConvertFrom-Json
  Assert-Equal $created.connection_methods[0].ssh_config_alias 'ctl-windows-ci' 'Saved SSH-config alias'
  Assert-Equal $created.connection_methods[0].target.hostname '127.0.0.1' 'Saved hostname override'
  $result = Invoke-Ctl -Arguments ($saved + @('exec', '--', 'cmd.exe', '/D', '/C', 'echo SAVED_HOST'))
  Assert-Equal $result.stdout.Trim() 'SAVED_HOST' 'Saved host output'
  # An alias and a saved name may collide. The saved route deliberately points
  # at a closed loopback port; --destination must still use the SSH-config alias.
  Invoke-Ctl -Arguments @('host', 'create', 'ctl-windows-ci', '127.0.0.1', '--port', '1', '--json') | Out-Null
  $catalogHash = (Get-FileHash $catalog).Hash
  $result = Invoke-Ctl -Arguments @('--host', 'ctl-windows-ci', '--remote-platform', 'windows', 'exec', '--', 'echo', 'MUST_NOT_RUN') -ExpectedExit 255
  if (-not $result.stderr.Contains('port 1:')) { throw "Saved collision did not select its configured port: $($result.stderr)" }
  $result = Invoke-Ctl -Arguments ($temporary + @('exec', '--', 'cmd.exe', '/D', '/C', 'echo EXEC_STDOUT& echo EXEC_STDERR 1>&2& exit /b 7')) -ExpectedExit 7
  Assert-Equal $result.stdout.Trim() 'EXEC_STDOUT' 'Remote exec stdout'
  Assert-Equal $result.stderr.Trim() 'EXEC_STDERR' 'Remote exec stderr'
  $result = Invoke-Ctl -Arguments ($temporary + @('exec', '--', 'findstr.exe', '.')) -InputText "EXEC_STDIN`r`n"
  Assert-Equal $result.stdout.Trim() 'EXEC_STDIN' 'Remote exec stdin'

  # Bypass a corrupt catalog too: a temporary destination does not need to read it.
  $catalogBytes = [IO.File]::ReadAllBytes($catalog)
  try {
    [IO.File]::WriteAllText($catalog, 'invalid host catalog')
    $corruptHash = (Get-FileHash $catalog).Hash
    $result = Invoke-Ctl -Arguments ($temporary + @('exec', '--', 'cmd.exe', '/D', '/C', 'echo CATALOG_BYPASS'))
    Assert-Equal $result.stdout.Trim() 'CATALOG_BYPASS' 'Corrupt catalog bypass'
    Assert-Equal (Get-FileHash $catalog).Hash $corruptHash 'Corrupt catalog preservation'
  } finally {
    [IO.File]::WriteAllBytes($catalog, $catalogBytes)
  }

  Write-Host 'Checking ctmux CLI routing through both destination selectors'
  $session = 'ssh-cli-' + [guid]::NewGuid().ToString('N')
  Invoke-Ctl -Arguments ($temporary + @('ctmux', 'new', '--detached', '--name', $session, '--', 'cmd.exe', '/D', '/Q')) | Out-Null
  $result = Invoke-Ctl -Arguments ($saved + @('ctmux', 'list'))
  if (-not $result.stdout.Contains("$session`t")) { throw "Saved host did not list the temporary destination's session: $($result.stdout)" }
  $view = (Invoke-Ctl -Arguments ($saved + @('ctmux', 'view', $session))).stdout | ConvertFrom-Json
  Assert-Equal $view.session_name $session 'Remote ctmux view'
  # Kill the terminal directly so this fixture does not create a client archive
  # in the real profile's .tokn directory (Windows Known Folders ignore HOME).
  Invoke-Ctl -Arguments ($saved + @('ctmux', 'kill-terminal', $view.terminals[0].terminal_id)) | Out-Null
  $deadline = [DateTime]::UtcNow.AddSeconds(15)
  do {
    $result = Invoke-Ctl -Arguments ($temporary + @('ctmux', 'list'))
    if (-not $result.stdout.Contains("$session`t")) { break }
    Start-Sleep -Milliseconds 100
  } while ([DateTime]::UtcNow -lt $deadline)
  if ($result.stdout.Contains("$session`t")) { throw 'Killed ctmux session is still listed' }

  Write-Host 'Checking remote task lifecycle and log streams'
  # SSH login does not inherit this script's environment. Start a fixture-owned
  # daemon on the user's default endpoint with isolated state. This verifies the
  # remote task gateway reuses it; windows-tasks.ps1 covers daemon auto-start.
  if (Get-Process ctl-taskd -ErrorAction SilentlyContinue) { throw 'Refusing to reuse an existing ctl-taskd' }
  $taskStart = [Diagnostics.ProcessStartInfo]::new((Join-Path $env:WINDIR 'System32\ctl-taskd.exe'))
  $taskStart.UseShellExecute = $false
  $taskStart.RedirectStandardOutput = $true
  $taskStart.RedirectStandardError = $true
  foreach ($name in @('CTL_TASKD_RUNTIME_DIR', 'CTL_TASKD_DATA_DIR', 'CTMUX_RUNTIME_DIR', 'XDG_RUNTIME_DIR')) {
    $taskStart.Environment.Remove($name) | Out-Null
  }
  $taskStart.ArgumentList.Add('--data-directory')
  $taskStart.ArgumentList.Add((Join-Path $root 'task-data'))
  $taskDaemon = [Diagnostics.Process]::Start($taskStart)
  $taskStdout = $taskDaemon.StandardOutput.ReadToEndAsync()
  $taskStderr = $taskDaemon.StandardError.ReadToEndAsync()
  $deadline = [DateTime]::UtcNow.AddSeconds(15)
  do {
    if ($taskDaemon.HasExited) { throw "Fixture task daemon exited: $($taskStderr.Result)" }
    $ready = Invoke-Ctl -Arguments @('task', 'list') -ExpectedExit -1 -TimeoutSeconds 5
    if ($ready.exit_code -eq 0) { break }
    Start-Sleep -Milliseconds 100
  } while ([DateTime]::UtcNow -lt $deadline)
  if ($ready.exit_code -ne 0) { throw "Fixture task daemon did not become ready: $($ready.stderr)" }
  Assert-Equal $ready.stdout '' 'Empty fixture task daemon'
  $result = Invoke-Ctl -Arguments ($temporary + @('task', 'list'))
  Assert-Equal $result.stdout '' 'Empty remote task daemon'
  $taskWorkspace = Join-Path $root 'task workspace'
  New-Item -ItemType Directory $taskWorkspace | Out-Null
  $task = 'ssh-task-' + [guid]::NewGuid().ToString('N')
  $row = Get-TaskRow (Invoke-Ctl -Arguments ($temporary + @('task', 'create', $task, '--cwd', $taskWorkspace, '--', 'cmd.exe', '/D', '/C', 'echo TASK_STDOUT& echo TASK_STDERR 1>&2& cd& exit /b 7'))) $task
  Assert-Equal $row.state 'stopped' 'Created task state'
  $taskId = $row.task_id
  $row = Get-TaskRow (Invoke-Ctl -Arguments ($saved + @('task', 'show', $task))) $task
  Assert-Equal $row.task_id $taskId 'Task identity across selectors'
  Invoke-Ctl -Arguments ($saved + @('task', 'start', $task)) | Out-Null
  $result = Invoke-Ctl -Arguments ($temporary + @('task', 'logs', $task, '--follow'))
  Assert-Equal $result.stdout.Trim() "TASK_STDOUT`r`n$taskWorkspace" 'Remote task stdout and working directory'
  Assert-Equal $result.stderr.Trim() 'TASK_STDERR' 'Remote task stderr'
  Wait-TaskState $task 'failed' | Out-Null
  Invoke-Ctl -Arguments ($saved + @('task', 'restart', $task)) | Out-Null
  $result = Invoke-Ctl -Arguments ($temporary + @('task', 'logs', $task, '--follow'))
  Assert-Equal $result.stdout.Trim() "TASK_STDOUT`r`n$taskWorkspace" 'Restarted remote task stdout'
  Wait-TaskState $task 'failed' | Out-Null
  $result = Invoke-Ctl -Arguments ($saved + @('task', 'remove', $task))
  Assert-Equal $result.stdout.Trim() "removed $taskId" 'Removed remote task'

  $longTask = 'ssh-long-' + [guid]::NewGuid().ToString('N')
  Invoke-Ctl -Arguments ($temporary + @('task', 'create', $longTask, '--start', '--cwd', $taskWorkspace, '--', 'cmd.exe', '/D', '/C', 'ping -n 120 127.0.0.1 >nul')) | Out-Null
  $row = Get-TaskRow (Invoke-Ctl -Arguments ($saved + @('task', 'show', $longTask))) $longTask
  Assert-Equal $row.state 'running' 'Task survives creating SSH channel disconnect'
  $row = Get-TaskRow (Invoke-Ctl -Arguments ($saved + @('task', 'stop', $longTask))) $longTask
  Assert-Equal $row.state 'stopped' 'Stopped remote task'
  Invoke-Ctl -Arguments ($temporary + @('task', 'remove', $longTask)) | Out-Null
  $result = Invoke-Ctl -Arguments ($saved + @('task', 'list'))
  Assert-Equal $result.stdout '' 'Remote task cleanup'
  Assert-Equal (Get-FileHash $catalog).Hash $catalogHash 'Connections preserve the saved host catalog'
  Write-Host 'Checking caller-owned SSH keys and one-shot password channels'
  $result = Invoke-Ctl -Arguments @('--destination', 'ctl-windows-ci', '--identity-file', "$root\client", '--port', '22222', '--remote-platform', 'windows', 'exec', '--', 'cmd.exe', '/D', '/C', 'echo CALLER_KEY_AUTHENTICATED')
  Assert-Equal $result.stdout.Trim() 'CALLER_KEY_AUTHENTICATED' 'Caller-owned SSH key authentication'
  # The SSH config's working IdentityFile must not rescue this incorrect key.
  Invoke-Ctl -Arguments @('--destination', 'ctl-windows-ci', '--identity-file', "$root\wrong-client", '--remote-platform', 'windows', 'exec', '--', 'cmd.exe', '/D', '/C', 'exit /b 0') -ExpectedExit 255 | Out-Null
  $callerDirectory = Join-Path $root 'caller cli'
  New-Item -ItemType Directory $callerDirectory | Out-Null
  Copy-Item '.\target\debug\ctl.exe' "$callerDirectory\ctl.exe"
  Invoke-CallerPassword -Executable "$callerDirectory\ctl.exe" -Credentials (@{ user = $callerUser; password = $callerPassword } | ConvertTo-Json -Compress)
  $differentKey = (Get-Content "$root\wrong-client.pub").Split(' ')
  $fingerprintOutput = & "$openssh\ssh-keygen.exe" -E sha256 -lf "$root\host.pub"
  if ($LASTEXITCODE -ne 0) { throw 'Could not inspect the fixture host fingerprint' }
  Invoke-CallerTrust -Metadata (@{
    key_user = $user
    key_file = "$root\client"
    server_key = "$($hostKey[0]) $($hostKey[1])"
    different_key = "$($differentKey[0]) $($differentKey[1])"
    fingerprint = ($fingerprintOutput -split '\s+')[1]
    port = 22222
    include_file = $trustConfig
    password_user = $callerUser
    password = $callerPassword
  } | ConvertTo-Json -Compress)
  Invoke-Ctl -Arguments ($temporary + @('ctmux', 'list')) | Out-Null
} catch {
  Stop-Service sshd -ErrorAction SilentlyContinue
  Get-Content "$root\client.log" -Tail 100 -ErrorAction SilentlyContinue
  Get-Content "$root\service.log" -Tail 100 -ErrorAction SilentlyContinue
  if ((Get-Service sshd).Status -eq 'Stopped') {
    $diagnostic = Start-Process "$openssh\sshd.exe" -ArgumentList @('-D', '-e', '-f', $serverConfig) -RedirectStandardError "$root\startup.log" -RedirectStandardOutput "$root\startup.out" -PassThru
    if (-not $diagnostic.WaitForExit(2000)) { Stop-Process -Id $diagnostic.Id -Force }
    Get-Content "$root\startup.log" -Tail 100 -ErrorAction SilentlyContinue
  }
  Get-Content "$env:ProgramData\ssh\logs\sshd.log" -Tail 100 -ErrorAction SilentlyContinue
  Get-CimInstance Win32_Service -Filter "Name = 'sshd'" | Format-List PathName, StartName, ExitCode
  Get-Service sshd | Format-List Name, Status, StartType
  Get-WinEvent -FilterHashtable @{ LogName = 'System'; ProviderName = 'Service Control Manager'; StartTime = (Get-Date).AddMinutes(-5) } -MaxEvents 10 -ErrorAction SilentlyContinue | Format-List TimeCreated, Message
  Get-WinEvent -LogName 'OpenSSH/Operational' -MaxEvents 20 -ErrorAction SilentlyContinue | Format-List TimeCreated, Message
  Get-WinEvent -LogName 'OpenSSH/Admin' -MaxEvents 20 -ErrorAction SilentlyContinue | Format-List TimeCreated, Message
  throw
} finally {
  try {
    Stop-Service sshd -ErrorAction SilentlyContinue
    if ($null -ne $taskDaemon) {
      if (-not $taskDaemon.HasExited) { $taskDaemon.Kill($true); $taskDaemon.WaitForExit(5000) | Out-Null }
      if ($taskStderr.Wait(5000) -and $taskStderr.Result) { Write-Host $taskStderr.Result }
      $taskDaemon.Dispose()
    }
    Get-CimInstance Win32_Process -Filter "Name = 'ctmuxd.exe' OR Name = 'ctl-taskd.exe' OR Name = 'ctl-agent.exe'" | Where-Object { $_.ExecutablePath -in $installed } | ForEach-Object {
      Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue
    }
    foreach ($binary in $installed) { Remove-Item $binary -Force -ErrorAction SilentlyContinue }
    if ($null -eq $originalConfig) { Remove-Item $config -ErrorAction SilentlyContinue }
    else { [IO.File]::WriteAllBytes($config, $originalConfig) }
    if ($null -eq $originalServerConfig) { Remove-Item $serverConfig -ErrorAction SilentlyContinue }
    else { [IO.File]::WriteAllBytes($serverConfig, $originalServerConfig) }
    if ($null -eq $originalAuthorizedKeys) { Remove-Item $authorizedKeys -ErrorAction SilentlyContinue }
    else { [IO.File]::WriteAllBytes($authorizedKeys, $originalAuthorizedKeys) }
    if ($null -eq $originalShell) { Remove-ItemProperty $shellKey -Name DefaultShell -ErrorAction SilentlyContinue }
    else { Set-ItemProperty $shellKey -Name DefaultShell -Value $originalShell }
    # Restore the parent first: inherited ACL propagation must not overwrite the
    # original child ACLs after they have been restored.
    foreach ($path in $aclPaths) {
      if ($originalAcls.ContainsKey($path) -and (Test-Path $path)) { Set-Acl $path $originalAcls[$path] }
    }
    Invoke-Native -Program 'sc.exe' -Arguments @('config', 'sshd', 'binPath=', $originalServicePath)
    if ($wasRunning) { Start-Service sshd }
    Set-Service sshd -StartupType $originalStartType
  } finally {
    # Service/config/ACL restoration must not skip the invocation's own cleanup.
    $env:CTL_TEST_SSH_HOST = $originalTestHost
    $env:CTL_TEST_SSH_FORWARD_HOST = $originalForwardHost
    $env:CTL_TEST_SSH_UNWANTED_PORT = $originalUnwantedPort
    $env:CTL_TEST_CTL_BIN = $originalCtlBin
    try {
      if ($callerCreated) { Remove-LocalUser -Name $callerUser }
    } finally {
      Remove-Item $root -Recurse -Force -ErrorAction SilentlyContinue
    }
  }
}
