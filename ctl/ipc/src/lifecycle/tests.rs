use super::*;

#[test]
fn current_binary_advertises_the_revocation_helper_and_all_historical_contracts() {
  let info = DaemonBinaryInfo::current();
  assert!(info.is_valid());
  assert_eq!(info.protocol_version, crate::CONTRACT_V1_1_14);
  assert_eq!(info.lifecycle_protocol_version, CONTRACT_V1_0_1);
  let helper = info
    .protocols
    .iter()
    .find(|entry| entry.name == "ctld_helper")
    .unwrap();
  assert_eq!(helper.version, crate::HELPER_API_CONTRACT_V1_1_5);
  assert_eq!(helper.build, 5);
  for contract in crate::SUPPORTED_HELPER_API_VERSIONS {
    assert!(helper.supports(*contract));
  }
}

#[cfg(unix)]
mod unix {
  use super::*;
  use std::os::unix::fs::PermissionsExt as _;
  use std::sync::atomic::{AtomicU64, Ordering};
  use tokio::net::UnixListener;

  static NEXT: AtomicU64 = AtomicU64::new(0);

  struct Fixture {
    directory: PathBuf,
    socket: PathBuf,
    executable: PathBuf,
    execution_guard: ctl_core::test_fixtures::ProcessGuard,
  }

  impl Fixture {
    async fn new() -> Self {
      // Share the protocol fixtures' lock: concurrent child creation can inherit
      // a writable script descriptor before exec and cause ETXTBSY on Linux.
      let execution_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
      let directory = std::env::temp_dir().join(format!(
        "ctld-lifecycle-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
      ));
      std::fs::create_dir_all(&directory).unwrap();
      let fixture = Self {
        socket: directory.join("ctld.sock"),
        executable: directory.join("ctld"),
        directory,
        execution_guard,
      };
      fixture.binary(&DaemonBinaryInfo::current());
      fixture
    }

