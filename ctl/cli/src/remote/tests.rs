use super::*;
use std::cell::{Cell, RefCell};
use std::future::ready;
use std::sync::Arc;

fn legacy_error() -> CoreError {
  CoreError::UnsupportedSshProtocol {
    marker: "ctl-ssh-v2".into(),
  }
}

fn assert_legacy_failure<T>(result: Result<T, Error>) {
  assert!(matches!(
    result,
    Err(Error::Core(CoreError::UnsupportedSshProtocol { marker })) if marker == "ctl-ssh-v2"
  ));
}

#[tokio::test]
async fn successful_repair_retries_the_connection_once_after_installation() {
  let recovery = Recovery::default();
  let attempts = Cell::new(0);
  let events = RefCell::new(Vec::new());
  let result: Result<&str, Error> = recovery
    .connect(
      || {
        events.borrow_mut().push("connect");
        attempts.set(attempts.get() + 1);
        ready(if attempts.get() == 1 {
          Err(legacy_error())
        } else {
          Ok("compatible stream")
        })
      },
      |error| {
        assert!(matches!(error, CoreError::UnsupportedSshProtocol { .. }));
        events.borrow_mut().push("install matching components");
        ready(Ok(()))
      },
    )
    .await;
  assert_eq!(result.unwrap(), "compatible stream");
  assert_eq!(attempts.get(), 2);
  assert_eq!(
    *events.borrow(),
    ["connect", "install matching components", "connect"]
  );
}

#[tokio::test]
async fn declining_or_failing_repair_never_opens_a_replacement_connection() {
  for declined in [true, false] {
    let recovery = Recovery::default();
    let attempts = Cell::new(0);
    let repairs = Cell::new(0);
    let result: Result<&str, Error> = recovery
      .connect(
        || {
          attempts.set(attempts.get() + 1);
          ready(Err(legacy_error()))
        },
        |error| {
          repairs.set(repairs.get() + 1);
          ready(if declined {
            // The connector preserves the original failure when the user
            // declines its repair offer.
            Err(Error::Core(error))
          } else {
            Err(Error::Io(io::Error::other("bundle verification failed")))
          })
        },
      )
      .await;
    if declined {
      assert_legacy_failure(result);
    } else {
      assert!(matches!(result, Err(Error::Io(_))));
    }
    assert_eq!(attempts.get(), 1);
    assert_eq!(repairs.get(), 1);
  }
}

#[tokio::test]
async fn incompatibility_after_repair_is_returned_without_an_install_loop() {
  let recovery = Recovery::default();
  let attempts = Cell::new(0);
  let repairs = Cell::new(0);
  let connect = || {
    attempts.set(attempts.get() + 1);
    ready(Err::<&str, _>(legacy_error()))
  };
  let result = recovery
    .connect(connect, |_| {
      repairs.set(repairs.get() + 1);
      ready(Ok::<_, Error>(()))
    })
    .await;
  assert_legacy_failure(result);
  assert_eq!(attempts.get(), 2);
  assert_eq!(repairs.get(), 1);

  let result = recovery
    .connect(connect, |_| {
      repairs.set(repairs.get() + 1);
      ready(Ok::<_, Error>(()))
    })
    .await;
  assert_legacy_failure(result);
  assert_eq!(attempts.get(), 3);
  assert_eq!(repairs.get(), 1);
}

#[tokio::test]
async fn a_later_failure_after_initial_success_does_not_prompt() {
  let recovery = Recovery::default();
  let repairs = Cell::new(0);
  let first: Result<&str, Error> = recovery
    .connect(
      || ready(Ok("connected")),
      |_| {
        repairs.set(repairs.get() + 1);
        ready(Ok(()))
      },
    )
    .await;
  assert_eq!(first.unwrap(), "connected");
  let later = recovery
    .connect(
      || ready(Err::<&str, _>(legacy_error())),
      |_| {
        repairs.set(repairs.get() + 1);
        ready(Ok::<_, Error>(()))
      },
    )
    .await;
  assert_legacy_failure(later);
  assert_eq!(repairs.get(), 0);
}

#[tokio::test]
async fn cloned_recovery_state_allows_only_one_concurrent_repair_offer() {
  let original = Arc::new(Recovery::default());
  let cloned = Arc::clone(&original);
  let repairs = Cell::new(0);
  let connect = || async {
    // Both channels are opening before either reports its incompatible agent.
    tokio::task::yield_now().await;
    Err::<&str, _>(legacy_error())
  };
  let repair = |error| {
    repairs.set(repairs.get() + 1);
    ready(Err::<(), _>(Error::Core(error)))
  };
  let (first, second) = tokio::join!(
    original.connect(connect, repair),
    cloned.connect(connect, repair),
  );
  assert_legacy_failure(first);
  assert_legacy_failure(second);
  assert_eq!(repairs.get(), 1);
}

