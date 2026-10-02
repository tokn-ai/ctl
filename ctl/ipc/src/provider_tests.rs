use super::*;
use std::os::unix::fs::PermissionsExt as _;
#[cfg(target_os = "macos")]
use std::os::unix::fs::symlink;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);

impl Fixture {
  fn new() -> Self {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = env::temp_dir().join(format!(
      "ctld-provider-{}-{}",
      std::process::id(),
      NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    Self(path)
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

#[tokio::test]
async fn concurrent_preparation_reuses_one_successful_executable() {
  let calls = Arc::new(AtomicUsize::new(0));
  let observed = calls.clone();
  let provider = DaemonProvider::new(move || {
    observed.fetch_add(1, Ordering::Relaxed);
    Box::pin(async {
      sleep(Duration::from_millis(10)).await;
      Ok(Some(PathBuf::from("verified-ctld")))
    })
  });
  let (first, second) = tokio::join!(provider.prepare(), provider.prepare());
  assert_eq!(first.unwrap(), second.unwrap());
  assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn failed_or_cancelled_preparation_can_be_retried() {
  for cancelled in [false, true] {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let provider = DaemonProvider::new(move || {
      let first = observed.fetch_add(1, Ordering::Relaxed) == 0;
      Box::pin(async move {
        if first && cancelled {
          std::future::pending::<()>().await;
        }
        if first {
          Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "another setup is running",
          ))
        } else {
          Ok(Some(PathBuf::from("verified-ctld")))
        }
      })
    });
    if cancelled {
      assert!(
        timeout(Duration::from_millis(10), provider.prepare())
          .await
          .is_err()
      );
    } else {
      assert_eq!(
        provider.prepare().await.unwrap_err().kind(),
        io::ErrorKind::WouldBlock
      );
    }
    assert_eq!(
      provider.prepare().await.unwrap(),
      Some(PathBuf::from("verified-ctld"))
    );
    assert_eq!(calls.load(Ordering::Relaxed), 2);
  }
}

#[tokio::test]
async fn absent_provider_payload_preserves_sibling_discovery() {
  let fixture = Fixture::new();
  let current = fixture.0.join("ctl");
  let sibling = fixture.0.join("ctld");
  std::fs::write(&sibling, "source-built helper").unwrap();
  let provider = DaemonProvider::new(|| Box::pin(async { Ok(None) }));
  assert_eq!(
    prepare_default_daemon(&current, None, Some(&provider))
      .await
      .unwrap(),
    sibling
  );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn desktop_priority_and_managed_selection_trust_precede_provider_execution() {
  let fixture = Fixture::new();
  let calls = Arc::new(AtomicUsize::new(0));
  let observed = calls.clone();
  let provider = DaemonProvider::new(move || {
    observed.fetch_add(1, Ordering::Relaxed);
    Box::pin(async { Ok(Some(PathBuf::from("verified-new-helper"))) })
  });
  let directory = managed::ensure_component_directory(&fixture.0).unwrap();
  let current = fixture.0.join("ctl");
  symlink("../outside", directory.join("current")).unwrap();
  assert!(matches!(
    prepare_default_daemon(&current, Some(&fixture.0), Some(&provider)).await,
    Err(ConnectError::StartDaemon { .. })
  ));
  assert_eq!(calls.load(Ordering::Relaxed), 0);
  std::fs::remove_file(directory.join("current")).unwrap();
  symlink("versions/old-missing-build", directory.join("current")).unwrap();
  assert_eq!(
    prepare_default_daemon(&current, Some(&fixture.0), Some(&provider))
      .await
      .unwrap(),
    PathBuf::from("verified-new-helper")
  );
  assert_eq!(calls.load(Ordering::Relaxed), 1);

  let desktop = fixture.0.join("ctmux.app/Contents/MacOS/ctmux");
  let helper = bundled_macos_daemon(&desktop).unwrap();
  std::fs::create_dir_all(helper.parent().unwrap()).unwrap();
  std::fs::write(&helper, "desktop helper").unwrap();
  assert_eq!(
    prepare_default_daemon(&desktop, Some(&fixture.0), Some(&provider))
      .await
      .unwrap(),
    helper
  );
  assert_eq!(calls.load(Ordering::Relaxed), 1);
}

fn test_provider() -> DaemonExecutableFuture {
  Box::pin(async {
    use std::io::Write as _;
    let called = PathBuf::from(env::var_os("CTLD_PROVIDER_TEST_CALLED").unwrap());
    writeln!(
      std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(called)?,
      "called"
    )?;
    if env::var("CTLD_PROVIDER_TEST_MODE").unwrap() == "busy" {
      return Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "another setup is running",
      ));
    }
    Ok(Some(PathBuf::from(
      env::var_os("CTLD_PROVIDER_TEST_EXECUTABLE").unwrap(),
    )))
  })
}

#[tokio::test]
async fn lazy_provider_covers_startup_availability_overrides_and_passive_queries() {
  let _execution_guard = tests::SUBPROCESS_FIXTURE_LOCK.lock().await;
  let fixture = Fixture::new();
  let executable = fixture.0.join("ctld");
  let metadata = serde_json::to_string(&ctl_component_info::ComponentInfo {
    build: ctl_component_info::build_info(),
    protocols: vec![
      ctl_component_info::ProtocolInfo {
        name: "ctld".into(),
        version: PROTOCOL_VERSION,
      },
      ctl_component_info::ProtocolInfo {
        name: "ctld_lifecycle".into(),
        version: lifecycle::PROTOCOL_VERSION,
      },
    ],
  })
  .unwrap();
  std::fs::write(&executable, format!(
    "#!/bin/sh\ncase \"$1\" in\n--protocol-version) echo {PROTOCOL_VERSION};;\n--component-info) printf '%s\\n' '{metadata}';;\n*) /usr/bin/touch \"$CTLD_PROVIDER_TEST_STARTED\";;\nesac\n"
  )).unwrap();
  std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
  for mode in [
    "existing",
    "passive",
    "override",
    "explicit",
    "startup",
    "available",
    "busy",
  ] {
    let called = fixture.0.join(format!("called-{mode}"));
    let started = fixture.0.join(format!("started-{mode}"));
    let mut child = tokio::process::Command::new(env::current_exe().unwrap());
    child
      .args([
        "--exact",
        "provider_tests::registered_provider_child",
        "--nocapture",
      ])
      .env_remove(DAEMON_EXECUTABLE_ENV)
      .env("HOME", &fixture.0)
      .env("CTLD_PROVIDER_TEST_MODE", mode)
      .env("CTLD_PROVIDER_TEST_EXECUTABLE", &executable)
      .env("CTLD_PROVIDER_TEST_CALLED", &called)
      .env("CTLD_PROVIDER_TEST_STARTED", &started)
      .kill_on_drop(true);
    if mode == "override" {
      child.env(DAEMON_EXECUTABLE_ENV, &executable);
    }
    let output = timeout(Duration::from_secs(5), child.output())
      .await
      .unwrap()
      .unwrap();
    assert!(output.status.success(), "{mode}: {output:?}");
    let expected = match mode {
      "startup" | "available" => 1,
      "busy" => 2,
      _ => 0,
    };
    let calls = std::fs::read_to_string(called).unwrap_or_default();
    assert_eq!(calls.lines().count(), expected, "{mode}");
    assert_eq!(
      started.exists(),
      matches!(mode, "startup" | "override" | "explicit"),
      "{mode}"
    );
  }
}

#[tokio::test]
async fn registered_provider_child() {
  struct Endpoint(PathBuf);
  impl Drop for Endpoint {
    fn drop(&mut self) {
      let _ = std::fs::remove_file(&self.0);
    }
  }
  let Ok(mode) = env::var("CTLD_PROVIDER_TEST_MODE") else {
    return;
  };
  // Keep the socket spelling short enough for macOS sockaddr_un.
  let socket = PathBuf::from(format!(
    "/tmp/ctld-provider-owner-{}.sock",
    std::process::id()
  ));
  let _endpoint = Endpoint(socket.clone());
  let executable = PathBuf::from(env::var_os("CTLD_PROVIDER_TEST_EXECUTABLE").unwrap());
  let started = PathBuf::from(env::var_os("CTLD_PROVIDER_TEST_STARTED").unwrap());
  register_daemon_executable_provider(test_provider).unwrap();
  assert_eq!(
    register_daemon_executable_provider(test_provider)
      .unwrap_err()
      .kind(),
    io::ErrorKind::AlreadyExists
  );
  if mode == "existing" {
    let _listener = tokio::net::UnixListener::bind(&socket).unwrap();
    connect_or_start_daemon_at(&socket).await.unwrap();
  } else if mode == "passive" {
    assert!(matches!(
      lifecycle::Client::new(socket.clone())
        .probe()
        .await
        .unwrap(),
      lifecycle::DaemonStatus::Absent
    ));
    assert_eq!(
      vpn::Client::new(socket).list().await.unwrap().connections,
      []
    );
  } else if mode == "available" {
    assert_eq!(
      lifecycle::Client::new(socket)
        .available()
        .await
        .unwrap()
        .executable,
      executable.canonicalize().unwrap()
    );
    assert_eq!(prepare_daemon_executable().await.unwrap(), executable);
  } else if mode == "busy" {
    for _ in 0..2 {
      assert!(
        matches!(connect_or_start_daemon_at(&socket).await, Err(ConnectError::PrepareDaemon(error)) if error.kind() == io::ErrorKind::WouldBlock)
      );
    }
  } else {
    let explicit = (mode == "explicit").then_some(executable.as_path());
    let server = async {
      while !started.exists() {
        sleep(Duration::from_millis(5)).await;
      }
      let listener = tokio::net::UnixListener::bind(&socket).unwrap();
      listener.accept().await.unwrap();
    };
    let (connected, ()) = tokio::join!(
      connect_or_start_daemon_at_with_executable(&socket, explicit),
      server
    );
    connected.unwrap();
  }
}
