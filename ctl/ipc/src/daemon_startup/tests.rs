use super::*;

#[cfg(unix)]
mod unix {
  use super::*;
  use ctl_core::component::ComponentInfo;
  use std::io::Write as _;
  use std::os::unix::fs::PermissionsExt as _;
  use std::sync::atomic::{AtomicU64, Ordering};
  use tokio::time::timeout;

  const CHILD_TEST: &str = "daemon_startup::tests::unix::startup_child";
  static NEXT: AtomicU64 = AtomicU64::new(0);

  struct Fixture {
    directory: PathBuf,
    endpoint: PathBuf,
    executable: PathBuf,
    _execution_guard: ctl_core::test_fixtures::ProcessGuard,
  }

  impl Fixture {
    async fn new() -> Self {
      let guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
      let directory = std::env::temp_dir().join(format!(
        "ctld-startup-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
      ));
      fs::create_dir(&directory).unwrap();
      fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
      let fixture = Self {
        endpoint: directory.join("owner.sock"),
        executable: directory.join("ctld"),
        directory,
        _execution_guard: guard,
      };
      fixture.write_script("");
      fixture
    }

    fn write_script(&self, query: &str) {
      let binary = crate::lifecycle::DaemonBinaryInfo::current();
      let metadata = serde_json::to_string(&ComponentInfo {
        build: binary.build,
        protocols: binary.protocols,
      })
      .unwrap();
      fs::write(
        &self.executable,
        format!(
          "#!/bin/sh\nset -eu\nif [ \"$1\" = --component-info ]; then\n{query}\n  printf '%s\\n' {}\n  exit 0\nfi\nexport CTLD_STARTUP_TEST_MODE=daemon\nexport CTLD_STARTUP_TEST_DIRECTORY={}\nexec {} --exact {CHILD_TEST} --nocapture\n",
          shell_quote(&metadata),
          shell_quote(&self.directory.to_string_lossy()),
          shell_quote(&std::env::current_exe().unwrap().to_string_lossy())
        ),
      )
      .unwrap();
      fs::set_permissions(&self.executable, fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn client_command(&self) -> tokio::process::Command {
      let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
      child
        .args(["--exact", CHILD_TEST, "--nocapture"])
        .env("CTLD_STARTUP_TEST_MODE", "client")
        .env("CTLD_STARTUP_TEST_DIRECTORY", &self.directory)
        .kill_on_drop(true);
      child
    }

    fn release_startup(&self) {
      fs::write(self.directory.join("ready"), b"").unwrap();
    }

    fn started(&self) -> Vec<rustix::process::Pid> {
      fs::read_to_string(self.directory.join("started"))
        .unwrap_or_default()
        .lines()
        .map(|line| rustix::process::Pid::from_raw(line.parse().unwrap()).unwrap())
        .collect()
    }

    async fn stop(&self) {
      fs::write(self.directory.join("stop"), b"").unwrap();
      wait_until(|| {
        self
          .started()
          .into_iter()
          .all(|pid| rustix::process::test_kill_process(pid).is_err())
      })
      .await;
    }
  }

  fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
  }

  impl Drop for Fixture {
    fn drop(&mut self) {
      for pid in self.started() {
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
      }
      let _ = fs::remove_dir_all(&self.directory);
      if let Ok(key) = endpoint_key(&self.endpoint) {
        let _ = fs::remove_file(startup_directory().join(key));
      }
    }
  }

  async fn wait_until(mut ready: impl FnMut() -> bool) {
    timeout(std::time::Duration::from_secs(3), async {
      while !ready() {
        sleep(std::time::Duration::from_millis(5)).await;
      }
    })
    .await
    .expect("startup fixture did not reach its expected phase");
  }

