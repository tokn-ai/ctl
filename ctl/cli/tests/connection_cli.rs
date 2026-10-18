#![cfg(unix)]

use ctl_ipc::{ClientMessage, ServerMessage};
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
    ctl_core::test_fixtures::shell_command(path, format!("{body}\n")).unwrap();
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
  fn broker(&self) -> tokio::task::JoinHandle<ctl_ipc::SshTarget> {
    let listener = tokio::net::UnixListener::bind(self.0.join("ctld.sock")).unwrap();
    let socket = self.0.join("master");
    tokio::spawn(async move {
      let (mut stream, _) = listener.accept().await.unwrap();
      assert!(matches!(
        ctl_ipc::read_frame::<_, ClientMessage>(&mut stream)
          .await
          .unwrap(),
        Some(ClientMessage::Handshake { .. })
      ));
      ctl_ipc::write_frame(
        &mut stream,
        &ServerMessage::HandshakeAccepted {
          protocol_version: ctl_ipc::PROTOCOL_VERSION,
        },
      )
      .await
      .unwrap();
      let Some(ClientMessage::EnsureMaster { target }) =
        ctl_ipc::read_frame(&mut stream).await.unwrap()
      else {
        panic!("expected master request")
      };
      ctl_ipc::write_frame(
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
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
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
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
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
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
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
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
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
async fn temporary_destination_bypasses_a_saved_name_without_changing_the_catalog() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let fixture = Fixture::new();
  let catalog_path = fixture.0.join("hosts.json");
  let catalog = fs::read(&catalog_path).unwrap();
  fixture.script("ssh", "printf '%s\\n' \"$@\" > \"$CTL_TEST_ARGS\"; exit 29");
  let broker = fixture.broker();
  let output = fixture
    .command()
    .args(["--destination", "work", "exec", "--", "printf", "a b'c"])
    .output()
    .await
    .unwrap();
  assert_eq!(
    output.status.code(),
    Some(29),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  let target = broker.await.unwrap();
  assert_eq!(target.destination, "work");
  assert_eq!(target.ssh_config_alias.as_deref(), Some("work"));
  assert!(target.uses_ssh_config_master());
  assert_eq!(target.hostname, None);
  assert_eq!(target.user, None);
  assert_eq!(target.port, None);
  assert_eq!(target.identity_file, None);
  assert_eq!(target.gateways, []);
  let args = fixture.args();
  assert_eq!(&args[args.len() - 2..], &["work", "'printf' 'a b'\\''c'"]);
  assert!(!args.contains(&"10.0.0.20".into()));
  assert_eq!(fs::read(catalog_path).unwrap(), catalog);
}

#[tokio::test]
async fn temporary_destinations_work_with_an_unreadable_or_invalid_catalog() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  for (destination, invalid_catalog) in [
    ("192.168.1.1", true),
    ("mymachine.example.com", false),
    ("operator@work", true),
    ("2001:db8::1", false),
  ] {
    let fixture = Fixture::new();
    let catalog_path = fixture.0.join("hosts.json");
    if invalid_catalog {
      fs::write(&catalog_path, b"invalid catalog").unwrap();
    } else {
      fs::remove_file(&catalog_path).unwrap();
      fs::create_dir(&catalog_path).unwrap();
    }
    fixture.script("ssh", "printf '%s\\n' \"$@\" > \"$CTL_TEST_ARGS\"");
    let broker = fixture.broker();
    let output = fixture
      .command()
      .args(["--destination", destination, "shell", "--plain"])
      .output()
      .await
      .unwrap();
    assert!(
      output.status.success(),
      "{destination}: {}",
      String::from_utf8_lossy(&output.stderr)
    );
    let target = broker.await.unwrap();
    assert_eq!(target.destination, destination);
    assert_eq!(target.ssh_config_alias.as_deref(), Some(destination));
    assert!(target.uses_ssh_config_master());
    assert_eq!(target.user, None);
    assert_eq!(fixture.args().last().map(String::as_str), Some(destination));
    if invalid_catalog {
      assert_eq!(fs::read(catalog_path).unwrap(), b"invalid catalog");
    } else {
      assert!(catalog_path.is_dir());
    }
  }
}

#[tokio::test]
async fn saved_host_selection_rejects_unknown_destinations_before_connecting() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let fixture = Fixture::new();
  fixture.script("ssh", "printf '%s\\n' \"$@\" > \"$CTL_TEST_ARGS\"");
  for destination in ["192.168.1.1", "mymachine.example.com", "operator@missing"] {
    let output = fixture
      .command()
      .args(["-H", destination, "exec", "--", "true"])
      .output()
      .await
      .unwrap();
    assert!(!output.status.success());
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostic.contains("--destination"), "{diagnostic}");
  }
  assert!(!fixture.0.join("ctld.sock").exists());
  assert!(!fixture.0.join("args").exists());
}

#[tokio::test]
async fn temporary_destination_inherits_openssh_endpoint_and_identity_configuration() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  for (destination, expected_user) in [("work", "configured"), ("operator@work", "operator")] {
    let fixture = Fixture::new();
    let identity = fixture.0.join("configured key");
    fs::write(&identity, "fixture").unwrap();
    let config_path = fixture.0.join("ssh-config");
    fs::write(
      &config_path,
      format!(
        "Host work\n  HostName configured.example.com\n  User configured\n  Port 2207\n  IdentityFile \"{}\"\n  ProxyJump configured-jump\n  ControlMaster auto\n  ControlPath {}/configured-master\n",
        identity.display(),
        fixture.0.display()
      ),
    )
    .unwrap();
    fixture.script(
      "ssh",
      "exec /usr/bin/ssh -G -F \"$CTL_TEST_SSH_CONFIG\" \"$@\"",
    );
    let broker = fixture.broker();
    let output = fixture
      .command()
      .env("CTL_TEST_SSH_CONFIG", &config_path)
      .args(["--destination", destination, "shell", "--plain"])
      .output()
      .await
      .unwrap();
    assert!(
      output.status.success(),
      "{}",
      String::from_utf8_lossy(&output.stderr)
    );
    let target = broker.await.unwrap();
    assert_eq!(target.destination, destination);
    assert!(target.uses_ssh_config_master());
    assert_eq!(target.gateways, []);
    let config = String::from_utf8(output.stdout).unwrap();
    for line in [
      format!("user {expected_user}"),
      "hostname configured.example.com".into(),
      "port 2207".into(),
      format!("identityfile {}", identity.display()),
    ] {
      assert!(
        config.lines().any(|value| value == line),
        "missing {line}: {config}"
      );
    }
  }
}

