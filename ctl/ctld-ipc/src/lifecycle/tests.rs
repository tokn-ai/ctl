use super::*;

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
    _execution_guard: tokio::sync::MutexGuard<'static, ()>,
  }

  impl Fixture {
    async fn new() -> Self {
      // Share the protocol fixtures' lock: concurrent child creation can inherit
      // a writable script descriptor before exec and cause ETXTBSY on Linux.
      let execution_guard = crate::tests::SUBPROCESS_FIXTURE_LOCK.lock().await;
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
        _execution_guard: execution_guard,
      };
      fixture.binary(&DaemonBinaryInfo::current());
      fixture
    }

    fn binary(&self, info: &DaemonBinaryInfo) {
      let metadata = ComponentInfo {
        build: info.build.clone(),
        protocols: vec![
          component_info::ProtocolInfo {
            name: "ctld".into(),
            version: info.protocol_version,
          },
          component_info::ProtocolInfo {
            name: "ctld_lifecycle".into(),
            version: info.lifecycle_protocol_version,
          },
        ],
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
      Some(Request::CtldInspect {
        protocol_version: PROTOCOL_VERSION
      })
    ));
    crate::write_frame(&mut stream, &Response::CtldInfo { info })
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
      crate::read_frame::<_, crate::ClientMessage>(&mut stream)
        .await
        .unwrap();
      crate::write_frame(
        &mut stream,
        &crate::ServerMessage::HandshakeAccepted {
          protocol_version: crate::PROTOCOL_VERSION,
        },
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
        protocol_version: Some(crate::PROTOCOL_VERSION)
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
    binary.protocol_version += 1;
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
    std::fs::copy(&fixture.executable, &next_executable).unwrap();
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
    before.binary.protocol_version -= 1;
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
