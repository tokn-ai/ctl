//! Authenticated native Windows fixture, invoked by scripts/ci/windows-ssh.ps1.

use ctl_ipc::lifecycle::{Client as LifecycleClient, DaemonStatus};
use ctl_ipc::{
  ClientMessage, LocalPortForward, PortForwardState, PortForwardStatus, ServerMessage, SshTarget,
};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{Duration, sleep, timeout};

const DEADLINE: Duration = Duration::from_secs(30);
const PAYLOAD: &[u8] = b"forward\0\xff\xfe\r\nbytes";
const SNAPSHOT_DESCENDANTS: &str = r"
$ErrorActionPreference = 'Stop'
$processes = @(Get-CimInstance Win32_Process)
$parents = [Collections.Generic.HashSet[uint32]]::new()
$parents.Add([uint32]$env:CTL_FORWARD_OWNER_PID) | Out-Null
$descendants = @()
do {
  $changed = $false
  foreach ($process in $processes) {
    if ($parents.Contains($process.ParentProcessId) -and $parents.Add($process.ProcessId)) {
      if ($null -eq $process.CreationDate) { throw 'Owned process creation time is unavailable' }
      $changed = $true
      $descendants += [pscustomobject]@{
        process_id = $process.ProcessId
        parent_id = $process.ParentProcessId
        name = $process.Name
        created_at = [string]$process.CreationDate.ToUniversalTime().Ticks
      }
    }
  }
} while ($changed)
ConvertTo-Json -InputObject @($descendants) -Compress
";
const WAIT_DESCENDANTS_GONE: &str = r"
$ErrorActionPreference = 'Stop'
$expected = @([Console]::In.ReadToEnd() | ConvertFrom-Json)
$deadline = [DateTime]::UtcNow.AddSeconds(15)
do {
  $actual = @{}
  foreach ($process in @(Get-CimInstance Win32_Process)) {
    $actual[[string]$process.ProcessId] = $process
  }
  $remaining = @($expected | Where-Object {
    $process = $actual[[string]$_.process_id]
    $null -ne $process -and $null -ne $process.CreationDate -and
      [string]$process.CreationDate.ToUniversalTime().Ticks -eq $_.created_at
  })
  if ($remaining.Count -eq 0) { exit 0 }
  Start-Sleep -Milliseconds 100
} while ([DateTime]::UtcNow -lt $deadline)
ConvertTo-Json -InputObject @($remaining) -Compress
exit 1
";

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct ProcessIdentity {
  process_id: u32,
  parent_id: u32,
  name: String,
  created_at: String,
}

async fn powershell(script: &str, owner: u32, input: &[u8]) -> std::process::Output {
  timeout(DEADLINE, async {
    let mut child = Command::new("pwsh.exe")
      .args(["-NoProfile", "-NonInteractive", "-Command", script])
      .env("CTL_FORWARD_OWNER_PID", owner.to_string())
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(Stdio::piped())
      .kill_on_drop(true)
      .spawn()
      .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(input).await.unwrap();
    drop(stdin);
    child.wait_with_output().await.unwrap()
  })
  .await
  .expect("native process ownership check timed out")
}

async fn descendants(owner: &Owner) -> Vec<ProcessIdentity> {
  let owner_id = owner
    .child
    .id()
    .expect("ctld exited before its ownership check");
  let output = powershell(SNAPSHOT_DESCENDANTS, owner_id, &[]).await;
  assert!(
    output.status.success(),
    "process snapshot failed: {output:?}"
  );
  let processes: Vec<ProcessIdentity> = serde_json::from_slice(&output.stdout).unwrap();
  let ssh: Vec<_> = processes
    .iter()
    .filter(|process| process.name.eq_ignore_ascii_case("ssh.exe"))
    .collect();
  assert!(
    ssh.len() >= 4,
    "anchor, data connection and proxy descendants are missing: {processes:?}"
  );
  assert!(
    ssh
      .iter()
      .filter(|process| process.parent_id == owner_id)
      .count()
      >= 2,
    "anchor and data SSH children are missing: {processes:?}"
  );
  processes
}

