#![cfg(unix)]

use ctld_ipc::{ClientMessage, ServerMessage};
use serde_json::json;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::AsyncWriteExt as _;

struct Fixture(PathBuf);
impl Fixture {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!("ctl-cli-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&path).unwrap();
    fs::write(path.join("hosts.json"), serde_json::to_vec(&json!({
      "revision": "test", "document": {"schema_version": 1, "ssh_gateways": [], "hosts": [
        {"host_id": "host-1", "name": "work", "preferred_method_id": "direct", "connection_methods": [
          {"method_id": "direct", "name": "Direct", "target": {"kind": "ssh", "destination": "10.0.0.20", "user": "alice", "port": 2222}}
        ]}
      ]}
    })).unwrap()).unwrap();
    Self(path)
  }
  fn script(&self, name: &str, body: &str) {
    let path = self.0.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
  }
  fn command(&self) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_ctl"));
    command.env(
      "PATH",
      format!("{}:{}", self.0.display(), std::env::var("PATH").unwrap()),
    );
    command.env("CTL_HOSTS_PATH", self.0.join("hosts.json"));
    command.env("CTLD_SOCKET_PATH", self.0.join("ctld.sock"));
    command.env("CTL_TEST_ARGS", self.0.join("args"));
    command
  }
  fn args(&self) -> Vec<String> {
    fs::read_to_string(self.0.join("args"))
      .unwrap()
      .lines()
      .map(str::to_owned)
      .collect()
  }
  fn broker(&self) -> tokio::task::JoinHandle<ctld_ipc::SshTarget> {
    let listener = tokio::net::UnixListener::bind(self.0.join("ctld.sock")).unwrap();
    let socket = self.0.join("master");
    tokio::spawn(async move {
      let (mut stream, _) = listener.accept().await.unwrap();
      assert!(matches!(
        ctld_ipc::read_frame::<_, ClientMessage>(&mut stream)
          .await
          .unwrap(),
        Some(ClientMessage::Handshake { .. })
      ));
      ctld_ipc::write_frame(
        &mut stream,
        &ServerMessage::HandshakeAccepted {
          protocol_version: ctld_ipc::PROTOCOL_VERSION,
        },
      )
      .await
      .unwrap();
      let Some(ClientMessage::EnsureMaster { target }) =
        ctld_ipc::read_frame(&mut stream).await.unwrap()
      else {
        panic!("expected master request")
      };
      ctld_ipc::write_frame(
        &mut stream,
        &ServerMessage::MasterReady {
          control_path: socket,
        },
      )
      .await
      .unwrap();
      target
    })
  }
}
impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