  #[tokio::test]
  async fn concurrent_processes_start_exactly_one_owner() {
    let fixture = Fixture::new().await;
    let mut clients = Vec::new();
    for _ in 0..4 {
      clients.push(fixture.client_command().spawn().unwrap());
    }
    wait_until(|| !fixture.started().is_empty()).await;
    fixture.release_startup();
    wait_until(|| {
      fs::read_dir(&fixture.directory)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
          entry
            .file_name()
            .to_string_lossy()
            .starts_with("connected-")
        })
        .count()
        == 4
    })
    .await;
    assert_eq!(fixture.started().len(), 1, "multiple daemons were launched");
    fixture.stop().await;
    for mut client in clients {
      let status = timeout(std::time::Duration::from_secs(3), client.wait())
        .await
        .unwrap()
        .unwrap();
      assert!(
        status.success(),
        "concurrent startup client exited {status}"
      );
    }
  }

  #[tokio::test]
  async fn cancelled_client_keeps_startup_coordinated_until_owner_is_ready() {
    let fixture = Fixture::new().await;
    let endpoint = fixture.endpoint.clone();
    let executable = fixture.executable.clone();
    let first = tokio::spawn(async move {
      crate::connect_or_start_daemon_at_with_executable(&endpoint, Some(&executable)).await
    });
    wait_until(|| !fixture.started().is_empty()).await;
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    let lock =
      open_lock(&startup_directory().join(endpoint_key(&fixture.endpoint).unwrap())).unwrap();
    assert!(matches!(lock.try_lock(), Err(TryLockError::WouldBlock)));
    assert!(rustix::process::test_kill_process(fixture.started()[0]).is_ok());
    fixture.release_startup();
    crate::connect_or_start_daemon_at_with_executable(&fixture.endpoint, Some(&fixture.executable))
      .await
      .unwrap();
    assert_eq!(fixture.started().len(), 1);
    fixture.stop().await;
  }

  #[tokio::test]
  async fn owner_that_appears_during_query_prevents_a_new_launch() {
    let fixture = Fixture::new().await;
    let query_started = fixture.directory.join("query-started");
    let query_release = fixture.directory.join("query-release");
    fixture.write_script(&format!(
      "  touch {}\n  while [ ! -e {} ]; do sleep 0.01; done",
      shell_quote(&query_started.to_string_lossy()),
      shell_quote(&query_release.to_string_lossy())
    ));
    let endpoint = fixture.endpoint.clone();
    let executable = fixture.executable.clone();
    let client = tokio::spawn(async move {
      crate::connect_or_start_daemon_at_with_executable(&endpoint, Some(&executable)).await
    });
    wait_until(|| query_started.exists()).await;
    let _owner = tokio::net::UnixListener::bind(&fixture.endpoint).unwrap();
    fs::write(query_release, b"").unwrap();
    client.await.unwrap().unwrap();
    assert!(
      fixture.started().is_empty(),
      "the existing owner was replaced"
    );
  }

  #[tokio::test]
  async fn exited_child_is_reaped_while_client_stays_alive() {
    let fixture = Fixture::new().await;
    let pid_file = fixture.directory.join("exited-child");
    let mut command = Command::new("/bin/sh");
    command
      .args(["-c", "printf '%s' \"$$\" > \"$1\"; exit 17", "reaper-test"])
      .arg(&pid_file);
    spawn_reaped(command, None).await.unwrap();
    wait_until(|| pid_file.exists()).await;
    let pid =
      rustix::process::Pid::from_raw(fs::read_to_string(pid_file).unwrap().parse().unwrap())
        .unwrap();
    wait_until(|| rustix::process::test_kill_process(pid).is_err()).await;
    assert!(
      matches!(
        rustix::process::waitpid(Some(pid), rustix::process::WaitOptions::NOHANG),
        Err(rustix::io::Errno::CHILD)
      ),
      "the reaper must consume the exited child, not leave a zombie"
    );
  }

  #[tokio::test]
  async fn child_and_startup_reservation_outlive_client_runtime() {
    let fixture = Fixture::new().await;
    let endpoint = fixture.endpoint.clone();
    let executable = fixture.executable.clone();
    std::thread::spawn(move || {
      let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
      runtime.block_on(async {
        let lock = StartupLock::acquire(&endpoint).await.unwrap();
        let reservation = StartupReservation(Arc::new(Mutex::new(Some(lock))));
        let mut command = Command::new(executable);
        command.arg("--socket").arg(&endpoint);
        spawn_reaped(command, Some(reservation)).await.unwrap();
      });
    })
    .join()
    .unwrap();
    wait_until(|| !fixture.started().is_empty()).await;
    assert!(rustix::process::test_kill_process(fixture.started()[0]).is_ok());
    let error = StartupLock::acquire_for(&fixture.endpoint, Duration::from_millis(50))
      .await
      .err()
      .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    let mut client = Box::pin(crate::connect_or_start_daemon_at_with_executable(
      &fixture.endpoint,
      Some(&fixture.executable),
    ));
    std::future::poll_fn(|context| {
      assert!(std::future::Future::poll(client.as_mut(), context).is_pending());
      std::task::Poll::Ready(())
    })
    .await;
    fixture.release_startup();
    timeout(Duration::from_secs(3), client)
      .await
      .expect("client did not observe the owner after its launching runtime ended")
      .unwrap();
    fixture.stop().await;
    StartupLock::acquire_for(&fixture.endpoint, Duration::from_millis(50))
      .await
      .unwrap();
  }

  #[tokio::test]
  async fn timed_out_client_retains_reservation_until_slow_owner_is_ready() {
    let fixture = Fixture::new().await;
    let error = crate::connect_or_start_daemon_at_with_executable(
      &fixture.endpoint,
      Some(&fixture.executable),
    )
    .await
    .err()
    .unwrap();
    assert!(matches!(error, ConnectError::Connect(_)));
    assert_eq!(fixture.started().len(), 1);
    let error = StartupLock::acquire_for(&fixture.endpoint, Duration::from_millis(50))
      .await
      .err()
      .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    fixture.release_startup();
    StartupLock::acquire_for(&fixture.endpoint, Duration::from_secs(1))
      .await
      .unwrap();
    crate::connect_existing_at(&fixture.endpoint).await.unwrap();
    fixture.stop().await;
  }

  #[tokio::test]
  async fn startup_child() {
    let Ok(mode) = std::env::var("CTLD_STARTUP_TEST_MODE") else {
      return;
    };
    let directory = PathBuf::from(std::env::var_os("CTLD_STARTUP_TEST_DIRECTORY").unwrap());
    let endpoint = directory.join("owner.sock");
    if mode == "client" {
      crate::connect_or_start_daemon_at_with_executable(&endpoint, Some(&directory.join("ctld")))
        .await
        .unwrap();
      fs::write(
        directory.join(format!("connected-{}", std::process::id())),
        b"",
      )
      .unwrap();
      wait_until(|| directory.join("stop").exists()).await;
      let pid = rustix::process::Pid::from_raw(
        fs::read_to_string(directory.join("started"))
          .unwrap()
          .trim()
          .parse()
          .unwrap(),
      )
      .unwrap();
      wait_until(|| rustix::process::test_kill_process(pid).is_err()).await;
      return;
    }
    let mut started = OpenOptions::new()
      .create(true)
      .append(true)
      .open(directory.join("started"))
      .unwrap();
    writeln!(started, "{}", std::process::id()).unwrap();
    drop(started);
    timeout(Duration::from_secs(10), async {
      while !directory.join("ready").exists() {
        sleep(Duration::from_millis(5)).await;
      }
    })
    .await
    .unwrap();
    let listener = tokio::net::UnixListener::bind(&endpoint).unwrap();
    while !directory.join("stop").exists() {
      tokio::select! {
        peer = listener.accept() => { peer.unwrap(); }
        () = sleep(std::time::Duration::from_millis(5)) => {}
      }
    }
  }

  #[test]
  fn lock_key_resolves_parent_aliases_and_separates_endpoints() {
    let directory = std::env::temp_dir().canonicalize().unwrap();
    let alias = directory.join(".").join("ctld-key-test.sock");
    assert_eq!(
      endpoint_key(&alias).unwrap(),
      endpoint_key(&directory.join("ctld-key-test.sock")).unwrap()
    );
    assert_ne!(
      endpoint_key(&alias).unwrap(),
      endpoint_key(&directory.join("other-owner.sock")).unwrap()
    );
  }

  #[tokio::test]
  async fn unsafe_lock_files_and_directories_are_rejected() {
    let fixture = Fixture::new().await;
    let foreign = fixture.directory.join("foreign");
    fs::write(&foreign, b"untouched").unwrap();
    let symlink = fixture.directory.join("lock-symlink");
    std::os::unix::fs::symlink(&foreign, &symlink).unwrap();
    assert!(open_lock(&symlink).is_err());
    assert_eq!(fs::read(&foreign).unwrap(), b"untouched");
    let hard_link = fixture.directory.join("lock-hard-link");
    fs::set_permissions(&foreign, fs::Permissions::from_mode(0o600)).unwrap();
    fs::hard_link(&foreign, &hard_link).unwrap();
    assert!(open_lock(&hard_link).is_err());
    assert!(open_lock(&fixture.directory).is_err());
    let broad = fixture.directory.join("public");
    fs::create_dir(&broad).unwrap();
    fs::set_permissions(&broad, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(prepare_directory(&broad).is_err());
    let linked_directory = fixture.directory.join("linked-directory");
    std::os::unix::fs::symlink(&fixture.directory, &linked_directory).unwrap();
    assert!(prepare_directory(&linked_directory).is_err());
  }
}

#[cfg(windows)]
#[test]
fn pipe_lock_key_is_case_insensitive_and_separates_endpoints() {
  assert_eq!(
    endpoint_key(Path::new(r"\\.\pipe\ctld-v1-Test")).unwrap(),
    endpoint_key(Path::new(r"\\.\pipe\CTLD-V1-test")).unwrap()
  );
  assert_ne!(
    endpoint_key(Path::new(r"\\.\pipe\ctld-v1-Test")).unwrap(),
    endpoint_key(Path::new(r"\\.\pipe\ctld-v1-Other")).unwrap()
  );
}
