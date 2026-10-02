use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};

use ctl_ipc::lifecycle::{DaemonBinaryInfo, DaemonStatus};

struct Fixture(PathBuf);

impl Fixture {
  fn new() -> Self {
    let directory = PathBuf::from("/tmp").join(format!("ctmux-helper-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    Self(directory.canonicalize().unwrap())
  }

  fn helper(&self, relative: &str, label: &str) -> PathBuf {
    let executable = self.0.join(relative);
    fs::create_dir_all(executable.parent().unwrap()).unwrap();
    fs::write(&executable, format!(
      "#!/bin/sh\nprintf '{label}:%s\\n' \"$1\" >> \"$CTMUX_HELPER_TEST_REQUESTS\"\ncase \"$1\" in\n--credential-request) /bin/cat >/dev/null; printf '%s' '{{\"type\":\"imported\"}}';;\n--identity-request) /bin/cat >/dev/null; printf '%s' '{{\"type\":\"forgotten\"}}';;\n--component-info) printf '%s' \"$CTMUX_HELPER_TEST_COMPONENT\";;\n*) exit 91;;\nesac\n"
    )).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    executable
  }

  fn command(&self, mode: &str) -> tokio::process::Command {
    let shared = self.helper("shared/ctld.app/Contents/MacOS/ctld", "shared");
    let app = self.0.join("ctmux.app/Contents/MacOS/tests");
    fs::create_dir_all(app.parent().unwrap()).unwrap();
    fs::copy(std::env::current_exe().unwrap(), &app).unwrap();
    self.helper("ctmux.app/Contents/MacOS/ctld", "loose");
    if mode == "bundle" {
      self.helper(
        "ctmux.app/Contents/Helpers/ctld.app/Contents/MacOS/ctld",
        "bundle",
      );
    }
    let child_test = match mode {
      "about" => "about::local::tests::shared_helper_available_child",
      "about_timeout" => {
        "about::local::tests::shared_helper_timeout_preserves_running_status_child"
      }
      "restart" => "about::restart::tests::shared_helper_restart_selection_child",
      _ => "daemon_helper::tests::desktop_operation_child",
    };
    let mut command = tokio::process::Command::new(&app);
    command
      .args(["--exact", child_test, "--nocapture"])
      .env("HOME", &self.0)
      .env("PATH", &self.0)
      .env("CTLD_SOCKET_PATH", self.0.join("owner.sock"))
      .env("CTMUX_HELPER_TEST_MODE", mode)
      .env("CTMUX_HELPER_TEST_SHARED", &shared)
      .env("CTMUX_HELPER_TEST_PROVIDER", self.0.join("provider"))
      .env("CTMUX_HELPER_TEST_REQUESTS", self.0.join("requests"))
      .env(
        "CTMUX_HELPER_TEST_COMPONENT",
        serde_json::to_string(&component_info()).unwrap(),
      )
      .env_remove("CTLD_BIN")
      .env_remove("CTLD_ASKPASS")
      .env_remove("CTLD_IDENTITY_ASKPASS")
      .env_remove("CTLD_VPN_SOCKET_PATH")
      .stdin(Stdio::null())
      .kill_on_drop(true);
    if mode == "override" {
      command.env("CTLD_BIN", self.helper("explicit-ctld", "override"));
    }
    command
  }
}

fn component_info() -> ctl_core::component::ComponentInfo {
  ctl_core::component::ComponentInfo {
    build: ctl_core::component::build_info(),
    protocols: vec![
      ctl_core::component::ProtocolInfo {
        name: "ctld".into(),
        version: ctl_ipc::PROTOCOL_VERSION,
      },
      ctl_core::component::ProtocolInfo {
        name: "ctld_lifecycle".into(),
        version: ctl_ipc::lifecycle::PROTOCOL_VERSION,
      },
    ],
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

pub(crate) fn provider() -> ctl_ipc::DaemonExecutableFuture {
  Box::pin(async {
    fs::write(
      std::env::var_os("CTMUX_HELPER_TEST_PROVIDER").unwrap(),
      "verified",
    )
    .unwrap();
    if matches!(
      std::env::var("CTMUX_HELPER_TEST_MODE").as_deref(),
      Ok("timeout" | "about_timeout")
    ) && !PROVIDER_READY.load(Ordering::Relaxed)
    {
      std::future::pending::<()>().await;
    }
    Ok(Some(PathBuf::from(
      std::env::var_os("CTMUX_HELPER_TEST_SHARED").unwrap(),
    )))
  })
}

static PROVIDER_READY: AtomicBool = AtomicBool::new(false);

#[tokio::test]
async fn desktop_operations_discover_shared_helpers_and_preserve_bundle_overrides_and_passive_use()
{
  for mode in [
    "credentials",
    "identities",
    "bundle",
    "override",
    "passive",
    "vpn_owner",
    "about",
    "about_timeout",
    "restart",
    "timeout",
  ] {
    let fixture = Fixture::new();
    let marker = fixture.0.join("requests");
    let provider_marker = fixture.0.join("provider");
    let output = tokio::time::timeout(
      std::time::Duration::from_secs(10),
      fixture.command(mode).output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(output.status.success(), "{mode}: {output:?}");
    assert!(
      String::from_utf8_lossy(&output.stdout).contains("1 passed"),
      "{mode}: {output:?}"
    );
    let passive = matches!(mode, "passive" | "vpn_owner");
    assert_eq!(
      provider_marker.exists(),
      !matches!(mode, "bundle" | "override") && !passive,
      "{mode}"
    );
    if passive || matches!(mode, "timeout" | "about_timeout") {
      assert!(!marker.exists(), "{mode}");
    } else {
      let requests = fs::read_to_string(&marker).unwrap();
      let selected = if matches!(mode, "bundle" | "override") {
        mode
      } else {
        "shared"
      };
      assert!(
        requests
          .lines()
          .all(|request| request.starts_with(&format!("{selected}:"))),
        "{mode}: {requests}"
      );
    }
    assert!(!fixture.0.join(".tokn").exists(), "{mode}");
  }
}

#[tokio::test]
async fn desktop_operation_child() {
  let Ok(mode) = std::env::var("CTMUX_HELPER_TEST_MODE") else {
    return;
  };
  ctl_ipc::register_daemon_executable_provider(provider).unwrap();
  if mode == "timeout" {
    pending_discovery_times_out_without_holding_provider_lock().await;
    return;
  }
  match mode.as_str() {
    "credentials" | "bundle" | "override" => crate::credentials::import_credential_metadata()
      .await
      .unwrap(),
    "identities" => {
      crate::credentials::identities::forget_identity_passphrase(
        serde_json::from_value(serde_json::json!({"identity_id": "a".repeat(64)})).unwrap(),
      )
      .await
      .unwrap();
    }
    "passive" => {
      assert_eq!(
        crate::vpn::vpn_status().await.unwrap().connections,
        Vec::<ctl_ipc::VpnStatus>::new()
      );
      assert!(matches!(
        ctl_ipc::lifecycle::Client::new(ctl_ipc::socket_path())
          .probe()
          .await
          .unwrap(),
        DaemonStatus::Absent
      ));
    }
    "vpn_owner" => {
      let listener = tokio::net::UnixListener::bind(ctl_ipc::socket_path()).unwrap();
      let owner = async {
        let (mut stream, _) = listener.accept().await.unwrap();
        assert!(matches!(
          ctl_ipc::read_frame::<_, ctl_ipc::ClientMessage>(&mut stream)
            .await
            .unwrap(),
          Some(ctl_ipc::ClientMessage::Handshake { .. })
        ));
        ctl_ipc::write_frame(
          &mut stream,
          &ctl_ipc::ServerMessage::HandshakeAccepted {
            protocol_version: ctl_ipc::PROTOCOL_VERSION,
          },
        )
        .await
        .unwrap();
        assert!(matches!(
          ctl_ipc::read_frame::<_, ctl_ipc::ClientMessage>(&mut stream)
            .await
            .unwrap(),
          Some(ctl_ipc::ClientMessage::VpnStatus)
        ));
        ctl_ipc::write_frame(
          &mut stream,
          &ctl_ipc::ServerMessage::VpnStatus {
            status: Box::default(),
            snapshot: Some(ctl_ipc::VpnSnapshot::default()),
          },
        )
        .await
        .unwrap();
      };
      let (snapshot, ()) = tokio::join!(crate::vpn::vpn_status(), owner);
      assert_eq!(
        snapshot.unwrap().connections,
        Vec::<ctl_ipc::VpnStatus>::new()
      );
    }
    _ => panic!("unexpected fixture mode"),
  }
}

async fn pending_discovery_times_out_without_holding_provider_lock() {
  tokio::time::pause();
  let started = tokio::time::Instant::now();
  let identity =
    serde_json::from_value(serde_json::json!({"identity_id": "a".repeat(64)})).unwrap();
  let (credential, identity) = tokio::join!(
    crate::credentials::import_credential_metadata(),
    crate::credentials::identities::forget_identity_passphrase(identity)
  );
  for error in [credential.unwrap_err(), identity.unwrap_err()] {
    assert_eq!(error.code, "credential_helper_timeout");
    assert_eq!(error.message, crate::daemon_helper::TIMEOUT_MESSAGE);
  }
  let deadline = crate::daemon_helper::PREPARATION_TIMEOUT;
  assert!(
    (deadline..=deadline + std::time::Duration::from_millis(10)).contains(&started.elapsed())
  );
  // Cancellation releases the provider mutex, so subsequent requests can retry.
  PROVIDER_READY.store(true, Ordering::Relaxed);
  assert_eq!(
    crate::daemon_helper::executable().await.unwrap(),
    shared_executable()
  );
}

pub(crate) fn shared_executable() -> PathBuf {
  Path::new(&std::env::var_os("CTMUX_HELPER_TEST_SHARED").unwrap()).to_owned()
}

pub(crate) fn binary_info() -> DaemonBinaryInfo {
  DaemonBinaryInfo::current()
}
