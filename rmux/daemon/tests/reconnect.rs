#![cfg(unix)]

use rmux_proto::{
  ClientMessage, CommandSpec, ErrorCode, LeaseKind, LeaseStatus, PROTOCOL_VERSION, ServerMessage,
  SessionInfo, TerminalSize, read_frame, write_frame,
};
use rmuxd::{DEFAULT_ATTACHMENT_LIVENESS_TIMEOUT, DaemonConfig, run};
use std::error::Error;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::sync::{Mutex, MutexGuard};
use tokio::time::{Instant, sleep, timeout};
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

// These tests all spawn a real PTY-backed shell. Serializing this integration
// layer avoids scheduling-dependent terminal startup failures while keeping
// unit tests and other crates fully parallel.
static PTY_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

async fn pty_test_lock() -> MutexGuard<'static, ()> {
  PTY_TEST_LOCK.get_or_init(|| Mutex::new(())).lock().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_survives_client_disconnect_and_resumes_from_sequence() -> TestResult {
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("rmux.sock");
  let daemon = spawn_daemon(&socket_path, 64 * 1024, 4 * 1024);
  let session = create_shell_session(
    &socket_path,
    "persistent",
    "printf 'before\\n'; IFS= read -r line; printf 'after:%s\\n' \"$line\"",
  )
  .await?;

  let (mut first_attach, first_attached) =
    attach_session(&socket_path, &session.session_id, None, true, false).await?;
  assert!(
    matches!(
      first_attached,
      ServerMessage::Attached {
        replay_from: 0,
        history_gap: false,
        ..
      },
    ),
    "expected an initial replay from zero, received {first_attached:?}"
  );
  let (first_output, resume_sequence) = read_output_until(&mut first_attach, b"before").await?;
  assert!(contains_bytes(&first_output, b"before"));
  write_frame(&mut first_attach, &ClientMessage::Detach).await?;
  drop(first_attach);

  let (mut second_attach, second_attached) = attach_session(
    &socket_path,
    &session.session_id,
    Some(resume_sequence),
    true,
    false,
  )
  .await?;
  assert!(matches!(
    second_attached,
    ServerMessage::Attached {
      replay_from,
      history_gap: false,
      ..
    } if replay_from == resume_sequence
  ));

  write_frame(
    &mut second_attach,
    &ClientMessage::Input {
      data: b"go\n".to_vec(),
    },
  )
  .await?;
  let (second_output, _) = read_output_until(&mut second_attach, b"after:go").await?;
  assert!(!contains_bytes(&second_output, b"before"));
  assert!(contains_bytes(&second_output, b"after:go"));

  loop {
    if matches!(
      required_message(&mut second_attach).await?,
      ServerMessage::SessionEnded { .. }
    ) {
      break;
    }
  }
  drop(second_attach);

  let daemon_result = timeout(Duration::from_secs(3), daemon)
    .await
    .map_err(|_| "rmuxd did not exit after its final session ended")?;
  daemon_result??;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkpoint_restores_terminal_state_after_journal_compaction() -> TestResult {
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("rmux.sock");
  let daemon = spawn_daemon(&socket_path, 32, 1);
  let session = create_shell_session(
    &socket_path,
    "checkpoint",
    "printf 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'; printf '\\033[2J\\033[Hcheckpoint-ready'; IFS= read -r line",
  )
  .await?;

  let (mut first_attach, first_attached) =
    attach_session(&socket_path, &session.session_id, Some(0), false, false).await?;
  assert!(matches!(first_attached, ServerMessage::Attached { .. }));
  let (initial_output, _) = read_output_until(&mut first_attach, b"checkpoint-ready").await?;
  assert!(contains_bytes(&initial_output, b"checkpoint-ready"));
  write_frame(&mut first_attach, &ClientMessage::Detach).await?;
  drop(first_attach);

  let (mut restored_attach, attached) =
    attach_session(&socket_path, &session.session_id, Some(0), true, false).await?;
  let ServerMessage::Attached {
    checkpoint: Some(checkpoint),
    history_gap: true,
    terminal_size_mismatch: false,
    ..
  } = attached
  else {
    return Err(format!("expected checkpoint-backed attach, received {attached:?}").into());
  };
  assert!(checkpoint.is_supported());

  let mut restored_terminal = avt::Vt::new(80, 24);
  restored_terminal.feed_str(&String::from_utf8(checkpoint.payload)?);
  assert!(
    restored_terminal
      .text()
      .join("\n")
      .contains("checkpoint-ready")
  );

  write_frame(
    &mut restored_attach,
    &ClientMessage::Input {
      data: b"go\n".to_vec(),
    },
  )
  .await?;
  loop {
    if matches!(
      required_message(&mut restored_attach).await?,
      ServerMessage::SessionEnded { .. }
    ) {
      break;
    }
  }
  drop(restored_attach);

  let daemon_result = timeout(Duration::from_secs(3), daemon)
    .await
    .map_err(|_| "rmuxd did not exit after checkpoint test")?;
  daemon_result??;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn secondary_attachment_cannot_control_owned_session_but_receives_authorized_output()
-> TestResult {
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("rmux.sock");
  let daemon = spawn_daemon(&socket_path, 64 * 1024, 4 * 1024);
  let session = create_shell_session(
    &socket_path,
    "owned",
    "printf 'ready\\n'; IFS= read -r first; printf 'authorized:%s\\n' \"$first\"; IFS= read -r second; printf 'authorized:%s\\n' \"$second\"",
  )
  .await?;

  let desktop_size = TerminalSize::default();
  let phone_size = terminal_size(40, 10);
  let (mut first_attach, first_attached) = attach_session_with_options(
    &socket_path,
    &session.session_id,
    None,
    desktop_size.clone(),
    true,
    true,
  )
  .await?;
  let ServerMessage::Attached {
    input_lease,
    layout_lease,
    ..
  } = first_attached
  else {
    return Err(format!("expected first attachment, received {first_attached:?}").into());
  };
  assert_lease_status(&input_lease, true, true);
  assert_lease_status(&layout_lease, true, true);

  let (mut second_attach, second_attached) = attach_session_with_options(
    &socket_path,
    &session.session_id,
    None,
    phone_size.clone(),
    true,
    true,
  )
  .await?;
  let ServerMessage::Attached {
    terminal_size_mismatch,
    input_lease,
    layout_lease,
    ..
  } = second_attached
  else {
    return Err(format!("expected second attachment, received {second_attached:?}").into());
  };
  assert!(terminal_size_mismatch);
  assert_lease_status(&input_lease, true, false);
  assert_lease_status(&layout_lease, true, false);

  write_frame(
    &mut first_attach,
    &ClientMessage::Input {
      data: b"from-first\n".to_vec(),
    },
  )
  .await?;
  let (first_output, _) = read_output_until(&mut first_attach, b"authorized:from-first").await?;
  let (second_output, _) = read_output_until(&mut second_attach, b"authorized:from-first").await?;
  assert!(contains_bytes(&first_output, b"authorized:from-first"));
  assert!(contains_bytes(&second_output, b"authorized:from-first"));

  write_frame(
    &mut second_attach,
    &ClientMessage::Input {
      data: b"should-not-arrive\n".to_vec(),
    },
  )
  .await?;
  expect_error(&mut second_attach, ErrorCode::InputLeaseRequired).await?;

  write_frame(
    &mut second_attach,
    &ClientMessage::Resize {
      terminal_size: phone_size,
    },
  )
  .await?;
  expect_error(&mut second_attach, ErrorCode::LayoutLeaseRequired).await?;
  assert_eq!(
    session_info(&socket_path, &session.session_id)
      .await?
      .terminal_size,
    desktop_size
  );

  write_frame(
    &mut first_attach,
    &ClientMessage::Input {
      data: b"finish\n".to_vec(),
    },
  )
  .await?;
  wait_for_session_end(&mut first_attach).await?;
  wait_for_session_end(&mut second_attach).await?;
  drop(first_attach);
  drop(second_attach);

  wait_for_daemon_exit(daemon, "rmuxd did not exit after ownership test").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicitly_released_leases_can_be_acquired_by_another_attachment() -> TestResult {
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("rmux.sock");
  let daemon = spawn_daemon(&socket_path, 64 * 1024, 4 * 1024);
  let session = create_shell_session(
    &socket_path,
    "released",
    "printf 'ready\\n'; IFS= read -r line; printf 'authorized:%s\\n' \"$line\"",
  )
  .await?;

  let (mut first_attach, first_attached) =
    attach_session(&socket_path, &session.session_id, None, true, true).await?;
  let ServerMessage::Attached {
    input_lease,
    layout_lease,
    ..
  } = first_attached
  else {
    return Err(format!("expected first attachment, received {first_attached:?}").into());
  };
  assert_lease_status(&input_lease, true, true);
  assert_lease_status(&layout_lease, true, true);

  let (mut second_attach, second_attached) =
    attach_session(&socket_path, &session.session_id, None, false, false).await?;
  let ServerMessage::Attached {
    input_lease,
    layout_lease,
    ..
  } = second_attached
  else {
    return Err(format!("expected second attachment, received {second_attached:?}").into());
  };
  assert_lease_status(&input_lease, true, false);
  assert_lease_status(&layout_lease, true, false);

  let released_input = release_lease(&mut first_attach, LeaseKind::Input).await?;
  assert_lease_status(&released_input, false, false);
  let acquired_input = acquire_lease(&mut second_attach, LeaseKind::Input).await?;
  assert_lease_status(&acquired_input, true, true);

  let released_layout = release_lease(&mut first_attach, LeaseKind::Layout).await?;
  assert_lease_status(&released_layout, false, false);
  let acquired_layout = acquire_lease(&mut second_attach, LeaseKind::Layout).await?;
  assert_lease_status(&acquired_layout, true, true);

  let new_size = terminal_size(100, 40);
  write_frame(
    &mut second_attach,
    &ClientMessage::Resize {
      terminal_size: new_size.clone(),
    },
  )
  .await?;
  // `Resize` has no response of its own. The ordered heartbeat acknowledgement
  // proves that rmuxd processed the preceding resize before observing session
  // state from a separate connection.
  heartbeat(&mut second_attach, 1).await?;
  assert_eq!(
    session_info(&socket_path, &session.session_id)
      .await?
      .terminal_size,
    new_size
  );

  write_frame(
    &mut second_attach,
    &ClientMessage::Input {
      data: b"from-second\n".to_vec(),
    },
  )
  .await?;
  let (first_output, _) = read_output_until(&mut first_attach, b"authorized:from-second").await?;
  let (second_output, _) = read_output_until(&mut second_attach, b"authorized:from-second").await?;
  assert!(contains_bytes(&first_output, b"authorized:from-second"));
  assert!(contains_bytes(&second_output, b"authorized:from-second"));

  wait_for_session_end(&mut first_attach).await?;
  wait_for_session_end(&mut second_attach).await?;
  drop(first_attach);
  drop(second_attach);

  wait_for_daemon_exit(daemon, "rmuxd did not exit after release test").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnected_attachment_releases_its_leases_for_another_attachment() -> TestResult {
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("rmux.sock");
  let daemon = spawn_daemon(&socket_path, 64 * 1024, 4 * 1024);
  let session = create_shell_session(
    &socket_path,
    "disconnected",
    "printf 'ready\\n'; IFS= read -r line; printf 'authorized:%s\\n' \"$line\"",
  )
  .await?;

  let (first_attach, first_attached) =
    attach_session(&socket_path, &session.session_id, None, true, true).await?;
  let ServerMessage::Attached {
    input_lease,
    layout_lease,
    ..
  } = first_attached
  else {
    return Err(format!("expected first attachment, received {first_attached:?}").into());
  };
  assert_lease_status(&input_lease, true, true);
  assert_lease_status(&layout_lease, true, true);

  let (mut second_attach, second_attached) =
    attach_session(&socket_path, &session.session_id, None, false, false).await?;
  let ServerMessage::Attached {
    input_lease,
    layout_lease,
    ..
  } = second_attached
  else {
    return Err(format!("expected second attachment, received {second_attached:?}").into());
  };
  assert_lease_status(&input_lease, true, false);
  assert_lease_status(&layout_lease, true, false);

  drop(first_attach);

  let input_status = acquire_lease_until_owned(&mut second_attach, LeaseKind::Input).await?;
  assert_lease_status(&input_status, true, true);
  let layout_status = acquire_lease_until_owned(&mut second_attach, LeaseKind::Layout).await?;
  assert_lease_status(&layout_status, true, true);

  write_frame(
    &mut second_attach,
    &ClientMessage::Input {
      data: b"after-disconnect\n".to_vec(),
    },
  )
  .await?;
  let (output, _) = read_output_until(&mut second_attach, b"authorized:after-disconnect").await?;
  assert!(contains_bytes(&output, b"authorized:after-disconnect"));
  wait_for_session_end(&mut second_attach).await?;
  drop(second_attach);

  wait_for_daemon_exit(daemon, "rmuxd did not exit after disconnect test").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn silent_open_attachment_expires_and_cannot_renew_its_leases_late() -> TestResult {
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("rmux.sock");
  let liveness_timeout = Duration::from_millis(200);
  let daemon = spawn_daemon_with_liveness(&socket_path, 64 * 1024, 4 * 1024, liveness_timeout);
  let session = create_shell_session(
    &socket_path,
    "silent-owner",
    "printf 'ready\\n'; IFS= read -r line; printf 'authorized:%s\\n' \"$line\"",
  )
  .await?;

  let (mut stale_owner, stale_attached) =
    attach_session(&socket_path, &session.session_id, None, true, true).await?;
  let ServerMessage::Attached {
    input_lease,
    layout_lease,
    ..
  } = stale_attached
  else {
    return Err(format!("expected stale attachment, received {stale_attached:?}").into());
  };
  assert_lease_status(&input_lease, true, true);
  assert_lease_status(&layout_lease, true, true);

  let (mut contender, contender_attached) =
    attach_session(&socket_path, &session.session_id, None, true, true).await?;
  let ServerMessage::Attached {
    input_lease,
    layout_lease,
    ..
  } = contender_attached
  else {
    return Err(format!("expected contender attachment, received {contender_attached:?}").into());
  };
  assert_lease_status(&input_lease, true, false);
  assert_lease_status(&layout_lease, true, false);

  for nonce in 1..=8 {
    heartbeat(&mut contender, nonce).await?;
    sleep(Duration::from_millis(50)).await;
  }

  // The stale stream remains physically open from this process's point of
  // view. These post-expiry frames must not extend its deadline or preserve
  // its leases if the server races the timeout with a readable socket.
  let _late_heartbeat =
    write_frame(&mut stale_owner, &ClientMessage::Heartbeat { nonce: 1_000 }).await;
  let _late_acquire = write_frame(
    &mut stale_owner,
    &ClientMessage::AcquireLease {
      lease: LeaseKind::Input,
    },
  )
  .await;

  heartbeat(&mut contender, 2_000).await?;
  let input_status = acquire_lease(&mut contender, LeaseKind::Input).await?;
  assert_lease_status(&input_status, true, true);
  let layout_status = acquire_lease(&mut contender, LeaseKind::Layout).await?;
  assert_lease_status(&layout_status, true, true);

  write_frame(
    &mut contender,
    &ClientMessage::Input {
      data: b"after-expiry\n".to_vec(),
    },
  )
  .await?;
  let (output, _) = read_output_until(&mut contender, b"authorized:after-expiry").await?;
  assert!(contains_bytes(&output, b"authorized:after-expiry"));
  wait_for_session_end(&mut contender).await?;
  drop(stale_owner);
  drop(contender);

  wait_for_daemon_exit(daemon, "rmuxd did not exit after silent owner expiry").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn healthy_heartbeating_attachment_retains_its_leases() -> TestResult {
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("rmux.sock");
  let liveness_timeout = Duration::from_millis(200);
  let daemon = spawn_daemon_with_liveness(&socket_path, 64 * 1024, 4 * 1024, liveness_timeout);
  let session = create_shell_session(
    &socket_path,
    "healthy-owner",
    "printf 'ready\\n'; IFS= read -r line; printf 'authorized:%s\\n' \"$line\"",
  )
  .await?;

  let (mut owner, owner_attached) =
    attach_session(&socket_path, &session.session_id, None, true, true).await?;
  let ServerMessage::Attached {
    input_lease,
    layout_lease,
    ..
  } = owner_attached
  else {
    return Err(format!("expected owner attachment, received {owner_attached:?}").into());
  };
  assert_lease_status(&input_lease, true, true);
  assert_lease_status(&layout_lease, true, true);

  let (mut contender, contender_attached) =
    attach_session(&socket_path, &session.session_id, None, true, true).await?;
  let ServerMessage::Attached {
    input_lease,
    layout_lease,
    ..
  } = contender_attached
  else {
    return Err(format!("expected contender attachment, received {contender_attached:?}").into());
  };
  assert_lease_status(&input_lease, true, false);
  assert_lease_status(&layout_lease, true, false);

  for nonce in 1..=8 {
    heartbeat(&mut owner, nonce).await?;
    heartbeat(&mut contender, nonce + 100).await?;
    sleep(Duration::from_millis(50)).await;
  }

  let input_status = acquire_lease(&mut contender, LeaseKind::Input).await?;
  assert_lease_status(&input_status, true, false);
  let layout_status = acquire_lease(&mut contender, LeaseKind::Layout).await?;
  assert_lease_status(&layout_status, true, false);

  write_frame(
    &mut owner,
    &ClientMessage::Input {
      data: b"finish\n".to_vec(),
    },
  )
  .await?;
  wait_for_session_end(&mut owner).await?;
  wait_for_session_end(&mut contender).await?;
  drop(owner);
  drop(contender);

  wait_for_daemon_exit(daemon, "rmuxd did not exit after healthy owner test").await
}

fn spawn_daemon(
  socket_path: &Path,
  journal_capacity_bytes: usize,
  checkpoint_interval_bytes: usize,
) -> tokio::task::JoinHandle<Result<(), rmuxd::DaemonError>> {
  spawn_daemon_with_liveness(
    socket_path,
    journal_capacity_bytes,
    checkpoint_interval_bytes,
    DEFAULT_ATTACHMENT_LIVENESS_TIMEOUT,
  )
}

fn spawn_daemon_with_liveness(
  socket_path: &Path,
  journal_capacity_bytes: usize,
  checkpoint_interval_bytes: usize,
  attachment_liveness_timeout: Duration,
) -> tokio::task::JoinHandle<Result<(), rmuxd::DaemonError>> {
  let socket_path = socket_path.to_path_buf();
  tokio::spawn(async move {
    run(DaemonConfig {
      socket_path,
      journal_capacity_bytes,
      checkpoint_interval_bytes,
      startup_idle_timeout: Duration::from_secs(5),
      attachment_liveness_timeout,
    })
    .await
  })
}

async fn create_shell_session(
  socket_path: &Path,
  name: &str,
  script: &str,
) -> TestResult<rmux_proto::SessionInfo> {
  let mut create = connect_when_ready(socket_path).await?;
  handshake(&mut create).await?;
  write_frame(
    &mut create,
    &ClientMessage::CreateSession {
      name: Some(name.into()),
      command: Some(CommandSpec {
        program: "/bin/sh".into(),
        arguments: vec!["-c".into(), script.into()],
      }),
      working_directory: None,
      terminal_size: TerminalSize::default(),
    },
  )
  .await?;
  let created = required_message(&mut create).await?;
  let ServerMessage::SessionCreated { session } = created else {
    return Err(format!("expected session_created, received {created:?}").into());
  };
  Ok(session)
}

async fn attach_session(
  socket_path: &Path,
  session: &str,
  resume_from: Option<u64>,
  request_input_lease: bool,
  request_layout_lease: bool,
) -> TestResult<(UnixStream, ServerMessage)> {
  attach_session_with_options(
    socket_path,
    session,
    resume_from,
    TerminalSize::default(),
    request_input_lease,
    request_layout_lease,
  )
  .await
}

async fn attach_session_with_options(
  socket_path: &Path,
  session: &str,
  resume_from: Option<u64>,
  terminal_size: TerminalSize,
  request_input_lease: bool,
  request_layout_lease: bool,
) -> TestResult<(UnixStream, ServerMessage)> {
  let mut stream = connect_when_ready(socket_path).await?;
  handshake(&mut stream).await?;
  write_frame(
    &mut stream,
    &ClientMessage::AttachSession {
      session: session.into(),
      resume_from,
      terminal_size,
      request_input_lease,
      request_layout_lease,
    },
  )
  .await?;
  let attached = required_message(&mut stream).await?;
  Ok((stream, attached))
}

async fn session_info(socket_path: &Path, session_id: &str) -> TestResult<SessionInfo> {
  let mut stream = connect_when_ready(socket_path).await?;
  handshake(&mut stream).await?;
  write_frame(&mut stream, &ClientMessage::ListSessions).await?;
  let response = required_message(&mut stream).await?;
  let ServerMessage::SessionList { sessions } = response else {
    return Err(format!("expected session list, received {response:?}").into());
  };
  sessions
    .into_iter()
    .find(|candidate| candidate.session_id == session_id)
    .ok_or_else(|| format!("session '{session_id}' was absent from session list").into())
}

async fn acquire_lease(stream: &mut UnixStream, lease: LeaseKind) -> TestResult<LeaseStatus> {
  write_frame(stream, &ClientMessage::AcquireLease { lease }).await?;
  lease_status_response(stream, lease).await
}

async fn release_lease(stream: &mut UnixStream, lease: LeaseKind) -> TestResult<LeaseStatus> {
  write_frame(stream, &ClientMessage::ReleaseLease { lease }).await?;
  lease_status_response(stream, lease).await
}

async fn heartbeat(stream: &mut UnixStream, nonce: u64) -> TestResult {
  write_frame(stream, &ClientMessage::Heartbeat { nonce }).await?;
  loop {
    match required_message(stream).await? {
      ServerMessage::HeartbeatAck {
        nonce: acknowledged,
      } => {
        assert_eq!(acknowledged, nonce);
        return Ok(());
      }
      ServerMessage::Output { .. } | ServerMessage::Checkpoint { .. } => {}
      response => {
        return Err(format!("expected heartbeat acknowledgement, received {response:?}").into());
      }
    }
  }
}

async fn lease_status_response(
  stream: &mut UnixStream,
  expected_lease: LeaseKind,
) -> TestResult<LeaseStatus> {
  loop {
    match required_message(stream).await? {
      ServerMessage::LeaseStatus { lease, status } => {
        assert_eq!(lease, expected_lease);
        return Ok(status);
      }
      ServerMessage::Output { .. } | ServerMessage::Checkpoint { .. } => {}
      response => return Err(format!("expected lease status, received {response:?}").into()),
    }
  }
}

async fn acquire_lease_until_owned(
  stream: &mut UnixStream,
  lease: LeaseKind,
) -> TestResult<LeaseStatus> {
  let deadline = Instant::now() + Duration::from_secs(3);
  loop {
    let status = acquire_lease(stream, lease).await?;
    if status.owned_by_client {
      return Ok(status);
    }
    if Instant::now() >= deadline {
      return Err(format!("{lease:?} lease was not released after disconnect").into());
    }
    sleep(Duration::from_millis(10)).await;
  }
}

async fn expect_error(stream: &mut UnixStream, expected_code: ErrorCode) -> TestResult {
  loop {
    match required_message(stream).await? {
      ServerMessage::Error { code, .. } => {
        assert_eq!(code, expected_code);
        return Ok(());
      }
      ServerMessage::Output { .. } | ServerMessage::Checkpoint { .. } => {}
      response => return Err(format!("expected error response, received {response:?}").into()),
    }
  }
}

async fn wait_for_session_end(stream: &mut UnixStream) -> TestResult {
  loop {
    match required_message(stream).await? {
      ServerMessage::SessionEnded { .. } => return Ok(()),
      ServerMessage::Output { .. } => {}
      response => {
        return Err(format!("expected output or session end, received {response:?}").into());
      }
    }
  }
}

async fn wait_for_daemon_exit(
  daemon: tokio::task::JoinHandle<Result<(), rmuxd::DaemonError>>,
  timeout_message: &str,
) -> TestResult {
  let daemon_result = timeout(Duration::from_secs(3), daemon)
    .await
    .map_err(|_| timeout_message)?;
  daemon_result??;
  Ok(())
}

fn assert_lease_status(status: &LeaseStatus, held: bool, owned_by_client: bool) {
  assert_eq!(status.held, held);
  assert_eq!(status.owned_by_client, owned_by_client);
}

fn terminal_size(columns: u16, rows: u16) -> TerminalSize {
  TerminalSize {
    columns,
    rows,
    pixel_width: 0,
    pixel_height: 0,
  }
}

async fn connect_when_ready(socket_path: &Path) -> TestResult<UnixStream> {
  let deadline = Instant::now() + Duration::from_secs(3);
  loop {
    match UnixStream::connect(socket_path).await {
      Ok(stream) => return Ok(stream),
      Err(_) if Instant::now() < deadline => {
        sleep(Duration::from_millis(10)).await;
      }
      Err(error) => return Err(error.into()),
    }
  }
}

async fn handshake(stream: &mut UnixStream) -> TestResult {
  write_frame(
    stream,
    &ClientMessage::Handshake {
      protocol_version: PROTOCOL_VERSION,
      client_name: "integration-test".into(),
      client_version: env!("CARGO_PKG_VERSION").into(),
    },
  )
  .await?;
  assert!(matches!(
    required_message(stream).await?,
    ServerMessage::HandshakeAccepted {
      protocol_version: PROTOCOL_VERSION,
      ..
    }
  ));
  Ok(())
}

async fn required_message(stream: &mut UnixStream) -> TestResult<ServerMessage> {
  timeout(Duration::from_secs(3), read_frame(stream))
    .await
    .map_err(|_| "timed out waiting for rmuxd")??
    .ok_or_else(|| "rmuxd closed the connection unexpectedly".into())
}

async fn read_output_until(stream: &mut UnixStream, expected: &[u8]) -> TestResult<(Vec<u8>, u64)> {
  let mut output = Vec::new();
  loop {
    match required_message(stream).await? {
      ServerMessage::Output {
        sequence_end, data, ..
      } => {
        output.extend(data);
        if contains_bytes(&output, expected) {
          return Ok((output, sequence_end));
        }
      }
      message => return Err(format!("expected output, received {message:?}").into()),
    }
  }
}

struct TestDirectory {
  path: std::path::PathBuf,
}

impl TestDirectory {
  fn new() -> Self {
    Self {
      path: std::path::PathBuf::from("/tmp").join(format!(
        "rmux-t-{}",
        &Uuid::new_v4().simple().to_string()[..8]
      )),
    }
  }
}

impl Drop for TestDirectory {
  fn drop(&mut self) {
    let _ignored = std::fs::remove_dir_all(&self.path);
  }
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
  haystack
    .windows(needle.len())
    .any(|candidate| candidate == needle)
}