    fn binary(&self, info: &DaemonBinaryInfo) {
      let metadata = ComponentInfo {
        build: info.build.clone(),
        protocols: info.protocols.clone(),
      };
      std::fs::write(
        &self.executable,
        format!(
          "#!/bin/sh\n[ \"$1\" = --component-info ] || exit 17\nprintf '%s\\n' '{}'\n",
          serde_json::to_string(&metadata).unwrap()
        ),
      )
      .unwrap();
      std::fs::set_permissions(&self.executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn client(&self) -> Client {
      Client::new(self.socket.clone()).with_daemon_executable(self.executable.clone())
    }
  }

  impl Drop for Fixture {
    fn drop(&mut self) {
      let _ = std::fs::remove_dir_all(&self.directory);
    }
  }

  fn info(instance: &str) -> DaemonInfo {
    DaemonInfo {
      instance_id: instance.into(),
      binary: DaemonBinaryInfo::current(),
      active_vpn_count: 2,
    }
  }

  async fn inspect_peer(listener: &UnixListener, info: DaemonInfo) -> crate::Stream {
    let (mut stream, _) = listener.accept().await.unwrap();
    assert!(matches!(
      crate::read_frame::<_, Request>(&mut stream).await.unwrap(),
      Some(Request::CtldInspect { protocol }) if protocol.accepts(PROTOCOL_VERSION)
    ));
    crate::write_frame(
      &mut stream,
      &Response::CtldInfo {
        protocol_version: PROTOCOL_VERSION,
        info,
      },
    )
    .await
    .unwrap();
    stream
  }

  #[tokio::test]
  async fn passive_probe_never_starts_an_absent_owner() {
    let fixture = Fixture::new().await;
    assert_eq!(
      fixture.client().probe().await.unwrap(),
      DaemonStatus::Absent
    );
    assert!(!fixture.socket.exists());
    assert_eq!(
      fixture.client().available().await.unwrap().info,
      DaemonBinaryInfo::current()
    );
  }

  #[tokio::test]
  async fn legacy_owner_retains_its_known_protocol_and_cannot_be_restarted() {
    let fixture = Fixture::new().await;
    let listener = UnixListener::bind(&fixture.socket).unwrap();
    let server = tokio::spawn(async move {
      let (mut stream, _) = listener.accept().await.unwrap();
      crate::read_frame::<_, Request>(&mut stream).await.unwrap();
      drop(stream);
      let (mut stream, _) = listener.accept().await.unwrap();
      crate::read_frame::<_, serde_json::Value>(&mut stream)
        .await
        .unwrap();
      crate::write_frame(
        &mut stream,
        &serde_json::json!({ "type": "handshake_accepted", "protocol_version": crate::PROTOCOL_BUILD }),
      )
      .await
      .unwrap();
      drop(stream);
      let (mut stream, _) = listener.accept().await.unwrap();
      crate::read_frame::<_, Request>(&mut stream).await.unwrap();
    });
    assert_eq!(
      fixture.client().probe().await.unwrap(),
      DaemonStatus::Legacy {
        protocol_version: Some(crate::PROTOCOL_BUILD)
      }
    );
    assert!(matches!(
      fixture.client().preflight_restart().await,
      Err(LifecycleError::Unsupported)
    ));
    server.await.unwrap();
  }

  #[tokio::test]
  async fn incompatible_replacement_fails_before_contacting_the_owner() {
    let fixture = Fixture::new().await;
    let mut binary = DaemonBinaryInfo::current();
    binary.protocol_version = ProtocolVersion::new(1, 0, 13);
    binary.protocols[0] = ProtocolInfo::new(
      "ctld",
      13,
      binary.protocol_version,
      &[binary.protocol_version],
    );
    fixture.binary(&binary);
    let listener = UnixListener::bind(&fixture.socket).unwrap();
    assert!(matches!(
      fixture.client().preflight_restart().await,
      Err(LifecycleError::Unavailable(_))
    ));
    assert!(
      timeout(Duration::from_millis(20), listener.accept())
        .await
        .is_err()
    );
  }

  #[tokio::test]
  async fn replacement_preflight_accepts_supported_contracts_below_advertised_latest() {
    let fixture = Fixture::new().await;
    let mut binary = DaemonBinaryInfo::current();
    let newer = ProtocolVersion::new(
      1,
      crate::PROTOCOL_VERSION.minor + 1,
      crate::PROTOCOL_BUILD + 1,
    );
    binary.protocol_version = newer;
    let mut supported = crate::SUPPORTED_PROTOCOL_VERSIONS.to_vec();
    supported.push(newer);
    binary.protocols[0] = ProtocolInfo::new("ctld", newer.build, newer, &supported);
    fixture.binary(&binary);
    let prepared = fixture.client().preflight_restart().await.unwrap();
    assert_eq!(prepared.available.info, binary);
    assert!(prepared.before.is_none());
  }

  #[tokio::test]
  async fn lifecycle_inspection_validates_the_selected_contract_and_retains_advertisements() {
    for compatible in [true, false] {
      let (mut client, mut server) = crate::Stream::pair().unwrap();
      let newer = ProtocolVersion::new(1, 1, 2);
      let mut observed = info("newer");
      observed.binary.lifecycle_protocol_version = newer;
      observed.binary.protocols[1] =
        ProtocolInfo::new("ctld_lifecycle", 2, newer, &[PROTOCOL_VERSION, newer]);
      let expected = observed.clone();
      let server = tokio::spawn(async move {
        let request: Request = crate::read_frame(&mut server).await.unwrap().unwrap();
        assert!(
          matches!(request, Request::CtldInspect { protocol } if protocol.negotiate(&[PROTOCOL_VERSION, newer]) == Some(PROTOCOL_VERSION))
        );
        crate::write_frame(
          &mut server,
          &Response::CtldInfo {
            protocol_version: if compatible { PROTOCOL_VERSION } else { newer },
            info: observed,
          },
        )
        .await
        .unwrap();
      });
      let result = inspect(&mut client).await;
      if compatible {
        assert_eq!(result.unwrap(), expected);
      } else {
        assert!(matches!(result, Err(LifecycleError::Unsupported)));
      }
      server.await.unwrap();
    }
  }

  #[tokio::test]
  async fn changed_binary_does_not_stop_the_pinned_owner() {
    let fixture = Fixture::new().await;
    let listener = UnixListener::bind(&fixture.socket).unwrap();
    let server = tokio::spawn(async move {
      let mut stream = inspect_peer(&listener, info("old")).await;
      assert!(
        crate::read_frame::<_, Request>(&mut stream)
          .await
          .unwrap()
          .is_none()
      );
    });
    let prepared = fixture.client().preflight_restart().await.unwrap();
    let mut changed = DaemonBinaryInfo::current();
    changed.build.source_fingerprint = "a".repeat(64);
    fixture.binary(&changed);
    let result = prepared.restart().await;
    assert!(
      matches!(result, Err(LifecycleError::BinaryChanged)),
      "expected changed-binary rejection, got {result:?}"
    );
    server.await.unwrap();
  }

  #[tokio::test]
  async fn retargeting_the_staged_helper_rejects_restart_even_with_identical_metadata() {
    let fixture = Fixture::new().await;
    let next_executable = fixture.directory.join("ctld-next");
    fixture
      .execution_guard
      .copy(&fixture.executable, &next_executable)
      .unwrap();
    let staged_executable = fixture.directory.join("current-ctld");
    std::os::unix::fs::symlink(&fixture.executable, &staged_executable).unwrap();
    let client =
      Client::new(fixture.socket.clone()).with_daemon_executable(staged_executable.clone());
    let listener = UnixListener::bind(&fixture.socket).unwrap();
    let server = tokio::spawn(async move {
      let mut pinned = inspect_peer(&listener, info("old")).await;
      assert!(
        crate::read_frame::<_, Request>(&mut pinned)
          .await
          .unwrap()
          .is_none(),
        "changing the staged helper must not submit a restart request"
      );
    });
    let prepared = client.preflight_restart().await.unwrap();
    assert_eq!(
      prepared.available.executable,
      fixture.executable.canonicalize().unwrap()
    );
    let next_link = fixture.directory.join("next-ctld");
    std::os::unix::fs::symlink(next_executable, &next_link).unwrap();
    std::fs::rename(next_link, staged_executable).unwrap();
    assert_eq!(
      client.available().await.unwrap().info,
      prepared.available.info
    );
    assert!(matches!(
      prepared.restart().await,
      Err(LifecycleError::BinaryChanged)
    ));
    server.await.unwrap();
  }

  #[tokio::test]
  async fn changed_owner_is_not_restarted() {
    let fixture = Fixture::new().await;
    let listener = UnixListener::bind(&fixture.socket).unwrap();
    let server = tokio::spawn(async move {
      let mut pinned = inspect_peer(&listener, info("old")).await;
      drop(inspect_peer(&listener, info("replacement")).await);
      assert!(
        crate::read_frame::<_, Request>(&mut pinned)
          .await
          .unwrap()
          .is_none()
      );
    });
    let prepared = fixture.client().preflight_restart().await.unwrap();
    assert!(matches!(
      prepared.restart().await,
      Err(LifecycleError::OwnerChanged)
    ));
    server.await.unwrap();
  }

  #[tokio::test]
  async fn replacement_is_verified_only_after_graceful_owner_release() {
    replace_after_owner_release(info("old")).await;
  }

  #[tokio::test]
  async fn an_older_data_protocol_and_source_build_can_be_explicitly_restarted() {
    let mut before = info("old");
    before.binary.protocol_version = ProtocolVersion::new(1, 0, 11);
    before.binary.protocols[0] = ProtocolInfo::new(
      "ctld",
      11,
      before.binary.protocol_version,
      &[before.binary.protocol_version],
    );
    before.binary.build.source_fingerprint = "a".repeat(64);
    assert_ne!(before.binary, DaemonBinaryInfo::current());
    replace_after_owner_release(before).await;
  }

  async fn replace_after_owner_release(before: DaemonInfo) {
    let fixture = Fixture::new().await;
    let listener = UnixListener::bind(&fixture.socket).unwrap();
    let socket = fixture.socket.clone();
    let observed_before = before.clone();
    let (accepted, accepted_rx) = tokio::sync::oneshot::channel();
    let (release, release_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
      let mut pinned = inspect_peer(&listener, observed_before.clone()).await;
      drop(inspect_peer(&listener, observed_before).await);
      assert!(
        matches!(crate::read_frame::<_, Request>(&mut pinned).await.unwrap(), Some(Request::CtldRestart { expected_instance_id }) if expected_instance_id == "old")
      );
      crate::write_frame(
        &mut pinned,
        &Response::CtldRestartAccepted {
          instance_id: "old".into(),
        },
      )
      .await
      .unwrap();
      accepted.send(()).unwrap();
      release_rx.await.unwrap();
      drop(listener);
      std::fs::remove_file(&socket).unwrap();
      let successor = UnixListener::bind(&socket).unwrap();
      drop(pinned);
      // The bootstrap first observes that another verified owner already won.
      drop(successor.accept().await.unwrap());
      drop(inspect_peer(&successor, info("new")).await);
    });
    let prepared = fixture.client().preflight_restart().await.unwrap();
    let replacement = tokio::spawn(prepared.restart());
    accepted_rx.await.unwrap();
    assert!(!replacement.is_finished());
    release.send(()).unwrap();
    let result = replacement.await.unwrap().unwrap();
    assert_eq!(result.before.unwrap(), before);
    assert_eq!(result.after.instance_id, "new");
    assert_eq!(result.after.binary, DaemonBinaryInfo::current());
    server.await.unwrap();
  }
}