async fn assert_descendants_gone(processes: &[ProcessIdentity]) {
  let input = serde_json::to_vec(processes).unwrap();
  let output = powershell(WAIT_DESCENDANTS_GONE, 0, &input).await;
  assert!(
    output.status.success(),
    "owned forwarding or proxy processes survived: {output:?}"
  );
}

struct Echo {
  port: u16,
  tag: u8,
  task: JoinHandle<()>,
}

impl Echo {
  async fn start(tag: u8) -> Self {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
      let mut connections = JoinSet::new();
      loop {
        tokio::select! {
          accepted = listener.accept() => {
            let (stream, _) = accepted.unwrap();
            connections.spawn(async move {
              let _ = echo_connection(stream, tag).await;
            });
          }
          Some(result) = connections.join_next(), if !connections.is_empty() => {
            result.unwrap();
          }
        }
      }
    });
    Self { port, tag, task }
  }

  async fn roundtrip(&self, local_port: u16) -> TcpStream {
    timeout(DEADLINE, async {
      let mut stream = TcpStream::connect(("127.0.0.1", local_port)).await.unwrap();
      stream.write_all(PAYLOAD).await.unwrap();
      let mut response = vec![0; PAYLOAD.len() + 1];
      stream.read_exact(&mut response).await.unwrap();
      assert_eq!(response[0], self.tag);
      assert_eq!(&response[1..], PAYLOAD);
      stream
    })
    .await
    .expect("forwarded binary data did not roundtrip")
  }
}

async fn echo_connection(mut stream: TcpStream, tag: u8) -> std::io::Result<()> {
  loop {
    let mut request = vec![0; PAYLOAD.len()];
    stream.read_exact(&mut request).await?;
    stream.write_all(&[tag]).await?;
    stream.write_all(&request).await?;
  }
}

impl Drop for Echo {
  fn drop(&mut self) {
    self.task.abort();
  }
}

struct Owner {
  directory: PathBuf,
  socket: PathBuf,
  child: Child,
}

impl Owner {
  async fn start() -> Self {
    let id = uuid::Uuid::new_v4();
    let directory = std::env::temp_dir().join(format!("ctld-forward-{id}"));
    std::fs::create_dir(&directory).unwrap();
    let socket = PathBuf::from(format!(r"\\.\pipe\ctld-forward-{id}"));
    let mut command = Command::new(env!("CARGO_BIN_EXE_ctld"));
    #[cfg(windows)]
    command.creation_flags(0x0000_0008);
    let child = command
      .args([
        "--socket",
        socket.to_str().unwrap(),
        "--detach-from-terminal",
      ])
      .env_remove("CTLD_ASKPASS")
      .env_remove("CTLD_IDENTITY_ASKPASS")
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .kill_on_drop(true)
      .spawn()
      .unwrap();
    let mut owner = Self {
      directory,
      socket,
      child,
    };
    timeout(DEADLINE, async {
      loop {
        assert!(
          owner.child.try_wait().unwrap().is_none(),
          "ctld exited during startup"
        );
        if matches!(
          LifecycleClient::new(owner.socket.clone()).probe().await,
          Ok(DaemonStatus::Running { .. })
        ) {
          return;
        }
        sleep(Duration::from_millis(25)).await;
      }
    })
    .await
    .expect("fixture ctld did not become ready");
    owner
  }

