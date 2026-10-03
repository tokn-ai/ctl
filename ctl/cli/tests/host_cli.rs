use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Fixture(PathBuf);
impl Fixture {
  fn new() -> Self {
    // macOS's default temporary directory can exceed Unix socket path limits.
    #[cfg(unix)]
    let root = PathBuf::from("/tmp");
    #[cfg(not(unix))]
    let root = std::env::temp_dir();
    let path = root.join(format!("ctl-host-test-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&path).unwrap();
    Self(path)
  }
  fn command(&self, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ctl"));
    command
      .args(args)
      .env("CTL_HOSTS_PATH", self.0.join("hosts.json"))
      .env("CTLD_SOCKET_PATH", self.0.join("ctld.sock"))
      .env("CTLD_BIN", self.0.join("must-not-start-ctld"));
    command
  }
  fn run(&self, args: &[&str]) -> Value {
    let output = self.command(args).output().unwrap();
    assert!(
      output.status.success(),
      "{}",
      String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
  }
  fn fails(&self, args: &[&str]) -> String {
    let output = self.command(args).output().unwrap();
    assert!(!output.status.success());
    String::from_utf8(output.stderr).unwrap()
  }
  fn bytes(&self) -> Vec<u8> {
    fs::read(self.0.join("hosts.json")).unwrap()
  }
  fn create(&self) -> Value {
    self.run(&[
      "host",
      "create",
      "work",
      "10.0.0.20",
      "--user",
      "alice",
      "--json",
    ])
  }
}
impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

#[test]
fn removed_add_command_does_not_create_or_modify_the_catalog() {
  for existing_catalog in [false, true] {
    let fixture = Fixture::new();
    let original = existing_catalog.then(|| {
      fixture.create();
      fixture.bytes()
    });
    let error = fixture.fails(&["host", "add", "other", "other", "--json"]);
    assert!(error.contains("unrecognized subcommand"), "{error}");
    assert!(error.contains("add"), "{error}");
    if let Some(original) = original {
      assert_eq!(fixture.bytes(), original);
    } else {
      assert!(!fixture.0.join("hosts.json").exists());
      assert!(!fixture.0.join("workspace.lock").exists());
    }
  }
}

#[test]
fn questionnaire_requires_a_terminal_without_mutating_the_catalog() {
  for existing_catalog in [false, true] {
    let fixture = Fixture::new();
    let original = existing_catalog.then(|| {
      fixture.create();
      fixture.bytes()
    });
    for args in [vec!["host", "create"], vec!["host", "create", "--json"]] {
      let output = fixture
        .command(&args)
        .stdin(Stdio::null())
        .output()
        .unwrap();
      assert!(!output.status.success(), "{output:?}");
      assert!(output.stdout.is_empty(), "{output:?}");
      let error = String::from_utf8_lossy(&output.stderr);
      assert!(error.contains("interactive terminal"), "{error}");
      if let Some(original) = &original {
        assert_eq!(fixture.bytes(), *original);
      } else {
        assert!(!fixture.0.join("hosts.json").exists());
        assert!(!fixture.0.join("workspace.lock").exists());
      }
      assert!(!fixture.0.join("ctld.sock").exists());
    }
  }
}

#[test]
fn explicit_create_saves_ssh_config_settings_and_only_json_without_a_terminal() {
  let fixture = Fixture::new();
  let output = fixture
    .command(&[
      "host",
      "create",
      "work",
      "work-ssh-alias",
      "--method-name",
      "Config",
      "--ssh-config",
      "--use-ssh-config-master",
      "false",
      "--hostname",
      "10.0.0.20",
      "--user",
      "alice",
      "--port",
      "2222",
      "--identity-file",
      "/keys/key with space",
      "--json",
    ])
    .stdin(Stdio::null())
    .output()
    .unwrap();
  assert!(output.status.success(), "{output:?}");
  assert!(output.stderr.is_empty(), "{output:?}");
  let created: Value = serde_json::from_slice(&output.stdout).unwrap();
  let method = &created["connection_methods"][0];
  assert_eq!(created["name"], "work");
  assert_eq!(created["preferred_method_id"], method["method_id"]);
  assert_eq!(method["name"], "Config");
  assert_eq!(method["ssh_config_alias"], "work-ssh-alias");
  assert_eq!(method["use_ssh_config_master"], false);
  assert_eq!(method["target"]["destination"], "work-ssh-alias");
  assert_eq!(method["target"]["hostname"], "10.0.0.20");
  assert_eq!(method["target"]["user"], "alice");
  assert_eq!(method["target"]["port"], 2222);
  assert_eq!(method["target"]["identity_file"], "/keys/key with space");
  assert_eq!(
    fixture.run(&["host", "show", "work", "--json"])["host"],
    created
  );
  assert!(!fixture.0.join("ctld.sock").exists());
}

#[cfg(unix)]
#[test]
fn default_catalog_round_trip_uses_ctl_root_and_ignores_former_locations() {
  let fixture = Fixture::new();
  let former = fixture.0.join(".tokn/ctmux");
  fs::create_dir_all(&former).unwrap();
  fs::write(former.join("hosts.json"), b"broken old catalog").unwrap();
  let run = |args: &[&str]| {
    let output = fixture
      .command(args)
      .env_remove("CTL_HOSTS_PATH")
      .env("HOME", &fixture.0)
      .env("XDG_CONFIG_HOME", fixture.0.join("other-config"))
      .output()
      .unwrap();
    assert!(
      output.status.success(),
      "{}",
      String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice::<Value>(&output.stdout).unwrap()
  };
  assert_eq!(run(&["host", "list", "--json"]), json!([]));
  let added = run(&["host", "create", "work", "10.0.0.20", "--json"]);
  assert_eq!(run(&["host", "show", "work", "--json"])["host"], added);
  assert!(fixture.0.join(".tokn/ctl/hosts.json").is_file());
  assert_eq!(
    fs::read(former.join("hosts.json")).unwrap(),
    b"broken old catalog"
  );
  assert!(!fixture.0.join("other-config").exists());
}

#[cfg(unix)]
#[test]
fn global_task_catalog_round_trip_uses_ctl_root() {
  let fixture = Fixture::new();
  let former = fixture.0.join("other-config/ctl");
  fs::create_dir_all(&former).unwrap();
  fs::write(former.join("tasks.json"), b"broken old task catalog").unwrap();
  let run = |args: &[&str]| {
    let output = fixture
      .command(args)
      .env("HOME", &fixture.0)
      .env("XDG_CONFIG_HOME", fixture.0.join("other-config"))
      .output()
      .unwrap();
    assert!(
      output.status.success(),
      "{}",
      String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice::<Value>(&output.stdout).unwrap()
  };
  let saved = run(&["task", "save", "build", "--global", "--", "true"]);
  assert_eq!(
    run(&["task", "definitions", "show", "build", "--global"]),
    saved
  );
  assert!(fixture.0.join(".tokn/ctl/tasks.json").is_file());
  assert_eq!(
    fs::read(former.join("tasks.json")).unwrap(),
    b"broken old task catalog"
  );
}

#[test]
fn host_crud_preserves_identity_and_unselected_connection_settings() {
  let fixture = Fixture::new();
  let added = fixture.create();
  let id = added["host_id"].as_str().unwrap();
  let identity = json!({"remote_id": "9dcefd7e-2b35-43d8-97d9-7508186dbac0", "agent_version": "0.1.0", "ctmux_restart_supported": false});
  let mut snapshot: Value = serde_json::from_slice(&fixture.bytes()).unwrap();
  snapshot["document"]["hosts"][0]["remote_info"] = identity.clone();
  fs::write(
    fixture.0.join("hosts.json"),
    serde_json::to_vec(&snapshot).unwrap(),
  )
  .unwrap();
  let updated = fixture.run(&[
    "host", "update", "work", "--name", "office", "--port", "2222", "--json",
  ]);
  assert_eq!(updated["host_id"], id);
  assert_eq!(updated["remote_info"], identity);
  assert_eq!(updated["connection_methods"][0]["target"]["user"], "alice");
  assert_eq!(updated["connection_methods"][0]["target"]["port"], 2222);
  assert_eq!(
    fixture.run(&["host", "show", id, "--json"])["host"],
    updated
  );
  let cleared = fixture.run(&["host", "update", id, "--clear", "user,port", "--json"]);
  assert!(
    cleared["connection_methods"][0]["target"]
      .get("user")
      .is_none()
  );
  assert_eq!(
    fixture.run(&["host", "remove", "office", "--json"]),
    cleared
  );
  assert_eq!(fixture.run(&["host", "list", "--json"]), json!([]));
}

#[test]
fn methods_manage_preference_and_edit_only_the_selected_route() {
  let fixture = Fixture::new();
  let first = fixture.create();
  let alternate = fixture.run(&[
    "host", "method", "add", "work", "VPN", "vpn-work", "--vpn", "company", "--prefer", "--json",
  ]);
  assert_eq!(
    alternate["preferred_method_id"],
    alternate["connection_methods"][1]["method_id"]
  );
  let updated = fixture.run(&[
    "host",
    "update",
    "work",
    "--method",
    "SSH",
    "--identity-file",
    "/keys/key with space",
    "--json",
  ]);
  assert_eq!(
    updated["connection_methods"][1],
    alternate["connection_methods"][1]
  );
  assert_eq!(
    updated["connection_methods"][0]["method_id"],
    first["connection_methods"][0]["method_id"]
  );
  let bytes = fixture.bytes();
  assert!(
    fixture
      .fails(&["host", "method", "remove", "work", "VPN"])
      .contains("preferred")
  );
  assert_eq!(fixture.bytes(), bytes);
  fixture.run(&["host", "method", "prefer", "work", "SSH", "--json"]);
  let saved = fixture.run(&[
    "host",
    "method",
    "update",
    "work",
    "VPN",
    "--name",
    "Private",
    "--destination",
    "new-work",
    "--json",
  ]);
  assert_eq!(
    saved["connection_methods"][1]["target"]["vpn_connection_id"],
    "company"
  );
  let saved = fixture.run(&["host", "method", "remove", "work", "Private", "--json"]);
  assert_eq!(saved["connection_methods"].as_array().unwrap().len(), 1);
}

#[test]
fn invalid_changes_and_unknown_selectors_do_not_modify_catalog() {
  let fixture = Fixture::new();
  fixture.create();
  let original = fixture.bytes();
  for args in [
    vec!["host", "create", "work", "other"],
    vec!["host", "update", "missing", "--name", "test"],
    vec!["host", "update", "work", "--gateway", "missing"],
    vec!["host", "update", "work", "--port", "0"],
    vec!["host", "update", "work", "--destination", "bad host"],
    vec![
      "host", "update", "work", "--user", "alice", "--clear", "user",
    ],
    vec!["host", "method", "remove", "work", "SSH"],
    vec!["host", "list", "--method", "SSH"],
    vec!["-H", "work", "host", "remove", "work"],
  ] {
    fixture.fails(&args);
    assert_eq!(fixture.bytes(), original, "{args:?}");
  }
}

#[test]
fn passive_reads_do_not_create_the_catalog_or_start_ctld() {
  let fixture = Fixture::new();
  assert_eq!(fixture.run(&["host", "list", "--json"]), json!([]));
  assert!(!fixture.0.join("hosts.json").exists());
  assert!(!fixture.0.join("workspace.lock").exists());
  fixture.create();
  let saved = fixture.bytes();
  let status = fixture.run(&["host", "status", "work", "--json"]);
  #[cfg(unix)]
  assert_eq!(status[0]["statuses"][0]["state"], "disconnected");
  #[cfg(not(unix))]
  assert_eq!(status[0]["statuses"][0]["state"], "unsupported");
  assert_eq!(fixture.bytes(), saved);
}

#[test]
fn corrupt_catalog_and_pending_migrations_are_preserved() {
  let fixture = Fixture::new();
  fs::write(fixture.0.join("hosts.json"), b"broken").unwrap();
  fixture.fails(&["host", "create", "work", "server"]);
  assert_eq!(fixture.bytes(), b"broken");
  fs::remove_file(fixture.0.join("hosts.json")).unwrap();
  fs::write(
    fixture.0.join("workspace.json"),
    br#"{"document":{"schema_version":7}}"#,
  )
  .unwrap();
  assert!(
    fixture
      .fails(&["host", "create", "work", "server"])
      .contains("migrate")
  );
  assert!(!fixture.0.join("hosts.json").exists());
}

#[test]
fn names_with_at_are_literal_and_stable_ids_disambiguate_existing_names() {
  let fixture = Fixture::new();
  let added = fixture.run(&[
    "host",
    "create",
    "alice@work",
    "server",
    "--ssh-config",
    "--json",
  ]);
  let id = added["host_id"].as_str().unwrap();
  assert_eq!(
    fixture.run(&["host", "show", "alice@work", "--json"])["host"]["host_id"],
    id
  );
  let mut snapshot: Value = serde_json::from_slice(&fixture.bytes()).unwrap();
  let mut duplicate = added;
  duplicate["host_id"] = "duplicate".into();
  snapshot["document"]["hosts"]
    .as_array_mut()
    .unwrap()
    .push(duplicate);
  fs::write(
    fixture.0.join("hosts.json"),
    serde_json::to_vec(&snapshot).unwrap(),
  )
  .unwrap();
  assert!(
    fixture
      .fails(&["host", "remove", "alice@work"])
      .contains("More than one")
  );
  fixture.run(&["host", "remove", "duplicate", "--json"]);
}

#[test]
fn status_keeps_catalog_order_beyond_the_concurrency_limit() {
  let fixture = Fixture::new();
  let host = fixture.create();
  let mut snapshot: Value = serde_json::from_slice(&fixture.bytes()).unwrap();
  for index in 1..12 {
    let mut host = host.clone();
    host["host_id"] = format!("host-{index}").into();
    host["name"] = format!("Host {index}").into();
    snapshot["document"]["hosts"]
      .as_array_mut()
      .unwrap()
      .push(host);
  }
  fs::write(
    fixture.0.join("hosts.json"),
    serde_json::to_vec(&snapshot).unwrap(),
  )
  .unwrap();
  let values = fixture.run(&["host", "status", "--json"]);
  let values = values.as_array().unwrap();
  assert_eq!(values.len(), 12);
  for (index, value) in values.iter().enumerate().skip(1) {
    assert_eq!(value["host"]["host_id"], format!("host-{index}"));
    #[cfg(unix)]
    assert_eq!(value["statuses"][0]["state"], "disconnected");
  }
}

#[cfg(unix)]
mod unix {
  use super::*;
  use ctl_ipc::{ClientMessage, ServerMessage};
  use std::process::Output;

  async fn accept(listener: &tokio::net::UnixListener) -> (ctl_ipc::Stream, ClientMessage) {
    let (mut stream, _) = listener.accept().await.unwrap();
    assert!(matches!(
      ctl_ipc::read_frame(&mut stream).await.unwrap(),
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
    let message = ctl_ipc::read_frame(&mut stream).await.unwrap().unwrap();
    (stream, message)
  }

  async fn run(fixture: &Fixture, args: &[&str]) -> Output {
    tokio::process::Command::from(fixture.command(args))
      .output()
      .await
      .unwrap()
  }

  #[tokio::test]
  async fn status_reports_all_methods_without_ensure_master_and_keeps_errors_unknown() {
    let fixture = Fixture::new();
    fixture.create();
    fixture.run(&[
      "host", "method", "add", "work", "VPN", "vpn", "--vpn", "company", "--json",
    ]);
    fixture.run(&["host", "method", "add", "work", "Other", "other", "--json"]);
    let listener = tokio::net::UnixListener::bind(fixture.0.join("ctld.sock")).unwrap();
    let broker = tokio::spawn(async move {
      for _ in 0..3 {
        let (mut stream, message) = accept(&listener).await;
        let ClientMessage::ConnectionStatus { target } = message else {
          panic!("status must remain passive")
        };
        let response = match target.destination.as_str() {
          "10.0.0.20" => ServerMessage::ConnectionStatus {
            connected: true,
            manually_disconnected: false,
          },
          "vpn" => {
            assert_eq!(
              target.gateways[0].vpn.as_ref().unwrap().connection_id,
              "company"
            );
            ServerMessage::ConnectionStatus {
              connected: false,
              manually_disconnected: true,
            }
          }
          _ => ServerMessage::Error {
            code: "observation_failed".into(),
            message: "control check failed".into(),
          },
        };
        ctl_ipc::write_frame(&mut stream, &response).await.unwrap();
      }
    });
    let output = run(&fixture, &["host", "status", "work", "--json"]).await;
    assert!(
      output.status.success(),
      "{}",
      String::from_utf8_lossy(&output.stderr)
    );
    let status: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status[0]["statuses"][0]["state"], "connected");
    assert_eq!(status[0]["statuses"][1]["state"], "paused");
    assert_eq!(status[0]["statuses"][2]["state"], "unknown");
    assert_eq!(status[0]["statuses"][2]["message"], "control check failed");
    broker.await.unwrap();
  }

  #[tokio::test]
  async fn explicit_connect_and_disconnect_use_preferred_and_all_methods_respectively() {
    let fixture = Fixture::new();
    fixture.create();
    fixture.run(&[
      "host", "method", "add", "work", "Other", "other", "--prefer", "--json",
    ]);
    let listener = tokio::net::UnixListener::bind(fixture.0.join("ctld.sock")).unwrap();
    let broker = tokio::spawn(async move {
      let (mut stream, message) = accept(&listener).await;
      let ClientMessage::EnsureMaster { target } = message else {
        panic!("expected authentication")
      };
      assert_eq!(target.destination, "other");
      ctl_ipc::write_frame(
        &mut stream,
        &ServerMessage::MasterReady {
          control_path: PathBuf::from("unused"),
        },
      )
      .await
      .unwrap();
      let mut destinations = Vec::new();
      for _ in 0..2 {
        let (mut stream, message) = accept(&listener).await;
        let ClientMessage::DisconnectMaster { target } = message else {
          panic!("expected disconnect")
        };
        destinations.push(target.destination);
        ctl_ipc::write_frame(&mut stream, &ServerMessage::MasterDisconnected)
          .await
          .unwrap();
      }
      destinations.sort();
      assert_eq!(destinations, ["10.0.0.20", "other"]);
    });
    for action in ["connect", "disconnect"] {
      let output = run(&fixture, &["host", action, "work"]).await;
      assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
      );
    }
    broker.await.unwrap();
  }

  #[test]
  fn writes_are_private_and_do_not_follow_symlinks() {
    use std::os::unix::fs::PermissionsExt as _;
    let fixture = Fixture::new();
    fixture.create();
    assert_eq!(
      fs::metadata(fixture.0.join("hosts.json"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777,
      0o600
    );
    let original = fixture.bytes();
    fs::rename(
      fixture.0.join("hosts.json"),
      fixture.0.join("original.json"),
    )
    .unwrap();
    std::os::unix::fs::symlink(
      fixture.0.join("original.json"),
      fixture.0.join("hosts.json"),
    )
    .unwrap();
    fixture.fails(&["host", "create", "other", "other"]);
    assert_eq!(fs::read(fixture.0.join("original.json")).unwrap(), original);
  }
}
