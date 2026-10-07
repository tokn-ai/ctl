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
async fn operation_preparation_is_cached_separately_from_the_default_helper() {
  let calls = Arc::new(AtomicUsize::new(0));
  let observed = calls.clone();
  let provider = DaemonProvider::with_contract_policy(
    move |required| {
      observed.fetch_add(1, Ordering::Relaxed);
      Box::pin(async move {
        sleep(Duration::from_millis(10)).await;
        let path = if required == Some(HELPER_API_CONTRACT_V1_1_4) {
          "discovery-capable-helper"
        } else {
          "older-shared-helper"
        };
        Ok(Some(PathBuf::from(path)))
      })
    },
    DaemonDiscoveryPolicy::Shared,
  );
  let default = provider.prepare().await.unwrap();
  let (first, second) = tokio::join!(
    provider.prepare_for(Some(HELPER_API_CONTRACT_V1_1_4)),
    provider.prepare_for(Some(HELPER_API_CONTRACT_V1_1_4))
  );
  assert_eq!(
    first.unwrap(),
    Some(PathBuf::from("discovery-capable-helper"))
  );
  assert_eq!(
    second.unwrap(),
    Some(PathBuf::from("discovery-capable-helper"))
  );
  assert_eq!(provider.prepare().await.unwrap(), default);
  assert_eq!(prepared_daemon(Some(&provider)), default);
  assert_eq!(calls.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn preferred_operation_preparation_also_caches_verified_default_discovery() {
  let calls = Arc::new(AtomicUsize::new(0));
  let observed = calls.clone();
  let provider = DaemonProvider::with_contract_policy(
    move |required| {
      assert_eq!(required, Some(HELPER_API_CONTRACT_V1_1_4));
      observed.fetch_add(1, Ordering::Relaxed);
      Box::pin(async { Ok(Some(PathBuf::from("verified-local-helper"))) })
    },
    DaemonDiscoveryPolicy::Preferred,
  );
  let preferred = provider
    .prepare_for(Some(HELPER_API_CONTRACT_V1_1_4))
    .await
    .unwrap();
  assert_eq!(provider.prepare().await.unwrap(), preferred);
  assert_eq!(preferred_daemon(Some(&provider)), preferred);
  assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn preferred_local_discovery_precedes_selected_bundles_and_preserves_fallback() {
  let fixture = Fixture::new();
  let current = fixture.0.join("bin/ctl");
  std::fs::create_dir_all(current.parent().unwrap()).unwrap();
  let sibling = current.with_file_name("ctld");
  std::fs::write(&sibling, "ordinary sibling helper").unwrap();
  let selection = ctl_core::bundles::Store::new(&fixture.0)
    .root()
    .join("selected");
  std::fs::create_dir_all(&selection).unwrap();
  let checkpoint = selection.join(format!("local-{}.json", ctl_core::paths::native_target()));
  std::fs::write(
    &checkpoint,
    "an unrelated checkout's invalid global selection",
  )
  .unwrap();
  let local = fixture.0.join("verified-local-helper");
  let expected = local.clone();
  let provider = DaemonProvider::with_contract_policy(
    move |_| {
      let local = local.clone();
      Box::pin(async move { Ok(Some(local)) })
    },
    DaemonDiscoveryPolicy::Preferred,
  );
  assert_eq!(
    prepare_default_daemon(&current, Some(&fixture.0), Some(&provider))
      .await
      .unwrap(),
    expected,
  );
  assert_eq!(preferred_daemon(Some(&provider)), Some(expected));
  std::fs::remove_file(checkpoint).unwrap();
  let calls = Arc::new(AtomicUsize::new(0));
  let observed = calls.clone();
  let absent = DaemonProvider::with_contract_policy(
    move |_| {
      observed.fetch_add(1, Ordering::Relaxed);
      Box::pin(async { Ok(None) })
    },
    DaemonDiscoveryPolicy::Preferred,
  );
  assert_eq!(
    prepare_default_daemon(&current, Some(&fixture.0), Some(&absent))
      .await
      .unwrap(),
    sibling,
  );
  assert_eq!(calls.load(Ordering::Relaxed), 1);
  let invalid = DaemonProvider::with_contract_policy(
    |_| Box::pin(async { Err(io::Error::other("invalid local signature")) }),
    DaemonDiscoveryPolicy::Preferred,
  );
  assert!(matches!(
    prepare_default_daemon(&current, Some(&fixture.0), Some(&invalid)).await,
    Err(ConnectError::PrepareDaemon(_)),
  ));
}

#[tokio::test]
async fn development_preparation_ignores_other_selections_and_never_falls_back() {
  let fixture = Fixture::new();
  let current = fixture.0.join("bin/ctl");
  std::fs::create_dir_all(current.parent().unwrap()).unwrap();
  let sibling = current.with_file_name("ctld");
  std::fs::write(&sibling, "unrelated source-built helper").unwrap();
  let selected = ctl_core::bundles::Store::new(&fixture.0)
    .root()
    .join("selected");
  std::fs::create_dir_all(&selected).unwrap();
  std::fs::write(
    selected.join(format!("local-{}.json", ctl_core::paths::native_target())),
    "another checkout's invalid global selection",
  )
  .unwrap();
  let matching = fixture.0.join("matching-signed-development-helper");
  let expected = matching.clone();
  let provider = DaemonProvider::with_contract_policy(
    move |required| {
      assert_eq!(required, Some(HELPER_API_CONTRACT_V1_1_4));
      let path = matching.clone();
      Box::pin(async move { Ok(Some(path)) })
    },
    DaemonDiscoveryPolicy::Embedded,
  );
  assert_eq!(
    prepare_default_daemon_for_contract(
      &current,
      Some(&fixture.0),
      Some(&provider),
      Some(HELPER_API_CONTRACT_V1_1_4)
    )
    .await
    .unwrap(),
    expected,
  );
  for absent in [false, true] {
    let provider = DaemonProvider::with_contract_policy(
      move |_| {
        Box::pin(async move {
          if absent {
            Ok(None)
          } else {
            Err(io::Error::other(
              "matching bundle failed signature verification",
            ))
          }
        })
      },
      DaemonDiscoveryPolicy::Embedded,
    );
    assert!(matches!(
      prepare_default_daemon(&current, Some(&fixture.0), Some(&provider)).await,
      Err(ConnectError::PrepareDaemon(_)),
    ));
  }
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
  let current = fixture.0.join("bin/ctl");
  std::fs::create_dir_all(current.parent().unwrap()).unwrap();
  let sibling = current.with_file_name("ctld");
  std::fs::write(&sibling, "source-built helper").unwrap();
  let provider = DaemonProvider::with_policy(
    || Box::pin(async { Ok(None) }),
    DaemonDiscoveryPolicy::Shared,
  );
  assert_eq!(
    prepare_default_daemon(&current, None, Some(&provider))
      .await
      .unwrap(),
    sibling
  );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn standalone_shared_helper_precedes_bundle_after_verified_preparation() {
  let fixture = Fixture::new();
  let desktop = fixture.0.join("ctmux.app/Contents/MacOS/ctl");
  let bundled = bundled_macos_daemon(&desktop).unwrap();
  std::fs::create_dir_all(bundled.parent().unwrap()).unwrap();
  std::fs::write(&bundled, "desktop helper").unwrap();
  let shared = fixture.0.join("verified-shared-helper");
  let calls = Arc::new(AtomicUsize::new(0));
  let observed = calls.clone();
  let verified = shared.clone();
  let standalone = DaemonProvider::with_policy(
    move || {
      observed.fetch_add(1, Ordering::Relaxed);
      let executable = verified.clone();
      Box::pin(async move { Ok(Some(executable)) })
    },
    DaemonDiscoveryPolicy::Shared,
  );

  // Synchronous discovery cannot verify or install shared helpers.
  assert_eq!(
    default_macos_daemon(&desktop, Some(&fixture.0), Some(&standalone)).unwrap(),
    bundled
  );
  assert_eq!(calls.load(Ordering::Relaxed), 0);
  assert_eq!(
    prepare_default_daemon(&desktop, Some(&fixture.0), Some(&standalone))
      .await
      .unwrap(),
    shared
  );
  assert_eq!(
    default_macos_daemon(&desktop, Some(&fixture.0), Some(&standalone)).unwrap(),
    shared
  );
  assert_eq!(calls.load(Ordering::Relaxed), 1);

  let observed = calls.clone();
  let desktop_provider = DaemonProvider::new(move || {
    observed.fetch_add(1, Ordering::Relaxed);
    Box::pin(async { Ok(Some(PathBuf::from("another-helper"))) })
  });
  assert_eq!(
    prepare_default_daemon(&desktop, Some(&fixture.0), Some(&desktop_provider))
      .await
      .unwrap(),
    bundled
  );
  assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn standalone_absent_payload_does_not_select_unverified_legacy_installation() {
  let fixture = Fixture::new();
  let current = fixture.0.join("bin/ctl");
  std::fs::create_dir_all(current.parent().unwrap()).unwrap();
  let directory = managed::ensure_component_directory(&fixture.0).unwrap();
  let selection = "versions/0.1.0-aarch64-apple-darwin";
  let contents = directory.join(selection).join("ctld.app/Contents");
  std::fs::create_dir_all(contents.join("MacOS")).unwrap();
  std::fs::create_dir(contents.join("_CodeSignature")).unwrap();
  for resource in [
    "Info.plist",
    "embedded.provisionprofile",
    "_CodeSignature/CodeResources",
    "CodeResources",
    "MacOS/ctld",
  ] {
    std::fs::write(contents.join(resource), "unverified helper").unwrap();
  }
  let helper = contents.join("MacOS/ctld");
  std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
  symlink(selection, directory.join("current")).unwrap();
  // Desktop discovery retains its existing managed selection behavior.
  assert_eq!(
    default_macos_daemon(&current, Some(&fixture.0), None).unwrap(),
    helper.canonicalize().unwrap()
  );
  let standalone = DaemonProvider::with_policy(
    || Box::pin(async { Ok(None) }),
    DaemonDiscoveryPolicy::Shared,
  );
  assert_eq!(
    default_macos_daemon(&current, Some(&fixture.0), Some(&standalone)).unwrap(),
    PathBuf::from("ctld")
  );
  assert_eq!(
    prepare_default_daemon(&current, Some(&fixture.0), Some(&standalone))
      .await
      .unwrap(),
    PathBuf::from("ctld")
  );
  let sibling = current.with_file_name("ctld");
  std::fs::write(&sibling, "source-built helper").unwrap();
  assert_eq!(
    prepare_default_daemon(&current, Some(&fixture.0), Some(&standalone))
      .await
      .unwrap(),
    sibling
  );
  let bundled = bundled_macos_daemon(&current).unwrap();
  std::fs::create_dir_all(bundled.parent().unwrap()).unwrap();
  std::fs::write(&bundled, "desktop helper").unwrap();
  assert_eq!(
    prepare_default_daemon(&current, Some(&fixture.0), Some(&standalone))
      .await
      .unwrap(),
    bundled
  );
  std::fs::remove_file(directory.join("current")).unwrap();
  symlink("../outside", directory.join("current")).unwrap();
  assert!(matches!(
    prepare_default_daemon(&current, Some(&fixture.0), Some(&standalone)).await,
    Err(ConnectError::StartDaemon { .. })
  ));
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
  let current = fixture.0.join("bin/ctl");
  std::fs::create_dir_all(current.parent().unwrap()).unwrap();
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
  let _execution_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let fixture = Fixture::new();
  let executable = fixture.0.join("ctld");
  let metadata = serde_json::to_string(&ctl_core::component::ComponentInfo {
    build: ctl_core::component::build_info(),
    protocols: vec![
      ctl_core::component::ProtocolInfo::new(
        "ctld",
        PROTOCOL_BUILD,
        PROTOCOL_VERSION,
        SUPPORTED_PROTOCOL_VERSIONS,
      ),
      ctl_core::component::ProtocolInfo::new(
        "ctld_lifecycle",
        lifecycle::PROTOCOL_BUILD,
        lifecycle::PROTOCOL_VERSION,
        lifecycle::SUPPORTED_PROTOCOL_VERSIONS,
      ),
    ],
  })
  .unwrap();
  std::fs::write(&executable, format!(
    "#!/bin/sh\ncase \"$1\" in\n--protocol-version) echo {PROTOCOL_VERSION};;\n--component-info) printf '%s\\n' '{metadata}';;\n*) /usr/bin/touch \"$CTLD_PROVIDER_TEST_STARTED\";;\nesac\n"
  )).unwrap();
  std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
  for policy in ["desktop", "standalone", "development", "preferred"] {
    for mode in [
      "existing",
      "passive",
      "override",
      "explicit",
      "startup",
      "available",
      "busy",
    ] {
      let called = fixture.0.join(format!("called-{policy}-{mode}"));
      let started = fixture.0.join(format!("started-{policy}-{mode}"));
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
        .env("CTLD_PROVIDER_TEST_POLICY", policy)
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
  let policy = env::var("CTLD_PROVIDER_TEST_POLICY").unwrap();
  let registered = match policy.as_str() {
    "desktop" => register_daemon_executable_provider(test_provider),
    "standalone" => register_standalone_daemon_executable_provider(test_provider),
    "development" => register_development_daemon_executable_provider(|_| test_provider()),
    "preferred" => register_preferred_contract_daemon_executable_provider(|_| test_provider()),
    policy => panic!("unexpected provider policy {policy}"),
  };
  registered.unwrap();
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
    if policy == "preferred" {
      let home = PathBuf::from(env::var_os("HOME").unwrap());
      let selected = ctl_core::bundles::Store::new(&home).root().join("selected");
      std::fs::create_dir_all(&selected).unwrap();
      let checkpoint = selected.join(format!("local-{}.json", ctl_core::paths::native_target()));
      std::fs::write(
        &checkpoint,
        "invalid global selection after local preparation",
      )
      .unwrap();
      assert_eq!(default_daemon_executable().unwrap(), executable);
      std::fs::remove_file(checkpoint).unwrap();
    }
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