  async fn request(&self, request: ClientMessage) -> ServerMessage {
    timeout(DEADLINE, async {
      let mut stream = ctl_ipc::connect_existing_at(&self.socket).await.unwrap();
      ctl_ipc::write_frame(
        &mut stream,
        &ClientMessage::Handshake {
          protocol: ctl_ipc::protocol_offer(),
        },
      )
      .await
      .unwrap();
      assert!(matches!(
        ctl_ipc::read_frame::<_, ServerMessage>(&mut stream)
          .await
          .unwrap(),
        Some(ServerMessage::HandshakeAccepted { .. })
      ));
      ctl_ipc::write_frame(&mut stream, &request).await.unwrap();
      ctl_ipc::read_frame(&mut stream)
        .await
        .unwrap()
        .expect("ctld closed the request")
    })
    .await
    .expect("ctld forwarding request timed out")
  }

  async fn configure(
    &self,
    target: &SshTarget,
    forward: &LocalPortForward,
    enabled: bool,
  ) -> ServerMessage {
    self
      .request(ClientMessage::ConfigurePortForward {
        target: target.clone(),
        forward: forward.clone(),
        enabled,
      })
      .await
  }

  async fn list(&self, target: &SshTarget) -> Vec<PortForwardStatus> {
    let response = self
      .request(ClientMessage::ListPortForwards {
        target: target.clone(),
      })
      .await;
    let ServerMessage::PortForwards { statuses } = response else {
      panic!("unexpected forwarding inventory: {response:?}")
    };
    statuses
  }

  async fn cli(&self, arguments: &[&str]) -> String {
    let executable = std::env::var_os("CTL_TEST_CTL_BIN").expect("set CTL_TEST_CTL_BIN");
    let output = timeout(
      DEADLINE,
      Command::new(executable)
        .args(arguments)
        .env("CTLD_SOCKET_PATH", &self.socket)
        .env("CTLD_BIN", env!("CARGO_BIN_EXE_ctld"))
        .env("CTL_HOSTS_PATH", self.directory.join("hosts.json"))
        .env("CTL_VPNS_PATH", self.directory.join("vpns.json"))
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output(),
    )
    .await
    .expect("forwarding CLI timed out")
    .unwrap();
    assert!(output.status.success(), "CLI {arguments:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
  }

  async fn stop(&mut self) {
    timeout(
      DEADLINE,
      LifecycleClient::new(self.socket.clone())
        .preflight_stop()
        .await
        .unwrap()
        .stop(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
      timeout(DEADLINE, self.child.wait())
        .await
        .unwrap()
        .unwrap()
        .success()
    );
    assert_eq!(
      LifecycleClient::new(self.socket.clone())
        .probe()
        .await
        .unwrap(),
      DaemonStatus::Absent
    );
  }
}

impl Drop for Owner {
  fn drop(&mut self) {
    let _ = self.child.start_kill();
    let _ = std::fs::remove_dir_all(&self.directory);
  }
}

fn target() -> SshTarget {
  let alias = std::env::var("CTL_TEST_SSH_FORWARD_HOST").expect("set CTL_TEST_SSH_FORWARD_HOST");
  SshTarget {
    destination: alias.clone(),
    hostname: Some("127.0.0.1".into()),
    user: None,
    port: None,
    identity_file: None,
    ssh_config_alias: Some(alias),
    use_ssh_config_master: None,
    gateways: Vec::new(),
  }
}

async fn unused_port() -> u16 {
  TcpListener::bind(("127.0.0.1", 0))
    .await
    .unwrap()
    .local_addr()
    .unwrap()
    .port()
}

async fn forward(id: &str, remote_port: u16) -> LocalPortForward {
  LocalPortForward {
    forward_id: id.into(),
    bind_address: "127.0.0.1".into(),
    local_port: unused_port().await,
    remote_host: "127.0.0.1".into(),
    remote_port,
  }
}

fn assert_active(response: ServerMessage, expected: &LocalPortForward) {
  let ServerMessage::PortForwardConfigured { status } = response else {
    panic!("unexpected forwarding response: {response:?}")
  };
  assert_eq!(status.state, PortForwardState::Active, "{status:?}");
  assert_eq!(&status.forward, expected);
}

fn assert_failed(response: ServerMessage) {
  match response {
    ServerMessage::PortForwardConfigured { status } => {
      assert_eq!(status.state, PortForwardState::Error, "{status:?}");
    }
    ServerMessage::Error { code, .. } => assert_eq!(code, "ssh_port_forward_failed"),
    response => panic!("forwarding did not report failure: {response:?}"),
  }
}

async fn assert_released(local_port: u16) {
  timeout(DEADLINE, async {
    loop {
      if TcpListener::bind(("127.0.0.1", local_port)).await.is_ok() {
        return;
      }
      sleep(Duration::from_millis(25)).await;
    }
  })
  .await
  .expect("owned forwarding listener survived cleanup");
}

async fn assert_closed(stream: &mut TcpStream) {
  let mut byte = [0];
  match timeout(DEADLINE, stream.read(&mut byte))
    .await
    .expect("forwarding channel survived owner shutdown")
  {
    Ok(0) => {}
    Err(error)
      if matches!(
        error.kind(),
        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
      ) => {}
    result => panic!("forwarding channel remained usable after owner shutdown: {result:?}"),
  }
}

async fn cli_selectors(owner: &Owner, echo: &Echo) {
  let alias = std::env::var("CTL_TEST_SSH_FORWARD_HOST").unwrap();
  let temporary = std::env::var("CTL_TEST_SSH_HOST").unwrap();
  owner
    .cli(&[
      "host",
      "create",
      "forward-fixture",
      &alias,
      "--ssh-config",
      "--hostname",
      "127.0.0.1",
      "--json",
    ])
    .await;
  for (selector, destination, id) in [
    ("--host", "forward-fixture", "cli-saved"),
    ("--destination", temporary.as_str(), "cli-temporary"),
  ] {
    let port = unused_port().await;
    let specification = format!("127.0.0.1:{port}:127.0.0.1:{}", echo.port);
    let output = owner
      .cli(&[
        selector,
        destination,
        "port",
        "add",
        &specification,
        "--id",
        id,
        "--json",
      ])
      .await;
    let statuses: Vec<PortForwardStatus> = serde_json::from_str(&output).unwrap();
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].state, PortForwardState::Active);
    assert_eq!(statuses[0].forward.local_port, port);
    drop(echo.roundtrip(port).await);
    let output = owner
      .cli(&[selector, destination, "port", "list", "--json"])
      .await;
    let statuses: Vec<PortForwardStatus> = serde_json::from_str(&output).unwrap();
    assert!(
      statuses
        .iter()
        .any(|status| status.forward.forward_id == id && status.state == PortForwardState::Active)
    );
    owner
      .cli(&[selector, destination, "port", "remove", id])
      .await;
    assert_released(port).await;
  }
}

