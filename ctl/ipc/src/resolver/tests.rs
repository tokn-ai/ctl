use super::*;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

const CHILD_MODE: &str = "CTLD_RESOLVER_TEST_MODE";
const CHILD_DIRECTORY: &str = "CTLD_RESOLVER_TEST_DIRECTORY";

struct Fixture(PathBuf);

impl Fixture {
  fn new() -> Self {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let directory = std::env::temp_dir().join(format!(
      "ctld-resolver-{}-{}",
      std::process::id(),
      NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    Self(directory.canonicalize().unwrap())
  }

  fn helper(&self) -> PathBuf {
    let executable = self.0.join("selected-ctld");
    let metadata = serde_json::to_string(&component_info()).unwrap();
    fs::write(
      &executable,
      format!(
        "#!/bin/sh\nif [ \"$1\" = --component-info ]; then\n  printf '%s\\n' '{metadata}'\n  exit 0\nfi\nprintf '%s\\n' started > \"$CTLD_RESOLVER_TEST_DIRECTORY/started\"\nexit 1\n"
      ),
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    executable
  }

  async fn child(&self, mode: &str, explicit: bool) {
    let executable = self.helper();
    if mode == "contract" {
      fs::copy(&executable, self.0.join("contract-ctld")).unwrap();
    }
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command
      .args([
        "--exact",
        "resolver::tests::registered_resolver_child",
        "--nocapture",
      ])
      .env(CHILD_MODE, mode)
      .env(CHILD_DIRECTORY, &self.0)
      .env("HOME", self.0.join("home"))
      .env_remove(crate::DAEMON_EXECUTABLE_ENV)
      .kill_on_drop(true);
    if explicit {
      command.env(
        crate::DAEMON_EXECUTABLE_ENV,
        if mode == "missing-override" {
          self.0.join("missing-ctld")
        } else {
          executable
        },
      );
    }
    let output = tokio::time::timeout(Duration::from_secs(10), command.output())
      .await
      .unwrap()
      .unwrap();
    assert!(output.status.success(), "{mode}: {output:?}");
    assert!(!self.0.join("started").exists(), "{mode} started a service");
    assert!(
      !self.0.join("legacy-called").exists(),
      "{mode} used legacy discovery"
    );
    assert!(
      !self.0.join("home").exists(),
      "{mode} wrote installation state"
    );
  }

  fn calls(&self) -> Vec<String> {
    fs::read_to_string(self.0.join("called"))
      .unwrap_or_default()
      .lines()
      .map(str::to_owned)
      .collect()
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

fn component_info() -> ComponentInfo {
  ComponentInfo {
    build: ctl_core::component::build_info(),
    protocols: crate::lifecycle::DaemonBinaryInfo::current().protocols,
  }
}

fn directory() -> PathBuf {
  PathBuf::from(std::env::var_os(CHILD_DIRECTORY).unwrap())
}

fn test_resolver(request: Request) -> ResolutionFuture {
  Box::pin(async move {
    let directory = directory();
    writeln!(
      OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join("called"))?,
      "{:?}:{}",
      request.intent,
      request
        .required
        .map_or_else(|| "default".into(), |required| required.to_string())
    )?;
    match std::env::var(CHILD_MODE).unwrap().as_str() {
      "error" => {
        return Err(io::Error::new(
          io::ErrorKind::PermissionDenied,
          "invalid selected signature",
        ));
      }
      "missing" => return Ok(None),
      _ => {}
    }
    Ok(Some(Selection {
      executable: directory.join(
        if std::env::var(CHILD_MODE).unwrap() == "contract" && request.required.is_some() {
          "contract-ctld"
        } else {
          "selected-ctld"
        },
      ),
      source: Source::Checkout,
      info: Some(component_info()),
      archive_sha256: Some("a".repeat(64)),
      installed: std::env::var(CHILD_MODE).unwrap() != "not-installed",
      reused: true,
    }))
  })
}

fn legacy_provider() -> crate::DaemonExecutableFuture {
  Box::pin(async {
    fs::write(directory().join("legacy-called"), "called")?;
    Err(io::Error::other("legacy discovery must remain unused"))
  })
}

#[tokio::test]
async fn explicit_override_wins_for_inspection_setup_and_preparation() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let fixture = Fixture::new();
  fixture.child("override", true).await;
  assert_eq!(fixture.calls(), Vec::<String>::new());
}

#[tokio::test]
async fn a_missing_explicit_override_is_never_replaced_during_setup() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let fixture = Fixture::new();
  fixture.child("missing-override", true).await;
  assert_eq!(fixture.calls(), Vec::<String>::new());
}

#[tokio::test]
async fn registered_selection_is_shared_by_all_intents_and_restart_preflight() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let fixture = Fixture::new();
  fixture.child("selection", false).await;
  assert_eq!(
    fixture.calls(),
    [
      "Inspect:default",
      "Setup:default",
      "Prepare:default",
      "Prepare:default"
    ]
  );
}

#[tokio::test]
async fn helper_contract_is_forwarded_without_replacing_default_selection() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let fixture = Fixture::new();
  fixture.child("contract", false).await;
  assert_eq!(fixture.calls(), ["Prepare:1.1.4", "Prepare:default"]);
}

