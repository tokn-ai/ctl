#![cfg(unix)]

use rmux_ipc::{control_socket_path, request_local_daemon_restart};
use rmux_proto::{
  ClientMessage, CommandSpec, ErrorCode, LeaseKind, LeaseStatus, PROTOCOL_VERSION, ServerMessage,
  SessionInfo, ShellState, TerminalSize, read_frame, write_frame,
};
use rmuxd::{DEFAULT_ATTACHMENT_LIVENESS_TIMEOUT, DaemonConfig, run};
use std::error::Error;
use std::os::unix::fs::PermissionsExt as _;
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
  wait_for_detached(&mut first_attach).await?;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unnamed_sessions_get_monotonic_names_without_colliding_with_explicit_names() -> TestResult
{
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("rmux.sock");
  let daemon = spawn_daemon(&socket_path, 64 * 1024, 4 * 1024);
  let explicit_one = create_shell_session(&socket_path, "session-1", "IFS= read -r line").await?;
  let explicit_three = create_shell_session(&socket_path, "session-3", "IFS= read -r line").await?;

  let (automatic_a, automatic_b) = tokio::join!(
    create_shell_session_with_name(&socket_path, None, "IFS= read -r line"),
    create_shell_session_with_name(&socket_path, None, "IFS= read -r line"),
  );
  let automatic_a = automatic_a?;
  let automatic_b = automatic_b?;
  let mut concurrent_names = [automatic_a.name.as_str(), automatic_b.name.as_str()];
  concurrent_names.sort_unstable();
  assert_eq!(concurrent_names, ["session-2", "session-4"]);

  let automatic_c = create_shell_session_with_name(&socket_path, None, "IFS= read -r line").await?;
  assert_eq!(automatic_c.name, "session-5");

  for session in [
    &explicit_one,
    &explicit_three,
    &automatic_a,
    &automatic_b,
    &automatic_c,
  ] {
    kill_shell_session(&socket_path, &session.session_id).await?;
  }

  wait_for_daemon_exit(daemon, "rmuxd did not exit after automatic naming test").await
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
  wait_for_detached(&mut first_attach).await?;
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
async fn shell_awareness_uses_private_reports_and_redacts_viewer_command_text() -> TestResult {
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("rmux.sock");
  let daemon = spawn_daemon(&socket_path, 64 * 1024, 4 * 1024);
  let session = create_shell_session(
    &socket_path,
    "shell-state",
    "printf 'rmux-shell-v1\\000zsh\\0001\\000cwd,command_line,cursor,prompt_phase\\000/workspace/rmux\\000editing\\0001\\000echo 日\\0006\\000' > \"$RMUX_SHELL_STATE_PIPE\"; IFS= read -r line",
  )
  .await?;

  let inspection = wait_for_shell_state(&socket_path, &session.session_id).await?;
  assert_eq!(inspection.shell.shell_type, rmux_proto::ShellType::Zsh);
  assert_eq!(inspection.cwd.as_deref(), Some("/workspace/rmux"));
  assert_eq!(inspection.prompt_phase, rmux_proto::PromptPhase::Editing);
  assert!(inspection.command_line_redacted);
  assert_eq!(inspection.current_command_line, None);

  let (mut owner, owner_attached) =
    attach_session_with_command_line_request(&socket_path, &session.session_id, true, true).await?;
  let ServerMessage::Attached {
    shell_state: owner_state,
    input_lease,
    ..
  } = owner_attached
  else {
    return Err(format!("expected owner attachment, received {owner_attached:?}").into());
  };
  assert!(input_lease.owned_by_client);
  assert!(!owner_state.command_line_redacted);
  assert_eq!(
    owner_state
      .current_command_line
      .as_ref()
      .map(|line| line.text.as_str()),
    Some("echo 日")
  );
  assert_eq!(
    owner_state
      .current_command_line
      .as_ref()
      .and_then(|line| line.cursor_scalar_offset),
    Some(6)
  );

  let (mut viewer, viewer_attached) =
    attach_session_with_command_line_request(&socket_path, &session.session_id, false, true)
      .await?;
  let ServerMessage::Attached {
    shell_state: viewer_state,
    input_lease,
    ..
  } = viewer_attached
  else {
    return Err(format!("expected viewer attachment, received {viewer_attached:?}").into());
  };
  assert!(!input_lease.owned_by_client);
  assert!(viewer_state.command_line_redacted);
  assert_eq!(viewer_state.current_command_line, None);

  let released_input = release_lease(&mut owner, LeaseKind::Input).await?;
  assert_lease_status(&released_input, false, false);
  let acquired_input = acquire_lease(&mut viewer, LeaseKind::Input).await?;
  assert_lease_status(&acquired_input, true, true);
  let upgraded_viewer_state =
    wait_for_unredacted_command_line(&mut viewer, viewer_state.revision).await?;
  assert!(upgraded_viewer_state.revision > viewer_state.revision);
  assert!(!upgraded_viewer_state.command_line_redacted);
  assert_eq!(
    upgraded_viewer_state
      .current_command_line
      .as_ref()
      .map(|line| line.text.as_str()),
    Some("echo 日")
  );

  write_frame(
    &mut viewer,
    &ClientMessage::Input {
      data: b"finish\n".to_vec(),
    },
  )
  .await?;

  wait_for_session_end(&mut owner).await?;
  wait_for_session_end(&mut viewer).await?;
  drop(owner);
  drop(viewer);
  wait_for_daemon_exit(daemon, "rmuxd did not exit after shell-awareness test").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn running_command_summaries_follow_input_lease_visibility() -> TestResult {
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("rmux.sock");
  let daemon = spawn_daemon(&socket_path, 64 * 1024, 4 * 1024);
  let session = create_shell_session(
    &socket_path,
    "running-command-state",
    "printf 'rmux-shell-v2\\000zsh\\0002\\000cwd,prompt_phase,running_command\\000/workspace/rmux\\000running\\0001\\000cargo test --workspace\\000\\000' > \"$RMUX_SHELL_STATE_PIPE\"; IFS= read -r line",
  )
  .await?;

  let inspection = wait_for_shell_state(&socket_path, &session.session_id).await?;
  assert_eq!(inspection.prompt_phase, rmux_proto::PromptPhase::Running);
  assert!(inspection.running_command_redacted);
  assert_eq!(inspection.running_command, None);

  let (mut viewer, attached) =
    attach_session_with_running_command_request(&socket_path, &session.session_id, false).await?;
  let ServerMessage::Attached {
    shell_state: viewer_state,
    input_lease,
    ..
  } = attached
  else {
    return Err(format!("expected viewer attachment, received {attached:?}").into());
  };
  assert!(!input_lease.held);
  assert!(!input_lease.owned_by_client);
  assert!(viewer_state.running_command_redacted);
  assert_eq!(viewer_state.running_command, None);

  let acquired_input = acquire_lease(&mut viewer, LeaseKind::Input).await?;
  assert_lease_status(&acquired_input, true, true);
  let visible_state = wait_for_visible_running_command(&mut viewer, viewer_state.revision).await?;
  assert!(visible_state.revision > viewer_state.revision);
  assert!(!visible_state.running_command_redacted);
  assert_eq!(
    visible_state.running_command.as_deref(),
    Some("cargo test --workspace")
  );

  let released_input = release_lease(&mut viewer, LeaseKind::Input).await?;
  assert_lease_status(&released_input, false, false);
  let redacted_state =
    wait_for_redacted_running_command(&mut viewer, visible_state.revision).await?;
  assert!(redacted_state.revision > visible_state.revision);
  assert!(redacted_state.running_command_redacted);
  assert_eq!(redacted_state.running_command, None);

  kill_shell_session(&socket_path, &session.session_id).await?;
  wait_for_session_end(&mut viewer).await?;
  drop(viewer);
  wait_for_daemon_exit(
    daemon,
    "rmuxd did not exit after running-command visibility test",
  )
  .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn alternate_screen_transitions_publish_a_tui_hint() -> TestResult {
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("rmux.sock");
  let daemon = spawn_daemon(&socket_path, 64 * 1024, 4 * 1024);
  let session = create_shell_session(
    &socket_path,
    "alternate-screen",
    "sleep 1; printf '\\033[?1049h'; sleep 1; printf '\\033[?1049l'; sleep 1",
  )
  .await?;

  let (mut attachment, attached) =
    attach_session(&socket_path, &session.session_id, None, false, false).await?;
  let ServerMessage::Attached { shell_state, .. } = attached else {
    return Err(format!("expected attachment, received {attached:?}").into());
  };
  assert_eq!(shell_state.tui_hint, rmux_proto::TuiHint::Unknown);

  wait_for_tui_hint(&mut attachment, rmux_proto::TuiHint::AlternateScreen).await?;
  wait_for_tui_hint(&mut attachment, rmux_proto::TuiHint::Inline).await?;
  wait_for_session_end(&mut attachment).await?;
  drop(attachment);
  wait_for_daemon_exit(daemon, "rmuxd did not exit after alternate-screen test").await
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
async fn geometry_changes_are_ordered_for_viewers_and_checkpointed_on_resume() -> TestResult {
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("rmux.sock");
  let daemon = spawn_daemon(&socket_path, 64 * 1024, 4 * 1024);
  let session = create_shell_session(
    &socket_path,
    "geometry",
    "printf 'before-resize\\n'; IFS= read -r first; printf 'after-resize:%s\\n' \"$first\"; IFS= read -r second",
  )
  .await?;

  let (mut owner, owner_attached) =
    attach_session(&socket_path, &session.session_id, None, true, true).await?;
  assert!(matches!(owner_attached, ServerMessage::Attached { .. }));
  let (_, before_resize_sequence) = read_output_until(&mut owner, b"before-resize").await?;
  assert!(before_resize_sequence > 0);

  let (mut viewer, viewer_attached) = attach_session(
    &socket_path,
    &session.session_id,
    Some(before_resize_sequence),
    false,
    false,
  )
  .await?;
  assert!(matches!(viewer_attached, ServerMessage::Attached { .. }));

  let resized = terminal_size(120, 36);
  write_frame(
    &mut owner,
    &ClientMessage::Resize {
      terminal_size: resized.clone(),
    },
  )
  .await?;
  let owner_boundary = wait_for_geometry_change(&mut owner, &resized).await?;
  let viewer_boundary = wait_for_geometry_change(&mut viewer, &resized).await?;
  assert_eq!(owner_boundary, before_resize_sequence);
  assert_eq!(viewer_boundary, before_resize_sequence);

  assert_geometry_checkpoint_resume(&socket_path, &session.session_id, viewer_boundary, &resized)
    .await?;

  write_frame(
    &mut owner,
    &ClientMessage::Input {
      data: b"go\n".to_vec(),
    },
  )
  .await?;
  let (viewer_output, first_viewer_sequence) =
    read_output_until_with_first_sequence(&mut viewer, b"after-resize:go").await?;
  assert!(contains_bytes(&viewer_output, b"after-resize:go"));
  assert_eq!(first_viewer_sequence, viewer_boundary);

  write_frame(
    &mut owner,
    &ClientMessage::Input {
      data: b"finish\n".to_vec(),
    },
  )
  .await?;
  wait_for_session_end(&mut owner).await?;
  wait_for_session_end(&mut viewer).await?;
  drop(owner);
  drop(viewer);

  wait_for_daemon_exit(daemon, "rmuxd did not exit after geometry test").await
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
  let daemon = spawn_daemon_with_liveness(
    &socket_path,
    64 * 1024,
    4 * 1024,
    Duration::from_millis(200),
  );
  let session = create_shell_session(
    &socket_path,
    "disconnected",
    "printf 'ready\\n'; IFS= read -r line; printf 'authorized:%s\\n' \"$line\"",
  )
  .await?;

  let (first_attach, first_attached) =
    attach_session(&socket_path, &session.session_id, None, true, true).await?;
  let ServerMessage::Attached {
    attachment_token,
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

  let (expired_resume, expired_response) =
    resume_attachment(&socket_path, &session.session_id, &attachment_token, None).await?;
  assert!(matches!(
    expired_response,
    ServerMessage::Error {
      code: ErrorCode::AttachmentResumeRejected,
      ..
    }
  ));
  drop(expired_resume);

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
async fn reconnect_token_rebinds_the_attachment_and_preserves_both_leases() -> TestResult {
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("rmux.sock");
  let daemon = spawn_daemon_with_liveness(
    &socket_path,
    64 * 1024,
    4 * 1024,
    Duration::from_millis(500),
  );
  let session = create_shell_session(
    &socket_path,
    "token-resume",
    "printf 'ready\\n'; IFS= read -r line; printf 'authorized:%s\\n' \"$line\"",
  )
  .await?;

  let (mut stale_attachment, attached) =
    attach_session(&socket_path, &session.session_id, None, true, true).await?;
  let ServerMessage::Attached {
    attachment_token,
    input_lease,
    layout_lease,
    ..
  } = attached
  else {
    return Err(format!("expected initial attachment, received {attached:?}").into());
  };
  assert_lease_status(&input_lease, true, true);
  assert_lease_status(&layout_lease, true, true);

  // Rebind while the old transport is still physically open. Possession of
  // the token supersedes that stale generation without waiting for liveness
  // expiry and without exposing either lease to a contender.
  let (mut resumed, resumed_response) =
    resume_attachment(&socket_path, &session.session_id, &attachment_token, None).await?;
  let ServerMessage::Attached {
    attachment_token: resumed_token,
    input_lease,
    layout_lease,
    ..
  } = resumed_response
  else {
    return Err(format!("expected resumed attachment, received {resumed_response:?}").into());
  };
  assert_eq!(resumed_token, attachment_token);
  assert_lease_status(&input_lease, true, true);
  assert_lease_status(&layout_lease, true, true);

  timeout(Duration::from_secs(1), async {
    loop {
      if read_frame::<_, ServerMessage>(&mut stale_attachment)
        .await?
        .is_none()
      {
        return Ok::<(), rmux_proto::CodecError>(());
      }
    }
  })
  .await
  .map_err(|_| "superseded attachment did not close")??;

  write_frame(
    &mut resumed,
    &ClientMessage::Input {
      data: b"after-resume\n".to_vec(),
    },
  )
  .await?;
  let (output, _) = read_output_until(&mut resumed, b"authorized:after-resume").await?;
  assert!(contains_bytes(&output, b"authorized:after-resume"));
  wait_for_session_end(&mut resumed).await?;
  drop(resumed);

  wait_for_daemon_exit(daemon, "rmuxd did not exit after token-resume test").await
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_control_restart_ends_sessions_and_closes_existing_attachments() -> TestResult {
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("rmux.sock");
  let control_path = control_socket_path(&socket_path)?;
  let daemon = spawn_daemon(&socket_path, 64 * 1024, 4 * 1024);
  let session = create_shell_session(&socket_path, "restart-target", "IFS= read -r line").await?;

  let (mut attached, attached_response) =
    attach_session(&socket_path, &session.session_id, None, true, false).await?;
  assert!(matches!(attached_response, ServerMessage::Attached { .. }));

  assert_eq!(
    std::fs::metadata(&control_path)?.permissions().mode() & 0o777,
    0o600
  );
  let control_stream = connect_when_ready(&control_path).await?;
  let terminated_sessions = request_local_daemon_restart(control_stream).await?;
  assert_eq!(terminated_sessions, 1);

  // Once the control endpoint has accepted restart, SessionEnded delivery is
  // best effort: a backpressured client may instead see the guaranteed data
  // connection closure that bounds daemon draining.
  wait_for_session_end_or_connection_close(&mut attached).await?;
  drop(attached);

  wait_for_daemon_exit(
    daemon,
    "rmuxd did not exit after cooperative restart drained every connection",
  )
  .await?;
  assert!(!socket_path.exists());
  assert!(!control_path.exists());
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_control_restart_cancels_a_stalled_data_connection() -> TestResult {
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("rmux.sock");
  let control_path = control_socket_path(&socket_path)?;
  let daemon = spawn_daemon(&socket_path, 64 * 1024, 4 * 1024);
  let _session = create_shell_session(&socket_path, "restart-target", "IFS= read -r line").await?;

  // Complete the raw handshake, then leave the data handler blocked waiting
  // for its first request. This used to keep ConnectionTracker alive until a
  // client disconnected, which could exceed the GUI's 15-second drain bound.
  let mut stalled = connect_when_ready(&socket_path).await?;
  handshake(&mut stalled).await?;

  let control_stream = connect_when_ready(&control_path).await?;
  assert_eq!(request_local_daemon_restart(control_stream).await?, 1);

  // Keep the peer socket open throughout shutdown. The daemon can only exit
  // within this test's three-second bound if it actively canceled the stalled
  // data handler rather than waiting for attachment liveness.
  wait_for_daemon_exit(
    daemon,
    "rmuxd did not cancel a stalled data connection during cooperative restart",
  )
  .await?;
  let response: Option<ServerMessage> = timeout(Duration::from_secs(1), read_frame(&mut stalled))
    .await
    .map_err(|_| "stalled data connection was not closed")??;
  assert!(response.is_none(), "stalled data connection remained open");
  drop(stalled);
  assert!(!socket_path.exists());
  assert!(!control_path.exists());
  Ok(())
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
    let result = run(DaemonConfig {
      socket_path,
      journal_capacity_bytes,
      checkpoint_interval_bytes,
      startup_idle_timeout: Duration::from_secs(5),
      attachment_liveness_timeout,
    })
    .await;
    if let Err(error) = &result {
      eprintln!("test rmuxd failed to start or serve: {error}");
    }
    result
  })
}

async fn create_shell_session(
  socket_path: &Path,
  name: &str,
  script: &str,
) -> TestResult<rmux_proto::SessionInfo> {
  create_shell_session_with_name(socket_path, Some(name), script).await
}

async fn create_shell_session_with_name(
  socket_path: &Path,
  name: Option<&str>,
  script: &str,
) -> TestResult<rmux_proto::SessionInfo> {
  let mut create = connect_when_ready(socket_path).await?;
  handshake(&mut create).await?;
  write_frame(
    &mut create,
    &ClientMessage::CreateSession {
      name: name.map(str::to_owned),
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

async fn kill_shell_session(socket_path: &Path, session: &str) -> TestResult {
  let mut stream = connect_when_ready(socket_path).await?;
  handshake(&mut stream).await?;
  write_frame(
    &mut stream,
    &ClientMessage::KillSession {
      session: session.into(),
    },
  )
  .await?;
  let response = required_message(&mut stream).await?;
  if response != ServerMessage::Success {
    return Err(format!("expected success after killing session, received {response:?}").into());
  }
  Ok(())
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
  attach_session_with_command_line_option(
    socket_path,
    session,
    resume_from,
    terminal_size,
    request_input_lease,
    request_layout_lease,
    false,
  )
  .await
}

async fn attach_session_with_command_line_request(
  socket_path: &Path,
  session: &str,
  request_input_lease: bool,
  request_command_line: bool,
) -> TestResult<(UnixStream, ServerMessage)> {
  attach_session_with_shell_metadata_options(
    socket_path,
    session,
    TestAttachmentOptions {
      request_input_lease,
      request_command_line,
      ..TestAttachmentOptions::default()
    },
  )
  .await
}

async fn attach_session_with_running_command_request(
  socket_path: &Path,
  session: &str,
  request_input_lease: bool,
) -> TestResult<(UnixStream, ServerMessage)> {
  attach_session_with_shell_metadata_options(
    socket_path,
    session,
    TestAttachmentOptions {
      request_input_lease,
      request_running_command: true,
      ..TestAttachmentOptions::default()
    },
  )
  .await
}

async fn attach_session_with_command_line_option(
  socket_path: &Path,
  session: &str,
  resume_from: Option<u64>,
  terminal_size: TerminalSize,
  request_input_lease: bool,
  request_layout_lease: bool,
  request_command_line: bool,
) -> TestResult<(UnixStream, ServerMessage)> {
  attach_session_with_shell_metadata_options(
    socket_path,
    session,
    TestAttachmentOptions {
      resume_from,
      terminal_size,
      request_input_lease,
      request_layout_lease,
      request_command_line,
      ..TestAttachmentOptions::default()
    },
  )
  .await
}

#[allow(
  clippy::struct_excessive_bools,
  reason = "test helper deliberately mirrors independent attach request flags"
)]
#[derive(Default)]
struct TestAttachmentOptions {
  resume_from: Option<u64>,
  terminal_size: TerminalSize,
  request_input_lease: bool,
  request_layout_lease: bool,
  request_command_line: bool,
  request_running_command: bool,
}

async fn attach_session_with_shell_metadata_options(
  socket_path: &Path,
  session: &str,
  options: TestAttachmentOptions,
) -> TestResult<(UnixStream, ServerMessage)> {
  let mut stream = connect_when_ready(socket_path).await?;
  handshake(&mut stream).await?;
  write_frame(
    &mut stream,
    &ClientMessage::AttachSession {
      session: session.into(),
      resume_from: options.resume_from,
      terminal_size: options.terminal_size,
      request_input_lease: options.request_input_lease,
      request_layout_lease: options.request_layout_lease,
      request_command_line: options.request_command_line,
      request_running_command: options.request_running_command,
    },
  )
  .await?;
  let attached = required_message(&mut stream).await?;
  Ok((stream, attached))
}

async fn resume_attachment(
  socket_path: &Path,
  session: &str,
  attachment_token: &str,
  resume_from: Option<u64>,
) -> TestResult<(UnixStream, ServerMessage)> {
  let mut stream = connect_when_ready(socket_path).await?;
  handshake(&mut stream).await?;
  write_frame(
    &mut stream,
    &ClientMessage::ResumeAttachment {
      session: session.into(),
      attachment_token: attachment_token.into(),
      resume_from,
      terminal_size: TerminalSize::default(),
      request_command_line: false,
      request_running_command: false,
    },
  )
  .await?;
  let attached = required_message(&mut stream).await?;
  Ok((stream, attached))
}

async fn assert_geometry_checkpoint_resume(
  socket_path: &Path,
  session: &str,
  geometry_boundary: u64,
  terminal_size: &TerminalSize,
) -> TestResult {
  for (resume_from, expected_history_gap) in [(geometry_boundary, false), (0, true)] {
    let (mut attachment, attached) = attach_session_with_options(
      socket_path,
      session,
      Some(resume_from),
      terminal_size.clone(),
      false,
      false,
    )
    .await?;
    let ServerMessage::Attached {
      checkpoint: Some(checkpoint),
      replay_from,
      history_gap,
      ..
    } = attached
    else {
      return Err(format!("expected geometry-safe checkpoint, received {attached:?}").into());
    };
    assert_eq!(&checkpoint.terminal_size, terminal_size);
    assert_eq!(checkpoint.sequence, geometry_boundary);
    assert_eq!(replay_from, geometry_boundary);
    assert_eq!(history_gap, expected_history_gap);

    write_frame(&mut attachment, &ClientMessage::Detach).await?;
    wait_for_detached(&mut attachment).await?;
  }
  Ok(())
}

async fn wait_for_detached(stream: &mut UnixStream) -> TestResult {
  loop {
    if required_message(stream).await? == ServerMessage::Detached {
      return Ok(());
    }
  }
}

async fn wait_for_shell_state(socket_path: &Path, session: &str) -> TestResult<ShellState> {
  let deadline = Instant::now() + Duration::from_secs(3);
  loop {
    let mut stream = connect_when_ready(socket_path).await?;
    handshake(&mut stream).await?;
    write_frame(
      &mut stream,
      &ClientMessage::GetShellState {
        session: session.into(),
      },
    )
    .await?;
    let response = required_message(&mut stream).await?;
    let ServerMessage::ShellStateResponse { shell_state, .. } = response else {
      return Err(format!("expected shell state response, received {response:?}").into());
    };
    if shell_state.revision > 0 {
      return Ok(shell_state);
    }
    if Instant::now() >= deadline {
      return Err("shell reporter did not publish state".into());
    }
    sleep(Duration::from_millis(10)).await;
  }
}

async fn wait_for_tui_hint(stream: &mut UnixStream, expected: rmux_proto::TuiHint) -> TestResult {
  loop {
    match required_message(stream).await? {
      ServerMessage::ShellStateChanged { state } if state.tui_hint == expected => return Ok(()),
      ServerMessage::ShellStateChanged { .. }
      | ServerMessage::Output { .. }
      | ServerMessage::Checkpoint { .. }
      | ServerMessage::PtyGeometryChanged { .. } => {}
      response => {
        return Err(format!("expected tui hint {expected:?}, received {response:?}").into());
      }
    }
  }
}

async fn wait_for_unredacted_command_line(
  stream: &mut UnixStream,
  after_revision: u64,
) -> TestResult<ShellState> {
  loop {
    match required_message(stream).await? {
      ServerMessage::ShellStateChanged { state }
        if state.revision > after_revision
          && !state.command_line_redacted
          && state.current_command_line.is_some() =>
      {
        return Ok(state);
      }
      ServerMessage::ShellStateChanged { .. }
      | ServerMessage::Output { .. }
      | ServerMessage::Checkpoint { .. }
      | ServerMessage::PtyGeometryChanged { .. } => {}
      message => {
        return Err(format!("expected unredacted shell state, received {message:?}").into());
      }
    }
  }
}

async fn wait_for_visible_running_command(
  stream: &mut UnixStream,
  after_revision: u64,
) -> TestResult<ShellState> {
  loop {
    match required_message(stream).await? {
      ServerMessage::ShellStateChanged { state }
        if state.revision > after_revision
          && !state.running_command_redacted
          && state.running_command.is_some() =>
      {
        return Ok(state);
      }
      ServerMessage::ShellStateChanged { .. }
      | ServerMessage::Output { .. }
      | ServerMessage::Checkpoint { .. }
      | ServerMessage::PtyGeometryChanged { .. } => {}
      message => {
        return Err(format!("expected visible running command, received {message:?}").into());
      }
    }
  }
}

async fn wait_for_redacted_running_command(
  stream: &mut UnixStream,
  after_revision: u64,
) -> TestResult<ShellState> {
  loop {
    match required_message(stream).await? {
      ServerMessage::ShellStateChanged { state }
        if state.revision > after_revision
          && state.running_command_redacted
          && state.running_command.is_none() =>
      {
        return Ok(state);
      }
      ServerMessage::ShellStateChanged { .. }
      | ServerMessage::Output { .. }
      | ServerMessage::Checkpoint { .. }
      | ServerMessage::PtyGeometryChanged { .. } => {}
      message => {
        return Err(format!("expected redacted running command, received {message:?}").into());
      }
    }
  }
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
      ServerMessage::Output { .. }
      | ServerMessage::Checkpoint { .. }
      | ServerMessage::ShellStateChanged { .. }
      | ServerMessage::PtyGeometryChanged { .. } => {}
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
      ServerMessage::Output { .. }
      | ServerMessage::Checkpoint { .. }
      | ServerMessage::ShellStateChanged { .. }
      | ServerMessage::PtyGeometryChanged { .. } => {}
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
      ServerMessage::Output { .. }
      | ServerMessage::Checkpoint { .. }
      | ServerMessage::ShellStateChanged { .. }
      | ServerMessage::PtyGeometryChanged { .. } => {}
      response => return Err(format!("expected error response, received {response:?}").into()),
    }
  }
}

async fn wait_for_session_end(stream: &mut UnixStream) -> TestResult {
  loop {
    match required_message(stream).await? {
      ServerMessage::SessionEnded { .. } => return Ok(()),
      ServerMessage::Output { .. }
      | ServerMessage::ShellStateChanged { .. }
      | ServerMessage::PtyGeometryChanged { .. } => {}
      response => {
        return Err(format!("expected output or session end, received {response:?}").into());
      }
    }
  }
}

async fn wait_for_session_end_or_connection_close(stream: &mut UnixStream) -> TestResult {
  loop {
    let message: Option<ServerMessage> = timeout(Duration::from_secs(3), read_frame(stream))
      .await
      .map_err(|_| "timed out waiting for rmuxd to end or close the attachment")??;
    let Some(message) = message else {
      return Ok(());
    };
    match message {
      ServerMessage::SessionEnded { .. } => return Ok(()),
      ServerMessage::Output { .. }
      | ServerMessage::ShellStateChanged { .. }
      | ServerMessage::PtyGeometryChanged { .. } => {}
      response => {
        return Err(
          format!("expected output, session end, or close, received {response:?}").into(),
        );
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
      ServerMessage::ShellStateChanged { .. }
      | ServerMessage::Checkpoint { .. }
      | ServerMessage::PtyGeometryChanged { .. } => {}
      message => return Err(format!("expected output, received {message:?}").into()),
    }
  }
}

async fn read_output_until_with_first_sequence(
  stream: &mut UnixStream,
  expected: &[u8],
) -> TestResult<(Vec<u8>, u64)> {
  let mut output = Vec::new();
  let mut first_sequence = None;
  loop {
    match required_message(stream).await? {
      ServerMessage::Output {
        sequence_start,
        data,
        ..
      } => {
        first_sequence.get_or_insert(sequence_start);
        output.extend(data);
        if contains_bytes(&output, expected) {
          return Ok((
            output,
            first_sequence.expect("an output frame establishes its first sequence"),
          ));
        }
      }
      ServerMessage::ShellStateChanged { .. }
      | ServerMessage::Checkpoint { .. }
      | ServerMessage::PtyGeometryChanged { .. } => {}
      message => return Err(format!("expected output, received {message:?}").into()),
    }
  }
}

async fn wait_for_geometry_change(
  stream: &mut UnixStream,
  expected_size: &TerminalSize,
) -> TestResult<u64> {
  loop {
    match required_message(stream).await? {
      ServerMessage::PtyGeometryChanged {
        terminal_size,
        observed_sequence,
      } => {
        assert_eq!(&terminal_size, expected_size);
        return Ok(observed_sequence);
      }
      ServerMessage::Output { .. }
      | ServerMessage::ShellStateChanged { .. }
      | ServerMessage::Checkpoint { .. } => {}
      message => return Err(format!("expected geometry change, received {message:?}").into()),
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