async fn half_closed_final_response(owner: &Owner, target: &SshTarget) {
  const RESPONSE: &[u8] = b"final\0\xff\xfe\r\nresponse";
  let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
  let port = listener.local_addr().unwrap().port();
  let backend = tokio::spawn(async move {
    let (mut stream, _) = listener.accept().await.unwrap();
    let mut request = Vec::new();
    stream.read_to_end(&mut request).await.unwrap();
    assert_eq!(request, PAYLOAD);
    stream.write_all(RESPONSE).await.unwrap();
    stream.shutdown().await.unwrap();
  });
  let forward = forward("half-closed-final-response", port).await;
  assert_active(owner.configure(target, &forward, true).await, &forward);
  timeout(DEADLINE, async {
    let mut stream = TcpStream::connect(("127.0.0.1", forward.local_port))
      .await
      .unwrap();
    stream.write_all(PAYLOAD).await.unwrap();
    stream.shutdown().await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    assert_eq!(
      response, RESPONSE,
      "final bytes were lost or readiness bytes entered the data stream"
    );
    backend.await.unwrap();
  })
  .await
  .expect("half-closed native SSH response did not complete");
  assert!(matches!(
    owner.configure(target, &forward, false).await,
    ServerMessage::PortForwardConfigured { .. }
  ));
  assert_released(forward.local_port).await;
}

