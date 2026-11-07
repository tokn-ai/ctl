#![cfg(unix)]

use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use ctl_proto::forwarding::{self, Frame, Request, Response};
use ctl_proto::transport::{Client, Service, Stream};
use ctmux_client::session::{CreateSessionRequest, SessionClient};
use ctmux_client::{
  AttachRequest, ClientIdentity, DEFAULT_PRESENTATION_WINDOW_BYTES, begin_attach,
};
use ctmux_proto::{ClientMessage, ServerMessage, TerminalSize, read_frame, write_frame};
use tokio::io::AsyncReadExt as _;
use tokio::net::{UdpSocket, UnixStream};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, timeout};

const DEADLINE: Duration = Duration::from_secs(15);

struct Directory(PathBuf);

impl Directory {
  fn new() -> Self {
    let token = uuid::Uuid::new_v4().simple().to_string();
    let path = PathBuf::from("/tmp").join(format!("ctl-agent-mux-{}", &token[..12]));
    std::fs::DirBuilder::new()
      .mode(0o700)
      .create(&path)
      .unwrap();
    Self(path)
  }
}

impl Drop for Directory {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

fn identity() -> ClientIdentity {
  ClientIdentity {
    name: "multiplex-test".into(),
    version: "fixture".into(),
  }
}

async fn open_ctmux(client: &Client) -> (Stream, String) {
  let mut stream = client.open(Service::Ctmux).await.unwrap();
  let mut marker = vec![0; ctl_proto::IDENTITY_PREFACE.len()];
  stream.read_exact(&mut marker).await.unwrap();
  assert_eq!(marker, ctl_proto::IDENTITY_PREFACE);
  let (mut reader, mut writer) = tokio::io::split(&mut stream);
  ctl_proto::negotiate_identity_contract(&mut reader, &mut writer)
    .await
    .unwrap();
  let remote = ctl_proto::read_identity(&mut stream).await.unwrap();
  assert!(remote.is_valid());
  (stream, remote.remote_id)
}

async fn wait_for_socket(path: &Path) {
  let deadline = Instant::now() + DEADLINE;
  loop {
    match UnixStream::connect(path).await {
      Ok(stream) => {
        drop(stream);
        return;
      }
      Err(_) if Instant::now() < deadline => sleep(Duration::from_millis(10)).await,
      Err(error) => panic!("fixture daemon did not start: {error}"),
    }
  }
}

async fn open_udp_rules(client: &Client) -> (Vec<Stream>, Vec<JoinHandle<()>>) {
  let mut rules = Vec::new();
  let mut echoes = Vec::new();
  for index in 0_u8..16 {
    let echo = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let address = echo.local_addr().unwrap();
    echoes.push(tokio::spawn(async move {
      let mut payload = [0; 32];
      let (size, source) = echo.recv_from(&mut payload).await.unwrap();
      let response = [vec![index], payload[..size].to_vec()].concat();
      echo.send_to(&response, source).await.unwrap();
    }));
    let mut rule = client.open(Service::Udp).await.unwrap();
    let mut marker = vec![0; forwarding::PREFACE.len()];
    rule.read_exact(&mut marker).await.unwrap();
    assert_eq!(marker, forwarding::PREFACE);
    let (mut reader, mut writer) = tokio::io::split(&mut rule);
    forwarding::negotiate_multiplex_contract(&mut reader, &mut writer)
      .await
      .unwrap();
    forwarding::write(
      &mut rule,
      &Request {
        host: address.ip().to_string(),
        port: address.port(),
      },
    )
    .await
    .unwrap();
    assert!(matches!(
      forwarding::read(&mut rule).await.unwrap(),
      Response::Ready
    ));
    forwarding::write_packet(
      &mut rule,
      &Frame::Datagram {
        peer_id: 1,
        payload: b"request".to_vec(),
      },
    )
    .await
    .unwrap();
    rules.push(rule);
  }
  (rules, echoes)
}

async fn assert_views_active(views: &mut [Stream]) {
  for (index, view) in views.iter_mut().enumerate() {
    let nonce = u64::try_from(index).unwrap();
    write_frame(view, &ClientMessage::Heartbeat { nonce })
      .await
      .unwrap();
    loop {
      let response: ServerMessage = read_frame(view).await.unwrap().unwrap();
      if matches!(response, ServerMessage::HeartbeatAck { nonce: acknowledged, .. } if acknowledged == nonce)
      {
        break;
      }
    }
  }
}

struct LiveServices {
  session_id: String,
  remote_id: String,
  views: Vec<Stream>,
  rules: Vec<Stream>,
}

async fn open_services(client: &Client) -> LiveServices {
  let (stream, remote_id) = open_ctmux(client).await;
  let session = SessionClient::new(stream, identity())
    .create(CreateSessionRequest {
      name: None,
      command: vec!["/bin/cat".into()],
      cwd: None,
      terminal_size: TerminalSize::default(),
    })
    .await
    .unwrap();
  let mut views = Vec::new();
  for _ in 0..16 {
    let (stream, received_id) = open_ctmux(client).await;
    assert_eq!(received_id, remote_id);
    let (stream, attached) = begin_attach(
      stream,
      &identity(),
      AttachRequest {
        session: session.session_id.clone(),
        resume_from: None,
        terminal_size: TerminalSize::default(),
        request_input_lease: false,
        request_layout_lease: false,
        request_command_line: false,
        request_running_command: false,
        presentation_window_bytes: DEFAULT_PRESENTATION_WINDOW_BYTES,
      },
    )
    .await
    .unwrap();
    assert_eq!(attached.session.session_id, session.session_id);
    views.push(stream);
  }
  let (mut rules, echoes) = open_udp_rules(client).await;
  for (index, rule) in rules.iter_mut().enumerate() {
    assert_eq!(
      forwarding::read_packet(rule).await.unwrap(),
      Some(Frame::Datagram {
        peer_id: 1,
        payload: [vec![u8::try_from(index).unwrap()], b"request".to_vec()].concat(),
      })
    );
  }
  for echo in echoes {
    echo.await.unwrap();
  }
  assert_views_active(&mut views).await;
  LiveServices {
    session_id: session.session_id,
    remote_id,
    views,
    rules,
  }
}

async fn verify_and_terminate_session(socket: &Path, session_id: String) {
  let retained = SessionClient::new(UnixStream::connect(socket).await.unwrap(), identity())
    .list()
    .await
    .unwrap();
  assert_eq!(retained.len(), 1);
  assert_eq!(retained[0].session_id, session_id);
  SessionClient::new(UnixStream::connect(socket).await.unwrap(), identity())
    .terminate(ctmux_client::session::SessionId(session_id))
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sixteen_views_and_udp_rules_share_one_transport_and_keep_remote_work_alive() {
  timeout(DEADLINE, async {
    let directory = Directory::new();
    let socket = directory.0.join("ctmux.sock");
    let mut daemon = tokio::spawn(ctmuxd::run(ctmuxd::DaemonConfig {
      socket_path: socket.clone(),
      startup_idle_timeout: DEADLINE,
      attachment_liveness_timeout: DEADLINE,
      ..ctmuxd::DaemonConfig::default()
    }));
    tokio::select! {
      () = wait_for_socket(&socket) => {},
      result = &mut daemon => panic!("fixture daemon exited before listening: {result:?}"),
    }
    let remote_id = uuid::Uuid::new_v4().to_string();
    let mut config = ctl_agent::ConnectConfig::new(socket.clone());
    config.identity = Some(ctl_proto::RemoteIdentity {
      remote_id: remote_id.clone(),
      agent_version: env!("CARGO_PKG_VERSION").into(),
      build: None,
      ctmux_restart_supported: false,
      bundle: None,
      protocols: ctl_agent::agent_protocols(),
    });
    let (connection, gateway) = tokio::io::duplex(64 * 1024);
    let (reader, writer) = tokio::io::split(gateway);
    let server =
      tokio::spawn(async move { ctl_agent::transport::serve(reader, writer, &config).await });
    let client = ctl_proto::transport::connect(connection).await.unwrap();
    let services = open_services(&client).await;
    assert_eq!(services.remote_id, remote_id);

    client.close();
    server.await.unwrap().unwrap();
    drop((services.views, services.rules));
    // Losing the one SSH transport removes its attachments, while the daemon
    // continues to own the shell and accepts a later independent connection.
    verify_and_terminate_session(&socket, services.session_id).await;
    daemon.await.unwrap().unwrap();
  })
  .await
  .expect("multiplexed views and UDP rules must complete within the fixture deadline");
}

fn ssh_command(alias: &str, control: &Path, remote_command: &str) -> Command {
  let mut command = Command::new("ssh");
  command
    .arg("-S")
    .arg(control)
    .args(["-T", "-o", "ControlMaster=no", "-o", "BatchMode=yes"])
    .args(["-o", "ProxyCommand=false", "-o", "ClearAllForwardings=yes"])
    .args(["-o", "StdinNull=no", "-o", "ForkAfterAuthentication=no"])
    .args(["-o", "RemoteCommand=none", "-o", "PermitLocalCommand=no"])
    .args(["--", alias, remote_command])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::inherit())
    .kill_on_drop(true);
  command
}

async fn start_anchor(alias: &str, control: &Path) -> Child {
  const READY: &[u8] = b"ctl-shared-anchor-ready\n";
  let mut anchor = ssh_command(
    alias,
    control,
    "printf 'ctl-shared-anchor-ready\\n'; exec cat",
  )
  .spawn()
  .unwrap();
  let mut marker = vec![0; READY.len()];
  anchor
    .stdout
    .as_mut()
    .unwrap()
    .read_exact(&mut marker)
    .await
    .unwrap();
  assert_eq!(marker, READY);
  anchor
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires ephemeral runner loopback sshd; run scripts/ci/caller-ssh-unix.sh"]
async fn native_ssh_limit_allows_sixteen_views_and_udp_rules_on_one_agent_session() {
  assert_eq!(std::env::var("GITHUB_ACTIONS").as_deref(), Ok("true"));
  timeout(Duration::from_secs(60), async {
    let alias = std::env::var("CTL_TEST_SHARED_SSH_ALIAS").unwrap();
    assert_eq!(alias, "ctl-caller-ci");
    let control = PathBuf::from(std::env::var_os("CTL_TEST_SHARED_SSH_CONTROL").unwrap());
    let runtime = PathBuf::from(std::env::var_os("CTL_TEST_SHARED_SSH_RUNTIME").unwrap());
    let mut anchor = start_anchor(&alias, &control).await;
    let mut gateway = ssh_command(&alias, &control, "exec ctl-agent multiplex")
      .spawn()
      .unwrap();
    let connection = tokio::io::join(
      gateway.stdout.take().unwrap(),
      gateway.stdin.take().unwrap(),
    );
    let client = ctl_proto::transport::connect(connection).await.unwrap();
    let services = open_services(&client).await;

    // The anchor and the shared agent occupy both permitted command sessions.
    // A normal third exec must fail while all 32 logical services remain live.
    let refused = ssh_command(&alias, &control, "true")
      .stderr(Stdio::piped())
      .output()
      .await
      .unwrap();
    assert!(
      !refused.status.success(),
      "the fixture must enforce MaxSessions=2"
    );
    let diagnostic = String::from_utf8_lossy(&refused.stderr);
    assert!(
      diagnostic.contains("Session open refused by peer")
        || diagnostic.contains("session request failed"),
      "the third exec failed for a different reason: {diagnostic}"
    );

    client.close();
    assert!(gateway.wait().await.unwrap().success());
    drop((services.views, services.rules));
    verify_and_terminate_session(&runtime.join("ctmux.sock"), services.session_id).await;
    anchor.kill().await.unwrap();
  })
  .await
  .expect("the authenticated shared transport must complete within the fixture deadline");
}