#[tokio::test]
async fn persistent_shell_requires_a_terminal_before_creating_a_session() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let fixture = Fixture::new();
  for arguments in [
    &["shell"][..],
    &["shell", "--resume"][..],
    &["ctmux", "--resume"][..],
    &["-H", "work", "ctmux", "--resume"][..],
  ] {
    let output = fixture.command().args(arguments).output().await.unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("requires a terminal"));
  }
  assert!(!fixture.0.join("ctld.sock").exists());
  assert!(!fixture.0.join("args").exists());
}

#[tokio::test]
async fn real_openssh_inspection_honors_explicit_options_before_saved_defaults() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
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
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
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
async fn ssh_inspection_uses_the_status_helper_selection_without_starting_a_route() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let fixture = Fixture::new();
  let path = fixture.0.join("hosts.json");
  let mut catalog: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
  catalog["document"]["hosts"][0]["connection_methods"][0]["target"]["vpn_connection_id"] =
    json!("office-vpn");
  fs::write(path, serde_json::to_vec(&catalog).unwrap()).unwrap();
  fixture.script("ssh", "printf '%s\\n' \"$@\" > \"$CTL_TEST_ARGS\"");
  let helper = fixture.0.join("selected-ctld");
  // Executable identity checks canonicalize the selected path, so this helper
  // must be a regular executable rather than the shared command launcher.
  fs::write(&helper, "#!/bin/sh\nprintf '%s\\n' \"$1\" >> \"$CTL_TEST_HELPER_REQUESTS\"\ncase \"$1\" in\n--component-info) printf '%s' \"$CTL_TEST_HELPER_INFO\";;\n*) exit 91;;\nesac\n").unwrap();
  fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
  let requests = fixture.0.join("helper-requests");
  let binary = ctl_ipc::lifecycle::DaemonBinaryInfo::current();
  let info = ctl_core::component::ComponentInfo {
    build: binary.build,
    protocols: binary.protocols,
  };
  let command = || {
    let mut command = fixture.command();
    command
      .env("HOME", &fixture.0)
      .env("CTMUX_RUNTIME_DIR", &fixture.0)
      .env("CTL_TASKD_RUNTIME_DIR", &fixture.0)
      .env("CTLD_BIN", &helper)
      .env("CTMUXD_BIN", fixture.0.join("missing-ctmuxd"))
      .env("CTL_TASKD_BIN", fixture.0.join("missing-taskd"))
      .env("CTL_TEST_HELPER_REQUESTS", &requests)
      .env(
        "CTL_TEST_HELPER_INFO",
        serde_json::to_string(&info).unwrap(),
      );
    command
  };
  let output = command()
    .args(["components", "status", "--json"])
    .output()
    .await
    .unwrap();
  let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
  let selection = &status["components"]
    .as_array()
    .unwrap()
    .iter()
    .find(|row| row["component"] == "ctld")
    .unwrap()["available_selection"];
  assert_eq!(selection["source"], "override", "{output:?}");
  let executable = selection["executable"].as_str().unwrap();
  let output = command()
    .args(["ssh", "-G", "work"])
    .output()
    .await
    .unwrap();
  assert!(output.status.success(), "{output:?}");
  let proxy = fixture
    .args()
    .into_iter()
    .find(|argument| argument.starts_with("ProxyCommand="))
    .unwrap();
  assert!(
    proxy.starts_with(&format!("ProxyCommand='{executable}' --proxy-route ")),
    "{proxy}"
  );
  assert!(
    fs::read_to_string(requests)
      .unwrap()
      .lines()
      .all(|argument| argument == "--component-info")
  );
  assert!(!fixture.0.join("ctld.sock").exists());
  assert!(!fixture.0.join(".tokn").exists());
}