async fn configure_and_failures(
  owner: &Owner,
  target: &SshTarget,
  first: &Echo,
  second: &Echo,
) -> LocalPortForward {
  assert_eq!(owner.list(target).await, Vec::<PortForwardStatus>::new());
  let mut primary = forward("primary", first.port).await;
  assert_active(owner.configure(target, &primary, true).await, &primary);
  drop(first.roundtrip(primary.local_port).await);
  assert_active(owner.configure(target, &primary, true).await, &primary);
  primary.remote_port = second.port;
  assert_active(owner.configure(target, &primary, true).await, &primary);
  drop(second.roundtrip(primary.local_port).await);
  let statuses = owner.list(target).await;
  assert_eq!(statuses.len(), 1);
  assert_eq!(statuses[0].forward, primary);

  let occupied = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
  let mut conflict = forward("occupied", first.port).await;
  conflict.local_port = occupied.local_addr().unwrap().port();
  assert_failed(owner.configure(target, &conflict, true).await);
  drop(second.roundtrip(primary.local_port).await);
  let mut unauthenticated = target.clone();
  unauthenticated.user = Some("ctl-forwarding-user-does-not-exist".into());
  assert_eq!(
    owner.list(&unauthenticated).await,
    Vec::<PortForwardStatus>::new()
  );
  let failed = forward("unauthenticated", first.port).await;
  assert_failed(owner.configure(&unauthenticated, &failed, true).await);
  assert_released(failed.local_port).await;
  drop(second.roundtrip(primary.local_port).await);

  assert!(matches!(
    owner.configure(target, &primary, false).await,
    ServerMessage::PortForwardConfigured { .. }
  ));
  assert_released(primary.local_port).await;
  assert!(
    !owner
      .list(target)
      .await
      .iter()
      .any(|status| status.forward.forward_id == primary.forward_id)
  );
  assert_active(owner.configure(target, &primary, true).await, &primary);
  primary
}

#[tokio::test]
#[ignore = "requires native Windows and authenticated loopback OpenSSH; run windows-ssh.ps1"]
async fn authenticated_forwarding_preserves_aliases_moves_data_and_releases_owned_resources() {
  timeout(Duration::from_secs(240), async {
    let first = Echo::start(b'A').await;
    let second = Echo::start(b'B').await;
    let target = target();
    let unwanted_port: u16 = std::env::var("CTL_TEST_SSH_UNWANTED_PORT")
      .unwrap()
      .parse()
      .unwrap();
    // The alias's configured LocalForward must be suppressed even when occupied.
    let unwanted = TcpListener::bind(("127.0.0.1", unwanted_port))
      .await
      .unwrap();
    let mut owner = Owner::start().await;
    let primary = configure_and_failures(&owner, &target, &first, &second).await;
    half_closed_final_response(&owner, &target).await;
    cli_selectors(&owner, &first).await;
    let mut connection = second.roundtrip(primary.local_port).await;
    let processes = descendants(&owner).await;
    drop(unwanted);
    assert_released(unwanted_port).await;
    owner.stop().await;
    assert_closed(&mut connection).await;
    assert_released(primary.local_port).await;
    assert_descendants_gone(&processes).await;

    let mut crashed = Owner::start().await;
    let forward = forward("crash-owned", first.port).await;
    assert_active(crashed.configure(&target, &forward, true).await, &forward);
    let mut connection = first.roundtrip(forward.local_port).await;
    let processes = descendants(&crashed).await;
    timeout(DEADLINE, crashed.child.kill())
      .await
      .unwrap()
      .unwrap();
    assert_closed(&mut connection).await;
    assert_released(forward.local_port).await;
    assert_descendants_gone(&processes).await;
  })
  .await
  .expect("authenticated Windows forwarding fixture timed out");
}