#[test]
fn repair_is_limited_to_known_old_or_missing_agents() {
  assert!(repair_reason(&CoreError::AgentNotFound).is_some());
  assert!(repair_reason(&CoreError::IdentityUnsupported).is_some());
  assert!(repair_reason(&legacy_error()).is_some());
  assert!(
    repair_reason(&CoreError::UnsupportedSshProtocol {
      marker: "ctl-ssh-v4".into()
    })
    .is_none()
  );
  assert!(repair_reason(&CoreError::InvalidSshPreface("banner".into())).is_none());
}

#[tokio::test]
async fn future_protocols_and_windows_never_offer_or_probe_for_repair() {
  // Reaching SSH validation with this destination would return an error. The
  // rejected policies must instead return false before any remote operation.
  let destination = "invalid\nhost";
  let settings = ConnectionTargetDto::ssh(destination);
  let future = CoreError::UnsupportedSshProtocol {
    marker: "ctl-ssh-v4".into(),
  };
  assert!(
    !offer_repair(
      &future,
      destination,
      &SshConnectionOptions::default(),
      &SshInteraction::Batch,
      RemoteService::Ctmux,
      &settings,
      &Recovery::default(),
    )
    .await
    .unwrap()
  );
  let windows = SshConnectionOptions {
    remote_platform: ctl_client::RemotePlatform::Windows,
    ..SshConnectionOptions::default()
  };
  for error in [legacy_error(), CoreError::AgentNotFound] {
    assert!(
      !offer_repair(
        &error,
        destination,
        &windows,
        &SshInteraction::Batch,
        RemoteService::Ctmux,
        &settings,
        &Recovery::default(),
      )
      .await
      .unwrap()
    );
  }
}