#[tokio::test]
async fn native_ports_add_list_and_remove_use_the_same_daemon_target() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let fixture = Fixture::new();
  let listener = tokio::net::UnixListener::bind(fixture.0.join("ctld.sock")).unwrap();
  let socket = fixture.0.join("master");
  let daemon = tokio::spawn(async move {
    let mut saved: Option<ctl_ipc::LocalPortForward> = None;
    for _ in 0..5 {
      let (mut stream, _) = listener.accept().await.unwrap();
      assert!(matches!(
        ctl_ipc::read_frame::<_, ClientMessage>(&mut stream)
          .await
          .unwrap(),
        Some(ClientMessage::Handshake { .. })
      ));
      ctl_ipc::write_frame(
        &mut stream,
        &ServerMessage::HandshakeAccepted {
          protocol_version: ctl_ipc::PROTOCOL_VERSION,
        },
      )
      .await
      .unwrap();
      let request: ClientMessage = ctl_ipc::read_frame(&mut stream).await.unwrap().unwrap();
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
            status: ctl_ipc::PortForwardStatus {
              forward,
              state: ctl_ipc::PortForwardState::Active,
              message: None,
            },
          }
        }
        ClientMessage::ListPortForwards { target } => {
          assert_eq!(target.port, Some(2222));
          ServerMessage::PortForwards {
            statuses: saved
              .iter()
              .map(|forward| ctl_ipc::PortForwardStatus {
                forward: forward.clone(),
                state: ctl_ipc::PortForwardState::Active,
                message: None,
              })
              .collect(),
          }
        }
        _ => panic!("unexpected request"),
      };
      ctl_ipc::write_frame(&mut stream, &response).await.unwrap();
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