#[tokio::test]
async fn ssh_saved_alias_uses_broker_and_preserves_command_and_exit_status() {
  let fixture = Fixture::new();
  fixture.script("ssh", "printf '%s\\n' \"$@\" > \"$CTL_TEST_ARGS\"; exit 37");
  let broker = fixture.broker();
  let output = fixture
    .command()
    .args(["ssh", "-t", "work", "printf '%s' \"a b\""])
    .output()
    .await
    .unwrap();
  assert_eq!(
    output.status.code(),
    Some(37),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  let target = broker.await.unwrap();
  assert_eq!(target.destination, "10.0.0.20");
  assert_eq!(target.user.as_deref(), Some("alice"));
  let args = fixture.args();
  assert!(args.contains(&"ProxyCommand=false".into()));
  assert_eq!(
    &args[args.len() - 2..],
    &["10.0.0.20", "printf '%s' \"a b\""]
  );
}

#[tokio::test]
async fn ssh_explicit_options_and_unknown_hosts_do_not_start_a_managed_connection() {
  let fixture = Fixture::new();
  fixture.script("ssh", "printf '%s\\n' \"$@\" > \"$CTL_TEST_ARGS\"");
  for args in [
    &["ssh", "-p", "2200", "work"][..],
    &["ssh", "-G", "work"],
    &["ssh", "other-host", "--host", "remote-argument"],
  ] {
    let output = fixture.command().args(args).output().await.unwrap();
    assert!(
      output.status.success(),
      "{}",
      String::from_utf8_lossy(&output.stderr)
    );
    assert!(!fixture.args().contains(&"-S".into()));
  }
  assert_eq!(fixture.args(), ["other-host", "--host", "remote-argument"]);
}

#[tokio::test]
async fn scp_transport_resolves_alias_without_consuming_binary_stdin() {
  let fixture = Fixture::new();
  fixture.script(
    "ssh",
    "printf '%s\\n' \"$@\" > \"$CTL_TEST_ARGS\"; cat; exit 23",
  );
  // Exercise the executable handoff scp uses, including its option dialect.
  fixture.script("scp", "test \"$1\" = -S || exit 91\ntransport=$2\nexec \"$transport\" -x -oClearAllForwardings=yes -oRequestTTY=no -s -- work sftp");
  let broker = fixture.broker();
  let mut child = fixture
    .command()
    .args(["scp", "./file.txt", "work:/tmp/"])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
  let bytes = b"\0\xffbinary\ninput\0";
  child.stdin.take().unwrap().write_all(bytes).await.unwrap();
  let output = child.wait_with_output().await.unwrap();
  assert_eq!(
    output.status.code(),
    Some(23),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert_eq!(output.stdout, bytes);
  assert_eq!(broker.await.unwrap().user.as_deref(), Some("alice"));
  assert!(fixture.args().contains(&"10.0.0.20".into()));
}

#[tokio::test]
async fn exec_local_preserves_arguments_and_exit_status() {
  let fixture = Fixture::new();
  let output = fixture
    .command()
    .args([
      "exec",
      "--",
      "/bin/sh",
      "-c",
      "printf '%s' \"$1\"; exit 19",
      "sh",
      "a b'c",
    ])
    .output()
    .await
    .unwrap();
  assert_eq!(output.status.code(), Some(19));
  assert_eq!(output.stdout, b"a b'c");
}

#[tokio::test]
async fn persistent_shell_requires_a_terminal_before_creating_a_session() {
  let fixture = Fixture::new();
  let output = fixture.command().args(["shell"]).output().await.unwrap();
  assert!(!output.status.success());
  assert!(String::from_utf8_lossy(&output.stderr).contains("requires a terminal"));
}

#[tokio::test]
async fn real_openssh_inspection_honors_explicit_options_before_saved_defaults() {
  let fixture = Fixture::new();
  let identity = fixture.0.join("key with spaces");
  fs::write(&identity, "fixture").unwrap();
  let path = fixture.0.join("hosts.json");
  let mut catalog: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
  catalog["document"]["hosts"][0]["connection_methods"][0]["target"]["identity_file"] =
    json!(identity);
  fs::write(path, serde_json::to_vec(&catalog).unwrap()).unwrap();
  let output = fixture
    .command()
    .args(["ssh", "-G", "-F", "none", "-p", "2200", "-l", "bob", "work"])
    .output()
    .await
    .unwrap();
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  let config = String::from_utf8(output.stdout).unwrap();
  assert!(
    config
      .lines()
      .any(|value| value == format!("identityfile {}", identity.display()))
  );
  for line in ["user bob", "hostname 10.0.0.20", "port 2200"] {
    assert!(
      config.lines().any(|value| value == line),
      "missing {line}: {config}"
    );
  }
}

#[tokio::test]
async fn explicit_jump_overrides_saved_vpn_without_starting_it() {
  let fixture = Fixture::new();
  let path = fixture.0.join("hosts.json");
  let mut catalog: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
  catalog["document"]["hosts"][0]["connection_methods"][0]["target"]["vpn_connection_id"] =
    json!("office-vpn");
  fs::write(path, serde_json::to_vec(&catalog).unwrap()).unwrap();
  fixture.script("ssh", "printf '%s\\n' \"$@\" > \"$CTL_TEST_ARGS\"");
  let output = fixture
    .command()
    .args(["ssh", "-J", "other-jump", "work"])
    .output()
    .await
    .unwrap();
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  let args = fixture.args();
  assert_eq!(&args[..2], &["-J", "other-jump"]);
  assert!(!args.iter().any(|arg| arg.starts_with("ProxyCommand=")));
}

#[tokio::test]
async fn native_ports_add_list_and_remove_use_the_same_daemon_target() {
  let fixture = Fixture::new();
  let listener = tokio::net::UnixListener::bind(fixture.0.join("ctld.sock")).unwrap();
  let socket = fixture.0.join("master");
  let daemon = tokio::spawn(async move {
    let mut saved: Option<ctld_ipc::LocalPortForward> = None;
    for _ in 0..5 {
      let (mut stream, _) = listener.accept().await.unwrap();
      assert!(matches!(
        ctld_ipc::read_frame::<_, ClientMessage>(&mut stream)
          .await
          .unwrap(),
        Some(ClientMessage::Handshake { .. })
      ));
      ctld_ipc::write_frame(
        &mut stream,
        &ServerMessage::HandshakeAccepted {
          protocol_version: ctld_ipc::PROTOCOL_VERSION,
        },
      )
      .await
      .unwrap();
      let request: ClientMessage = ctld_ipc::read_frame(&mut stream).await.unwrap().unwrap();
      let response = match request {
        ClientMessage::EnsureMaster { target } => {
          assert_eq!(target.destination, "10.0.0.20");
          ServerMessage::MasterReady {
            control_path: socket.clone(),
          }
        }
        ClientMessage::ConfigurePortForward {
          target,
          forward,
          enabled,
        } => {
          assert_eq!(target.user.as_deref(), Some("alice"));
          assert_eq!(forward.remote_host, "2001:db8::2");
          assert_eq!(forward.local_port, 8080);
          saved = enabled.then(|| forward.clone());
          ServerMessage::PortForwardConfigured {
            status: ctld_ipc::PortForwardStatus {
              forward,
              state: ctld_ipc::PortForwardState::Active,
              message: None,
            },
          }
        }
        ClientMessage::ListPortForwards { target } => {
          assert_eq!(target.port, Some(2222));
          ServerMessage::PortForwards {
            statuses: saved
              .iter()
              .map(|forward| ctld_ipc::PortForwardStatus {
                forward: forward.clone(),
                state: ctld_ipc::PortForwardState::Active,
                message: None,
              })
              .collect(),
          }
        }
        _ => panic!("unexpected request"),
      };
      ctld_ipc::write_frame(&mut stream, &response).await.unwrap();
    }
    assert!(saved.is_none());
  });
  for arguments in [
    &[
      "-H",
      "work",
      "port",
      "add",
      "8080:[2001:db8::2]:80",
      "--id",
      "web",
      "--json",
    ][..],
    &["-H", "work", "port", "list", "--json"],
    &["-H", "work", "port", "remove", "web"],
  ] {
    let output = fixture.command().args(arguments).output().await.unwrap();
    assert!(
      output.status.success(),
      "{}",
      String::from_utf8_lossy(&output.stderr)
    );
    if arguments.contains(&"--json") {
      let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
      assert_eq!(status[0]["forward"]["forward_id"], "web");
    }
  }
  daemon.await.unwrap();
}