#[tokio::test]
async fn resolver_failures_and_missing_selections_do_not_fall_back() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  for mode in ["error", "missing"] {
    let fixture = Fixture::new();
    fixture.child(mode, false).await;
    assert_eq!(
      fixture.calls(),
      ["Inspect:default", "Setup:default", "Prepare:default"]
    );
  }
}

#[tokio::test]
async fn existing_socket_owner_is_reused_without_resolving_a_replacement() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let fixture = Fixture::new();
  fixture.child("existing", false).await;
  assert_eq!(fixture.calls(), Vec::<String>::new());
}

#[tokio::test]
async fn an_inspected_future_installation_cannot_be_launched() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let fixture = Fixture::new();
  fixture.child("not-installed", false).await;
  assert_eq!(fixture.calls(), ["Inspect:default", "Prepare:default"]);
}

#[tokio::test]
async fn registered_resolver_child() {
  let Ok(mode) = std::env::var(CHILD_MODE) else {
    return;
  };
  register(test_resolver).unwrap();
  assert!(
    crate::default_daemon_executable()
      .unwrap_err()
      .to_string()
      .contains("shared ctld resolver")
  );
  assert_eq!(
    register(test_resolver).unwrap_err().kind(),
    io::ErrorKind::AlreadyExists
  );
  crate::register_daemon_executable_provider(legacy_provider).unwrap();
  let executable = directory().join("selected-ctld");
  match mode.as_str() {
    "override" | "selection" => {
      for intent in [Intent::Inspect, Intent::Setup, Intent::Prepare] {
        let selection = resolve(Request::new(intent)).await.unwrap();
        assert_eq!(selection.executable, executable.canonicalize().unwrap());
        assert_eq!(
          selection.source,
          if mode == "override" {
            Source::Override
          } else {
            Source::Checkout
          }
        );
        assert!(selection.installed);
        assert_eq!(selection.info.unwrap().build, component_info().build);
      }
      if mode == "selection" {
        let prepared = crate::lifecycle::Client::new(directory().join("unused.sock"))
          .preflight_restart()
          .await
          .unwrap();
        assert!(prepared.before.is_none());
        assert_eq!(
          prepared.available.executable,
          executable.canonicalize().unwrap()
        );
      }
    }
    "contract" => {
      assert_eq!(
        crate::prepare_daemon_executable_for_helper_contract(crate::HELPER_API_CONTRACT_V1_1_4)
          .await
          .unwrap(),
        directory().join("contract-ctld")
      );
      assert_eq!(
        crate::prepare_daemon_executable().await.unwrap(),
        executable
      );
    }
    "error" | "missing" | "missing-override" => {
      for intent in [Intent::Inspect, Intent::Setup, Intent::Prepare] {
        let error = resolve(Request::new(intent)).await.unwrap_err();
        let crate::ConnectError::PrepareDaemon(error) = error else {
          panic!("unexpected resolver error: {error:?}");
        };
        assert_eq!(
          error.kind(),
          if mode == "error" {
            io::ErrorKind::PermissionDenied
          } else {
            io::ErrorKind::NotFound
          }
        );
      }
    }
    "existing" => {
      let socket = PathBuf::from(format!(
        "/tmp/ctld-resolver-owner-{}.sock",
        std::process::id()
      ));
      let listener = tokio::net::UnixListener::bind(&socket).unwrap();
      let stream = crate::connect_or_start_daemon_at(&socket).await.unwrap();
      drop((stream, listener));
      fs::remove_file(socket).unwrap();
    }
    "not-installed" => {
      let selection = resolve(Request::new(Intent::Inspect)).await.unwrap();
      assert!(!selection.installed);
      assert_eq!(
        selection.ready_executable().unwrap_err().kind(),
        io::ErrorKind::NotFound
      );
      let error = crate::prepare_daemon_executable().await.unwrap_err();
      assert!(error.to_string().contains("ctl setup"));
    }
    other => panic!("unexpected child mode: {other}"),
  }
}