#[test]
fn nonterminal_repair_offer_leaves_piped_command_input_untouched() {
  use std::process::{Command, Stdio};

  let mut child = Command::new(std::env::current_exe().unwrap())
    .args([
      "--exact",
      "remote::tests::nonterminal_offer_preserves_piped_input_child",
      "--nocapture",
    ])
    .env("CTL_REMOTE_REPAIR_INPUT_CHILD", "yes")
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
  let mut input = child.stdin.take().unwrap();
  input.write_all(b"command input\n").unwrap();
  drop(input);
  let output = child.wait_with_output().unwrap();
  assert!(
    output.status.success(),
    "{}{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
}

#[tokio::test]
async fn nonterminal_offer_preserves_piped_input_child() {
  use std::io::Read as _;

  if std::env::var("CTL_REMOTE_REPAIR_INPUT_CHILD").as_deref() != Ok("yes") {
    return;
  }
  assert!(!io::stdin().is_terminal());
  assert!(!io::stderr().is_terminal());
  let destination = "invalid\nhost";
  let settings = ConnectionTargetDto::ssh(destination);
  for error in [
    legacy_error(),
    CoreError::AgentNotFound,
    CoreError::IdentityUnsupported,
  ] {
    assert!(
      !offer_repair(
        &error,
        destination,
        &SshConnectionOptions::default(),
        &SshInteraction::Batch,
        RemoteService::Ctmux,
        &settings,
        &Recovery::default(),
      )
      .await
      .unwrap()
    );
  }
  let mut remaining = String::new();
  io::stdin().read_to_string(&mut remaining).unwrap();
  assert_eq!(remaining, "command input\n");
}

#[cfg(unix)]
struct SignalChild(Option<std::process::Child>);

#[cfg(unix)]
impl Drop for SignalChild {
  fn drop(&mut self) {
    if let Some(child) = self.0.as_mut() {
      let _ = child.kill();
      let _ = child.wait();
    }
  }
}

#[cfg(unix)]
fn interrupt_child_after_ready(mode: &str, stage: &str) -> (std::process::ExitStatus, String) {
  use std::io::BufRead as _;
  use std::process::{Command, Stdio};

  let mut child = SignalChild(Some(
    Command::new(std::env::current_exe().unwrap())
      .args([
        "--exact",
        "remote::tests::repair_cancellation_child",
        "--nocapture",
      ])
      .env("CTL_REMOTE_REPAIR_CANCEL_CHILD", mode)
      .stdin(Stdio::null())
      .stdout(Stdio::piped())
      .stderr(Stdio::piped())
      .spawn()
      .unwrap(),
  ));
  let pid = child.0.as_ref().unwrap().id();
  let stdout = child.0.as_mut().unwrap().stdout.take().unwrap();
  let expected = format!("ctl cancellation ready: {stage}");
  let (ready, receiver) = std::sync::mpsc::channel();
  let reader = std::thread::spawn(move || {
    let mut reader = io::BufReader::new(stdout);
    let mut output = String::new();
    loop {
      let mut line = String::new();
      if reader.read_line(&mut line).unwrap() == 0 {
        break;
      }
      if line.trim().ends_with(&expected) {
        let _ = ready.send(());
      }
      output.push_str(&line);
    }
    output
  });
  receiver
    .recv_timeout(Duration::from_secs(10))
    .expect("isolated cancellation child did not reach its ready stage");
  assert!(
    Command::new("kill")
      .args(["-INT", &pid.to_string()])
      .status()
      .unwrap()
      .success()
  );
  let deadline = Instant::now() + Duration::from_secs(10);
  loop {
    if child.0.as_mut().unwrap().try_wait().unwrap().is_some() {
      break;
    }
    assert!(
      Instant::now() < deadline,
      "isolated cancellation child did not exit after SIGINT"
    );
    std::thread::sleep(Duration::from_millis(10));
  }
  let output = child.0.take().unwrap().wait_with_output().unwrap();
  let stdout = reader.join().unwrap();
  (
    output.status,
    format!("{stdout}{}", String::from_utf8_lossy(&output.stderr)),
  )
}

#[cfg(unix)]
#[test]
fn cancellation_signal_is_unarmed_before_repair_confirmation() {
  use std::os::unix::process::ExitStatusExt as _;

  let (status, output) = interrupt_child_after_ready("before", "before confirmation");
  assert_eq!(status.signal(), Some(2), "{output}");
}

#[cfg(unix)]
#[test]
fn confirmed_repair_cancels_install_and_retry_with_the_same_signal_guard() {
  for (mode, stage) in [("install", "install"), ("retry", "retry")] {
    let (status, output) = interrupt_child_after_ready(mode, stage);
    assert!(status.success(), "{output}");
  }
}

#[cfg(unix)]
struct StageDrop(Arc<AtomicBool>);

#[cfg(unix)]
impl Drop for StageDrop {
  fn drop(&mut self) {
    self.0.store(true, Ordering::Release);
  }
}

#[cfg(unix)]
#[tokio::test]
async fn repair_cancellation_child() {
  let Ok(mode) = std::env::var("CTL_REMOTE_REPAIR_CANCEL_CHILD") else {
    return;
  };
  let original = Arc::new(Recovery::default());
  let command_owner = Arc::clone(&original);
  let mut interrupt = Box::pin(original.interrupt());
  let ready = |stage: &str| {
    println!("ctl cancellation ready: {stage}");
    io::stdout().flush().unwrap();
  };
  if mode == "before" {
    // Drive the guard before its confirmation notification. SIGINT must retain
    // its default process behavior because ctrl_c has not been polled yet.
    std::future::poll_fn(|context| {
      assert!(interrupt.as_mut().poll(context).is_pending());
      std::task::Poll::Ready(())
    })
    .await;
    ready("before confirmation");
    interrupt.await.unwrap();
    panic!("the unconfirmed guard unexpectedly handled SIGINT");
  }

  // Notify through the command's clone, then poll registration before exposing
  // readiness. This prevents the parent from signaling before Tokio is armed.
  command_owner.repair_started.notify_one();
  std::future::poll_fn(|context| {
    assert!(interrupt.as_mut().poll(context).is_pending());
    std::task::Poll::Ready(())
  })
  .await;

  let attempts = Cell::new(0);
  let install_dropped = Arc::new(AtomicBool::new(false));
  let retry_dropped = Arc::new(AtomicBool::new(false));
  let mut command = Box::pin(command_owner.connect(
    || async {
      attempts.set(attempts.get() + 1);
      if attempts.get() == 1 {
        return Err(legacy_error());
      }
      let _stage = StageDrop(Arc::clone(&retry_dropped));
      ready("retry");
      std::future::pending::<Result<(), CoreError>>().await
    },
    |_| async {
      let _stage = StageDrop(Arc::clone(&install_dropped));
      ready("install");
      if mode == "install" {
        std::future::pending::<()>().await;
      }
      tokio::task::yield_now().await;
      Ok::<_, Error>(())
    },
  ));
  tokio::select! {
    result = &mut interrupt => result.unwrap(),
    _ = &mut command => panic!("the synthetic install/retry must wait for cancellation"),
  }
  drop(command);
  assert!(install_dropped.load(Ordering::Acquire));
  assert_eq!(retry_dropped.load(Ordering::Acquire), mode == "retry");
  assert_eq!(attempts.get(), if mode == "retry" { 2 } else { 1 });
}