fn implicit_port_add_broker(
  fixture: &Fixture,
  initial: Vec<ctl_ipc::PortForwardStatus>,
  expected_id: Option<&'static str>,
) -> tokio::task::JoinHandle<String> {
  let active = ctl_ipc::PortForwardState::Active;
  let listener = tokio::net::UnixListener::bind(fixture.0.join("ctld.sock")).unwrap();
  let socket = fixture.0.join("master");
  tokio::spawn(async move {
    let mut statuses = initial;
    let mut configured_ids = Vec::new();
    for request_index in 0..6 {
      let (mut stream, _) = listener.accept().await.unwrap();
      assert!(matches!(
        ctl_ipc::read_frame::<_, ClientMessage>(&mut stream)
          .await
          .unwrap(),
        Some(ClientMessage::Handshake { .. })
      ));
      ctl_ipc::write_frame(
        &mut stream,
        &ServerMessage::HandshakeAccepted {
          protocol_version: ctl_ipc::PROTOCOL_VERSION,
        },
      )
      .await
      .unwrap();
      let request = ctl_ipc::read_frame::<_, ClientMessage>(&mut stream)
        .await
        .unwrap()
        .unwrap();
      let response = match (request_index % 3, request) {
        (0, ClientMessage::ListPortForwards { target }) => {
          assert_eq!(target.destination, "10.0.0.20");
          assert_eq!(target.user.as_deref(), Some("alice"));
          assert_eq!(target.port, Some(2222));
          // Reordering the catalog must not change the chosen owner.
          statuses.reverse();
          ServerMessage::PortForwards {
            statuses: statuses.clone(),
          }
        }
        (1, ClientMessage::EnsureMaster { target }) => {
          assert_eq!(target.destination, "10.0.0.20");
          ServerMessage::MasterReady {
            control_path: socket.clone(),
          }
        }
        (
          2,
          ClientMessage::ConfigurePortForward {
            target,
            forward,
            enabled,
          },
        ) => {
          assert_eq!(target.destination, "10.0.0.20");
          assert_eq!(target.user.as_deref(), Some("alice"));
          assert_eq!(target.port, Some(2222));
          assert!(enabled);
          if let Some(expected) = expected_id {
            assert_eq!(forward.forward_id, expected);
          }
          configured_ids.push(forward.forward_id.clone());
          let configured = ctl_ipc::PortForwardStatus {
            forward,
            state: active,
            message: None,
          };
          statuses.retain(|status| status.forward.forward_id != configured.forward.forward_id);
          statuses.push(configured.clone());
          ServerMessage::PortForwardConfigured { status: configured }
        }
        _ => panic!("unexpected port-add request at index {request_index}"),
      };
      ctl_ipc::write_frame(&mut stream, &response).await.unwrap();
    }
    assert_eq!(configured_ids[0], configured_ids[1]);
    configured_ids[0].clone()
  })
}

#[tokio::test]
async fn repeated_implicit_port_add_reuses_the_exact_forward_and_prefers_active_records() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let forward = |id: &str| ctl_ipc::LocalPortForward {
    forward_id: id.into(),
    bind_address: "127.0.0.1".into(),
    local_port: 8080,
    remote_host: "127.0.0.1".into(),
    remote_port: 80,
  };
  let status = |id: &str, state| ctl_ipc::PortForwardStatus {
    forward: forward(id),
    state,
    message: None,
  };
  let active = ctl_ipc::PortForwardState::Active;
  let mut duplicates = vec![
    status("active-z", active),
    status("a-error", ctl_ipc::PortForwardState::Error),
    status("active-a", active),
  ];
  for different in [
    ctl_ipc::LocalPortForward {
      bind_address: "::1".into(),
      ..forward("0-other-bind")
    },
    ctl_ipc::LocalPortForward {
      local_port: 8081,
      ..forward("0-other-local-port")
    },
    ctl_ipc::LocalPortForward {
      remote_host: "database.internal".into(),
      ..forward("0-other-remote-host")
    },
    ctl_ipc::LocalPortForward {
      remote_port: 81,
      ..forward("0-other-remote-port")
    },
  ] {
    duplicates.push(ctl_ipc::PortForwardStatus {
      forward: different,
      state: active,
      message: None,
    });
  }
  for (initial, expected_id) in [
    (Vec::new(), None),
    (
      vec![status("retry", ctl_ipc::PortForwardState::Error)],
      Some("retry"),
    ),
    (duplicates, Some("active-a")),
  ] {
    let fixture = Fixture::new();
    let daemon = implicit_port_add_broker(&fixture, initial, expected_id);
    let mut returned_ids = Vec::new();
    for _ in 0..2 {
      let output = fixture
        .command()
        .args(["-H", "work", "port", "add", "8080:127.0.0.1:80", "--json"])
        .output()
        .await
        .unwrap();
      assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
      );
      let statuses: Vec<ctl_ipc::PortForwardStatus> =
        serde_json::from_slice(&output.stdout).unwrap();
      returned_ids.push(statuses[0].forward.forward_id.clone());
    }
    let configured_id = daemon.await.unwrap();
    assert_eq!(returned_ids, [configured_id.clone(), configured_id]);
  }
}
