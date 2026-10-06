#![cfg(unix)]

use ctmux_ipc::{control_socket_path, request_local_daemon_restart};
use ctmux_proto::{
  ClientMessage, CommandSpec, DEFAULT_PRESENTATION_WINDOW_BYTES, ErrorCode, LeaseKind, LeaseStatus,
  PROTOCOL_VERSION, ServerMessage, SessionInfo, ShellState, TerminalSize, read_frame, write_frame,
};
use ctmuxd::{DEFAULT_ATTACHMENT_LIVENESS_TIMEOUT, DaemonConfig, run};
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

#[tokio::test]
async fn published_contract_handshakes_select_explicit_shared_versions() -> TestResult {
  use ctl_core::protocol::{ProtocolOffer, ProtocolVersion};
  use ctmux_ipc::{LocalControlClientMessage, LocalControlServerMessage};
  let directory = TestDirectory::new();
  let socket = directory.path.join("ctmux.sock");
  let daemon = spawn_daemon(&socket, 4096, 1024);
  let future = ProtocolVersion::new(1, 1, 17);
  let mut stream = connect_when_ready(&socket).await?;
  write_frame(
    &mut stream,
    &ClientMessage::Handshake {
      protocol: ProtocolOffer::new(
        17,
        future,
        &[ctmux_proto::CONTRACT_V1_0_13, PROTOCOL_VERSION, future],
      ),
      client_name: "future-client".into(),
      client_version: "test".into(),
    },
  )
  .await?;
  let ServerMessage::HandshakeAccepted {
    protocol_version,
    protocols,
    ..
  } = required_message(&mut stream).await?
  else {
    panic!("handshake was rejected");
  };
  assert_eq!(protocol_version, PROTOCOL_VERSION);
  assert!(protocols.contains(&ctmux_proto::protocol_info()));
  write_frame(&mut stream, &ClientMessage::ListSessions).await?;
  assert_eq!(
    required_message(&mut stream).await?,
    ServerMessage::SessionList {
      sessions: Vec::new()
    }
  );

  let mut control = connect_when_ready(&control_socket_path(&socket)?).await?;
  ctmux_ipc::write_local_control_frame(
    &mut control,
    &LocalControlClientMessage::Handshake {
      protocol: ProtocolOffer::new(
        17,
        future,
        &[ctmux_ipc::LOCAL_CONTROL_PROTOCOL_VERSION, future],
      ),
    },
  )
  .await?;
  let reply =
    ctmux_ipc::read_local_control_frame::<_, LocalControlServerMessage>(&mut control).await?;
  assert!(
    matches!(reply, Some(LocalControlServerMessage::HandshakeAccepted { protocol_version, .. })
    if protocol_version == ctmux_ipc::LOCAL_CONTROL_PROTOCOL_VERSION)
  );
  daemon.abort();
  let _result = daemon.await;
  Ok(())
}

#[tokio::test]
async fn first_published_contract_keeps_complete_inline_history() -> TestResult {
  use ctl_core::protocol::ProtocolOffer;
  let _guard = PTY_TEST_LOCK.get_or_init(|| Mutex::new(())).lock().await;
  let directory = TestDirectory::new();
  let socket = directory.path.join("ctmux.sock");
  let daemon = spawn_daemon(&socket, 64 * 1024, 4 * 1024);
  let session = create_shell_session(&socket, "legacy-history",
    "i=0; while [ $i -lt 150 ]; do printf 'line-%03d\\n' \"$i\"; i=$((i + 1)); done; printf 'ready\\n'; IFS= read -r line"
  ).await?;
  let (mut owner, _) = attach_session(&socket, &session.session_id, None, true, true).await?;
  read_output_until(&mut owner, b"ready").await?;
  let mut legacy = connect_when_ready(&socket).await?;
  let old = ctmux_proto::CONTRACT_V1_0_13;
  write_frame(
    &mut legacy,
    &ClientMessage::Handshake {
      protocol: ProtocolOffer::new(13, old, &[old]),
      client_name: "legacy".into(),
      client_version: "test".into(),
    },
  )
  .await?;
  assert!(
    matches!(required_message(&mut legacy).await?, ServerMessage::HandshakeAccepted { protocol_version, .. } if protocol_version == old)
  );
  write_frame(
    &mut legacy,
    &ClientMessage::AttachSession {
      session: session.session_id.clone(),
      resume_from: None,
      terminal_size: TerminalSize::default(),
      request_input_lease: false,
      request_layout_lease: false,
      request_command_line: false,
      request_running_command: false,
      presentation_window_bytes: DEFAULT_PRESENTATION_WINDOW_BYTES,
    },
  )
  .await?;
  let ServerMessage::Attached {
    history: Some(history),
    checkpoint: Some(checkpoint),
    history_manifest,
    ..
  } = raw_message(&mut legacy).await?
  else {
    return Err("legacy attach did not receive inline history".into());
  };
  assert!(history_manifest.is_none());
  assert!(history.lines.len() > 64);
  assert!(history.lines.iter().any(|line| line == "line-000"));
  acknowledge_output(&mut legacy, checkpoint.sequence).await?;
  write_frame(&mut legacy, &ClientMessage::Detach).await?;
  wait_for_detached(&mut legacy).await?;
  drop(legacy);
  kill_shell_session(&socket, &session.session_id).await?;
  drop(owner);
  daemon.abort();
  Ok(())
}

#[tokio::test]
async fn unpublished_or_incompatible_contracts_are_rejected() -> TestResult {
  use ctl_core::protocol::{ProtocolOffer, ProtocolVersion};
  use ctmux_ipc::{LocalControlClientMessage, LocalControlErrorCode, LocalControlServerMessage};
  let directory = TestDirectory::new();
  let socket = directory.path.join("ctmux.sock");
  let daemon = spawn_daemon(&socket, 4096, 1024);
  for unsupported in [
    ProtocolVersion::new(1, 0, 14),
    ProtocolVersion::new(2, 0, 14),
  ] {
    let mut stream = connect_when_ready(&socket).await?;
    write_frame(
      &mut stream,
      &ClientMessage::Handshake {
        protocol: ProtocolOffer::new(14, unsupported, &[unsupported]),
        client_name: "incompatible-client".into(),
        client_version: "test".into(),
      },
    )
    .await?;
    assert!(matches!(
      required_message(&mut stream).await?,
      ServerMessage::Error {
        code: ErrorCode::ProtocolVersionMismatch,
        ..
      }
    ));
    assert!(read_frame::<_, ServerMessage>(&mut stream).await?.is_none());

    let mut control = connect_when_ready(&control_socket_path(&socket)?).await?;
    ctmux_ipc::write_local_control_frame(
      &mut control,
      &LocalControlClientMessage::Handshake {
        protocol: ProtocolOffer::new(14, unsupported, &[unsupported]),
      },
    )
    .await?;
    assert!(matches!(
      ctmux_ipc::read_local_control_frame::<_, LocalControlServerMessage>(&mut control).await?,
      Some(LocalControlServerMessage::Error {
        code: LocalControlErrorCode::ProtocolVersionMismatch,
        ..
      })
    ));
  }
  // Historical integer builds never advertised a published contract.
  for payload in [
    serde_json::json!({"type":"handshake", "protocol_version":13, "client_name":"old", "client_version":"test"}),
    serde_json::json!({"type":"handshake", "client_name":"missing", "client_version":"test"}),
  ] {
    let mut stream = connect_when_ready(&socket).await?;
    write_frame(&mut stream, &payload).await?;
    assert!(
      timeout(
        Duration::from_secs(1),
        read_frame::<_, ServerMessage>(&mut stream)
      )
      .await??
      .is_none()
    );
  }
  for payload in [
    serde_json::json!({"type":"handshake", "protocol_version":1}),
    serde_json::json!({"type":"handshake"}),
  ] {
    let mut control = connect_when_ready(&control_socket_path(&socket)?).await?;
    ctmux_ipc::write_local_control_frame(&mut control, &payload).await?;
    assert!(
      timeout(
        Duration::from_secs(1),
        ctmux_ipc::read_local_control_frame::<_, LocalControlServerMessage>(&mut control)
      )
      .await??
      .is_none()
    );
  }
  daemon.abort();
  let _result = daemon.await;
  Ok(())
}

async fn pty_test_lock() -> MutexGuard<'static, ()> {
  PTY_TEST_LOCK.get_or_init(|| Mutex::new(())).lock().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_survives_client_disconnect_and_resumes_from_sequence() -> TestResult {
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("ctmux.sock");
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
        checkpoint: Some(_),
        history_gap: false,
        ..
      },
    ),
    "expected an authoritative initial checkpoint, received {first_attached:?}"
  );
  let (first_output, resume_sequence) = read_output_until(&mut first_attach, b"before").await?;
  assert!(contains_bytes(&first_output, b"before"));
  write_frame(&mut first_attach, &ClientMessage::Detach).await?;
  wait_for_detached(&mut first_attach)
    .await
    .map_err(|error| format!("first attachment did not detach: {error}"))?;
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
  let second_output = wait_for_session_end(&mut second_attach).await?;
  assert!(!contains_bytes(&second_output, b"before"));
  assert!(contains_bytes(&second_output, b"after:go"));
  drop(second_attach);

  let daemon_result = timeout(Duration::from_secs(3), daemon)
    .await
    .map_err(|_| "ctmuxd did not exit after its final session ended")?;
  daemon_result??;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unnamed_sessions_get_monotonic_names_without_colliding_with_explicit_names() -> TestResult
{
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("ctmux.sock");
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

  wait_for_daemon_exit(daemon, "ctmuxd did not exit after automatic naming test").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkpoint_restores_terminal_state_after_journal_compaction() -> TestResult {
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("ctmux.sock");
  let daemon = spawn_daemon(&socket_path, 32, 1);
  let session = create_shell_session(
    &socket_path,
    "checkpoint",
    "printf '\\033[?1002;1006;2004hxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'; printf '\\033[2J\\033[Hcheckpoint-ready'; IFS= read -r line",
  )
  .await
  .map_err(|error| format!("checkpoint session was not created: {error}"))?;

  let (mut first_attach, first_attached) =
    attach_session(&socket_path, &session.session_id, Some(0), false, false)
      .await
      .map_err(|error| format!("first checkpoint attachment did not open: {error}"))?;
  assert!(matches!(first_attached, ServerMessage::Attached { .. }));
  let (initial_output, initial_sequence) = match &first_attached {
    ServerMessage::Attached {
      checkpoint: Some(checkpoint),
      ..
    } => (checkpoint.payload.clone(), checkpoint.sequence),
    _ => (Vec::new(), 0),
  };
  // Attachment can capture a checkpoint before the shell finishes printing.
  // Apply subsequent presentation frames before asserting the terminal state.
  let (initial_output, _) = read_output_until_from(
    &mut first_attach,
    b"checkpoint-ready",
    initial_output,
    initial_sequence,
  )
  .await?;
  assert!(contains_bytes(&initial_output, b"checkpoint-ready"));
  let mut initial_modes = ctmux_core::mouse::TerminalInputModes::default();
  for ch in std::str::from_utf8(&initial_output)?.chars() {
    initial_modes.feed(ch);
  }
  assert!(initial_modes.mouse().enabled());
  assert!(initial_modes.bracketed_paste());
  write_frame(&mut first_attach, &ClientMessage::Detach).await?;
  wait_for_detached(&mut first_attach).await?;
  drop(first_attach);

  let (mut restored_attach, attached) =
    attach_session(&socket_path, &session.session_id, Some(0), true, false)
      .await
      .map_err(|error| format!("restored checkpoint attachment did not open: {error}"))?;
  let ServerMessage::Attached {
    checkpoint: Some(checkpoint),
    history: Some(history),
    history_gap: true,
    terminal_size_mismatch: false,
    ..
  } = attached
  else {
    return Err(format!("expected checkpoint-backed attach, received {attached:?}").into());
  };
  assert!(checkpoint.is_supported());
  assert!(history.is_supported());
  assert_eq!(history.sequence, checkpoint.sequence);

  let mut restored_modes = ctmux_core::mouse::TerminalInputModes::default();
  for ch in std::str::from_utf8(&checkpoint.payload)?.chars() {
    restored_modes.feed(ch);
  }
  assert_eq!(restored_modes.mouse(), initial_modes.mouse());
  assert_eq!(
    restored_modes.bracketed_paste(),
    initial_modes.bracketed_paste()
  );
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
    let message = required_message(&mut restored_attach)
      .await
      .map_err(|error| format!("restored attachment did not finish: {error}"))?;
    if matches!(message, ServerMessage::SessionEnded { .. }) {
      break;
    }
  }
  drop(restored_attach);

  let daemon_result = timeout(Duration::from_secs(3), daemon)
    .await
    .map_err(|_| "ctmuxd did not exit after checkpoint test")?;
  daemon_result??;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn frozen_history_pages_survive_exit_and_geometry_replaces_the_snapshot() -> TestResult {
  use sha2::{Digest as _, Sha256};

  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("ctmux.sock");
  let daemon =
    spawn_daemon_with_liveness(&socket_path, 64 * 1024, 4 * 1024, Duration::from_secs(5));
  let session = create_shell_session(
    &socket_path, "history-pages",
    "i=0; while [ $i -lt 250 ]; do printf 'line-%03d\\n' \"$i\"; i=$((i + 1)); done; printf 'ready\\n'; IFS= read -r line",
  ).await?;
  let (mut owner, _) = attach_session(&socket_path, &session.session_id, None, true, true).await?;
  read_output_until(&mut owner, b"ready").await?;

  // This reader deliberately owns neither control lease and downloads no
  // history implicitly, so its frozen window is observable on the wire.
  let (mut viewer, attached) = attach_raw_viewer(&socket_path, &session.session_id).await?;
  let ServerMessage::Attached {
    checkpoint: Some(checkpoint),
    history: Some(recent),
    history_manifest: Some(initial),
    input_lease,
    layout_lease,
    ..
  } = attached
  else {
    return Err("expected current screen and paged history manifest".into());
  };
  assert!(!input_lease.owned_by_client && !layout_lease.owned_by_client);
  assert!(initial.total_lines > 64);
  assert!(recent.lines.len() <= 64);
  assert_eq!(
    initial.first_line + recent.lines.len() as u64,
    initial.total_lines
  );
  assert_eq!(initial.sequence, checkpoint.sequence);
  assert!(contains_bytes(&checkpoint.payload, b"ready"));
  acknowledge_output(&mut viewer, checkpoint.sequence).await?;

  write_frame(
    &mut viewer,
    &ClientMessage::HistoryRequest {
      snapshot_id: initial.snapshot_id.clone(),
      offset: 0,
      max_bytes: 101,
    },
  )
  .await?;
  let first_page = raw_history_page(&mut viewer).await?;
  assert!(
    matches!(first_page, ServerMessage::HistoryPage { offset: 0, ref data,
    next_offset: Some(101), .. } if data.len() == 101)
  );
  heartbeat(&mut viewer, 71).await?;

  let resized = terminal_size(100, 30);
  write_frame(
    &mut owner,
    &ClientMessage::Resize {
      terminal_size: resized.clone(),
    },
  )
  .await?;
  let (geometry_checkpoint, geometry_manifest) = raw_checkpoint(&mut viewer).await?;
  assert_eq!(geometry_checkpoint.terminal_size, resized);
  assert_eq!(geometry_checkpoint.sequence, checkpoint.sequence);
  assert_ne!(geometry_manifest.snapshot_id, initial.snapshot_id);
  acknowledge_output(&mut viewer, geometry_checkpoint.sequence).await?;
  write_frame(
    &mut viewer,
    &ClientMessage::HistoryRequest {
      snapshot_id: initial.snapshot_id.clone(),
      offset: 101,
      max_bytes: 101,
    },
  )
  .await?;
  expect_history_expired(&mut viewer, &initial.snapshot_id).await?;

  // Recovery requests need no lease and produce a fresh identity even when
  // no output has changed. This also proves the replacement cannot be
  // mistaken for the older window solely because its byte sequence is equal.
  write_frame(&mut viewer, &ClientMessage::RequestCheckpoint).await?;
  let (fresh_checkpoint, manifest) = raw_checkpoint(&mut viewer).await?;
  assert_eq!(fresh_checkpoint.sequence, geometry_checkpoint.sequence);
  assert_ne!(manifest.snapshot_id, geometry_manifest.snapshot_id);
  acknowledge_output(&mut viewer, fresh_checkpoint.sequence).await?;
  write_frame(&mut owner, &ClientMessage::Detach).await?;
  wait_for_detached(&mut owner).await?;
  drop(owner);
  kill_shell_session(&socket_path, &session.session_id).await?;

  // Exit must not discard the pending pinned history. Finish that transfer
  // before accepting SessionEnded; pages may split JSON strings and UTF-8.
  let bytes = read_history_bytes(&mut viewer, &manifest, 127).await?;
  assert_eq!(bytes.len() as u64, manifest.total_bytes);
  assert_eq!(
    format!("{:x}", Sha256::digest(&bytes)),
    manifest.content_hash
  );
  let rows: Vec<ctmux_proto::TerminalHistoryRow> = bytes
    .split(|byte| *byte == b'\n')
    .filter(|row| !row.is_empty())
    .map(serde_json::from_slice)
    .collect::<Result<_, _>>()?;
  assert_eq!(rows.len() as u64, manifest.total_rows);
  let lines = ctmux_proto::normalize_history_rows(&rows);
  assert_eq!(lines.len() as u64, manifest.total_lines);
  assert_eq!(lines.first().map(String::as_str), Some("line-000"));
  wait_for_session_end(&mut viewer).await?;
  drop(viewer);
  wait_for_daemon_exit(daemon, "ctmuxd did not exit after history transfer").await
}

async fn attach_raw_viewer(
  socket_path: &Path,
  session: &str,
) -> TestResult<(UnixStream, ServerMessage)> {
  let mut stream = connect_when_ready(socket_path).await?;
  handshake(&mut stream).await?;
  write_frame(
    &mut stream,
    &ClientMessage::AttachSession {
      session: session.into(),
      resume_from: None,
      terminal_size: TerminalSize::default(),
      request_input_lease: false,
      request_layout_lease: false,
      request_command_line: false,
      request_running_command: false,
      presentation_window_bytes: DEFAULT_PRESENTATION_WINDOW_BYTES,
    },
  )
  .await?;
  let attached = raw_message(&mut stream).await?;
  Ok((stream, attached))
}

async fn read_history_bytes(
  stream: &mut UnixStream,
  manifest: &ctmux_proto::TerminalHistoryManifest,
  max_bytes: u64,
) -> TestResult<Vec<u8>> {
  let mut bytes = Vec::new();
  let mut offset = 0;
  loop {
    write_frame(
      stream,
      &ClientMessage::HistoryRequest {
        snapshot_id: manifest.snapshot_id.clone(),
        offset,
        max_bytes,
      },
    )
    .await?;
    let (page_offset, data, next_offset) = loop {
      match raw_message(stream).await? {
        ServerMessage::HistoryPage {
          snapshot_id,
          offset,
          data,
          next_offset,
        } => {
          assert_eq!(snapshot_id, manifest.snapshot_id);
          break (offset, data, next_offset);
        }
        ServerMessage::Output { sequence_end, .. } => {
          acknowledge_output(stream, sequence_end).await?;
        }
        ServerMessage::ViewSnapshot { .. }
        | ServerMessage::ShellStateChanged { .. }
        | ServerMessage::LeaseStatus {
          notification: true, ..
        } => {}
        other => {
          return Err(format!("history was interrupted before completion: {other:?}").into());
        }
      }
    };
    assert_eq!(page_offset, offset);
    assert!(data.len() as u64 <= max_bytes);
    bytes.extend(data);
    let Some(next) = next_offset else {
      break;
    };
    offset = next;
  }
  Ok(bytes)
}

async fn expect_history_expired(stream: &mut UnixStream, expected: &str) -> TestResult {
  loop {
    match raw_message(stream).await? {
      ServerMessage::HistorySnapshotExpired { snapshot_id } => {
        assert_eq!(snapshot_id, expected);
        return Ok(());
      }
      ServerMessage::ViewSnapshot { .. }
      | ServerMessage::ShellStateChanged { .. }
      | ServerMessage::LeaseStatus {
        notification: true, ..
      } => {}
      other => {
        return Err(format!("expected scoped history expiration, received {other:?}").into());
      }
    }
  }
}

async fn raw_history_page(stream: &mut UnixStream) -> TestResult<ServerMessage> {
  loop {
    match raw_message(stream).await? {
      message @ ServerMessage::HistoryPage { .. } => return Ok(message),
      ServerMessage::Output { sequence_end, .. } => {
        acknowledge_output(stream, sequence_end).await?;
      }
      ServerMessage::ViewSnapshot { .. }
      | ServerMessage::ShellStateChanged { .. }
      | ServerMessage::LeaseStatus {
        notification: true, ..
      } => {}
      other => return Err(format!("expected history page, received {other:?}").into()),
    }
  }
}

async fn raw_checkpoint(
  stream: &mut UnixStream,
) -> TestResult<(
  ctmux_proto::TerminalCheckpoint,
  ctmux_proto::TerminalHistoryManifest,
)> {
  loop {
    match raw_message(stream).await? {
      ServerMessage::Checkpoint {
        checkpoint,
        history_manifest: Some(manifest),
        ..
      } => return Ok((checkpoint, *manifest)),
      ServerMessage::Output { sequence_end, .. } => {
        acknowledge_output(stream, sequence_end).await?;
      }
      ServerMessage::ViewSnapshot { .. }
      | ServerMessage::ShellStateChanged { .. }
      | ServerMessage::LeaseStatus {
        notification: true, ..
      } => {}
      other => return Err(format!("expected replacing checkpoint, received {other:?}").into()),
    }
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn presentation_window_pauses_output_without_blocking_heartbeats() -> TestResult {
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("ctmux.sock");
  let daemon = spawn_daemon(&socket_path, 64 * 1024, 64 * 1024);
  let session = create_shell_session(
    &socket_path,
    "presentation-window",
    "yes x | head -c 16384; IFS= read -r line",
  )
  .await?;

  let mut attachment = connect_when_ready(&socket_path).await?;
  handshake(&mut attachment).await?;
  write_frame(
    &mut attachment,
    &ClientMessage::AttachSession {
      session: session.session_id.clone(),
      resume_from: None,
      terminal_size: TerminalSize::default(),
      request_input_lease: false,
      request_layout_lease: false,
      request_command_line: false,
      request_running_command: false,
      presentation_window_bytes: 4 * 1024,
    },
  )
  .await?;
  let attached = required_message(&mut attachment).await?;
  let ServerMessage::Attached {
    checkpoint: Some(checkpoint),
    ..
  } = attached
  else {
    return Err(format!("expected checkpoint-backed attachment, received {attached:?}").into());
  };
  write_frame(
    &mut attachment,
    &ClientMessage::PresentationApplied {
      sequence: checkpoint.sequence,
    },
  )
  .await?;

  let first_sequence_end = loop {
    if let ServerMessage::Output {
      sequence_start,
      sequence_end,
      data,
    } = required_message(&mut attachment).await?
    {
      assert_eq!(sequence_start, checkpoint.sequence);
      assert_ne!(data, Vec::<u8>::new());
      assert!(data.len() <= 4 * 1024);
      break sequence_end;
    }
  };

  write_frame(&mut attachment, &ClientMessage::Heartbeat { nonce: 99 }).await?;
  loop {
    match required_message(&mut attachment).await? {
      ServerMessage::HeartbeatAck { nonce: 99 } => break,
      ServerMessage::Output { .. } => {
        return Err("daemon exceeded presentation credit before renderer progress".into());
      }
      ServerMessage::ViewSnapshot { .. }
      | ServerMessage::ShellStateChanged { .. }
      | ServerMessage::LeaseStatus {
        notification: true, ..
      } => {}
      message => {
        return Err(format!("unexpected message before heartbeat ACK: {message:?}").into());
      }
    }
  }

  write_frame(
    &mut attachment,
    &ClientMessage::PresentationApplied {
      sequence: first_sequence_end,
    },
  )
  .await?;
  loop {
    if let ServerMessage::Output {
      sequence_start,
      sequence_end,
      data,
      ..
    } = required_message(&mut attachment).await?
    {
      assert_eq!(sequence_start, first_sequence_end);
      assert_ne!(data, Vec::<u8>::new());
      acknowledge_output(&mut attachment, sequence_end).await?;
      break;
    }
  }

  kill_shell_session(&socket_path, &session.session_id).await?;
  wait_for_session_end(&mut attachment).await?;
  drop(attachment);
  wait_for_daemon_exit(daemon, "ctmuxd did not exit after presentation-window test").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ended_session_drains_output_after_a_delayed_checkpoint_ack() -> TestResult {
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("ctmux.sock");
  let mut daemon = spawn_daemon(&socket_path, 64 * 1024, 64 * 1024);
  let session = create_shell_session(
    &socket_path,
    "final-checkpoint",
    "IFS= read -r line; printf 'final:%s\\n' \"$line\"",
  )
  .await?;
  let (mut attachment, checkpoint_sequence) = attach_with_pending_checkpoint(
    &socket_path,
    &session.session_id,
    DEFAULT_PRESENTATION_WINDOW_BYTES,
  )
  .await?;
  write_frame(
    &mut attachment,
    &ClientMessage::Input {
      data: b"after-checkpoint\n".to_vec(),
    },
  )
  .await?;
  wait_for_session_removal(&socket_path, &session.session_id).await?;

  // The child has exited, but terminal output still depends on this renderer's
  // checkpoint acknowledgement. SessionEnded must not cut that output off.
  assert!(
    timeout(Duration::from_millis(100), &mut daemon)
      .await
      .is_err(),
    "daemon closed an attachment with output blocked behind a checkpoint"
  );
  acknowledge_output(&mut attachment, checkpoint_sequence).await?;
  let output = wait_for_session_end(&mut attachment).await?;
  assert!(contains_bytes(&output, b"final:after-checkpoint"));
  drop(attachment);
  wait_for_daemon_exit(daemon, "ctmuxd did not exit after final checkpoint drain").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ended_session_drains_output_larger_than_the_presentation_window() -> TestResult {
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("ctmux.sock");
  let mut daemon = spawn_daemon(&socket_path, 64 * 1024, 64 * 1024);
  let session = create_shell_session(
    &socket_path,
    "final-window",
    "IFS= read -r line; printf '%016384d' 0; printf ':final-tail\\n'",
  )
  .await?;
  let (mut attachment, checkpoint_sequence) =
    attach_with_pending_checkpoint(&socket_path, &session.session_id, 4 * 1024).await?;
  acknowledge_output(&mut attachment, checkpoint_sequence).await?;
  write_frame(
    &mut attachment,
    &ClientMessage::Input {
      data: b"go\n".to_vec(),
    },
  )
  .await?;
  wait_for_session_removal(&socket_path, &session.session_id).await?;
  assert!(
    timeout(Duration::from_millis(100), &mut daemon)
      .await
      .is_err(),
    "daemon closed an attachment before its final output fit the presentation window"
  );

  let output = wait_for_session_end(&mut attachment).await?;
  assert!(contains_bytes(&output, &vec![b'0'; 16_384]));
  assert!(contains_bytes(&output, b":final-tail"));
  drop(attachment);
  wait_for_daemon_exit(daemon, "ctmuxd did not exit after final window drain").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ended_session_drain_expires_even_when_a_stalled_renderer_heartbeats() -> TestResult {
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("ctmux.sock");
  let daemon = spawn_daemon_with_liveness(
    &socket_path,
    64 * 1024,
    64 * 1024,
    Duration::from_millis(500),
  );
  let session = create_shell_session(
    &socket_path,
    "stalled-final",
    "IFS= read -r line; printf 'final\\n'",
  )
  .await?;
  let (mut attachment, _) = attach_with_pending_checkpoint(
    &socket_path,
    &session.session_id,
    DEFAULT_PRESENTATION_WINDOW_BYTES,
  )
  .await?;
  write_frame(
    &mut attachment,
    &ClientMessage::Input {
      data: b"go\n".to_vec(),
    },
  )
  .await?;
  wait_for_session_removal(&socket_path, &session.session_id).await?;
  let heartbeats = tokio::spawn(async move {
    for nonce in 0..100 {
      if heartbeat(&mut attachment, nonce).await.is_err() {
        break;
      }
      sleep(Duration::from_millis(25)).await;
    }
  });
  let result = timeout(Duration::from_secs(2), daemon).await;
  heartbeats.abort();
  result.map_err(|_| "heartbeats kept an ended session's stalled output drain alive")???;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_metadata_tracks_unintegrated_shells_even_while_detached() -> TestResult {
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("ctmux.sock");
  let daemon = spawn_daemon(&socket_path, 64 * 1024, 4 * 1024);
  let session = create_shell_session(
    &socket_path,
    "native-state",
    "export ENV=/dev/null; exec /bin/sh -i",
  )
  .await?;
  let (mut attachment, _) =
    attach_session(&socket_path, &session.session_id, None, true, false).await?;
  write_frame(
    &mut attachment,
    &ClientMessage::Input {
      data: b"cd /; printf '\\137\\137ready\\137\\137\\n'\n".to_vec(),
    },
  )
  .await?;
  read_output_until(&mut attachment, b"__ready__").await?;
  let idle = wait_for_shell_state_matching(&socket_path, &session.session_id, |state| {
    state.cwd.as_deref() == Some("/")
      && state
        .process
        .as_ref()
        .is_some_and(|process| process.foreground == ctmux_proto::ForegroundProcess::Shell)
  })
  .await?;
  assert_eq!(idle.cwd_source, Some(ctmux_proto::CwdSource::Process));
  assert_eq!(idle.shell, ctmux_proto::ShellDescriptor::default());
  assert_eq!(idle.prompt_phase, ctmux_proto::PromptPhase::Unknown);

  write_frame(
    &mut attachment,
    &ClientMessage::Input {
      data: b"sleep 60\n".to_vec(),
    },
  )
  .await?;
  write_frame(&mut attachment, &ClientMessage::Detach).await?;
  wait_for_detached(&mut attachment).await?;
  drop(attachment);
  let running = wait_for_shell_state_matching(&socket_path, &session.session_id, |state| {
    state.process.as_ref().is_some_and(|process| {
      matches!(&process.foreground, ctmux_proto::ForegroundProcess::Child { name, .. }
        if name.as_deref() == Some("sleep"))
    })
  })
  .await?;
  assert!(running.revision > idle.revision);
  assert_eq!(running.running_command, None);
  assert_eq!(running.current_command_line, None);
  assert_eq!(running.prompt_phase, ctmux_proto::PromptPhase::Unknown);
  assert_eq!(running.cwd.as_deref(), Some("/"));

  let (mut attachment, attached) =
    attach_session(&socket_path, &session.session_id, None, true, false).await?;
  let ServerMessage::Attached { shell_state, .. } = attached else {
    return Err("expected native state on reattach".into());
  };
  assert_eq!(shell_state.process, running.process);
  write_frame(&mut attachment, &ClientMessage::Input { data: vec![3] }).await?;
  wait_for_shell_state_matching(&socket_path, &session.session_id, |state| {
    state
      .process
      .as_ref()
      .is_some_and(|process| process.foreground == ctmux_proto::ForegroundProcess::Shell)
  })
  .await?;
  write_frame(
    &mut attachment,
    &ClientMessage::Input {
      data: b"exit\n".to_vec(),
    },
  )
  .await?;
  wait_for_session_end(&mut attachment).await?;
  drop(attachment);
  wait_for_daemon_exit(daemon, "ctmuxd did not exit after native metadata test").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shell_awareness_uses_private_reports_and_redacts_viewer_command_text() -> TestResult {
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("ctmux.sock");
  let daemon = spawn_daemon(&socket_path, 64 * 1024, 4 * 1024);
  let session = create_shell_session(
    &socket_path,
    "shell-state",
    "printf 'ctmux-shell-v1\\000zsh\\0001\\000cwd,command_line,cursor,prompt_phase\\000/workspace/ctmux\\000editing\\0001\\000echo 日\\0006\\000' > \"$CTMUX_SHELL_STATE_PIPE\"; IFS= read -r line",
  )
  .await?;

  let inspection = wait_for_shell_state(&socket_path, &session.session_id).await?;
  assert_eq!(inspection.shell.shell_type, ctmux_proto::ShellType::Zsh);
  assert_eq!(inspection.cwd.as_deref(), Some("/workspace/ctmux"));
  assert_eq!(inspection.prompt_phase, ctmux_proto::PromptPhase::Editing);
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
  wait_for_daemon_exit(daemon, "ctmuxd did not exit after shell-awareness test").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn running_command_summaries_follow_input_lease_visibility() -> TestResult {
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("ctmux.sock");
  let daemon = spawn_daemon(&socket_path, 64 * 1024, 4 * 1024);
  let session = create_shell_session(
    &socket_path,
    "running-command-state",
    "printf 'ctmux-shell-v2\\000zsh\\0002\\000cwd,prompt_phase,running_command\\000/workspace/ctmux\\000running\\0001\\000cargo test --workspace\\000\\000' > \"$CTMUX_SHELL_STATE_PIPE\"; IFS= read -r line",
  )
  .await?;

  let inspection = wait_for_shell_state(&socket_path, &session.session_id).await?;
  assert_eq!(inspection.prompt_phase, ctmux_proto::PromptPhase::Running);
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
    "ctmuxd did not exit after running-command visibility test",
  )
  .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn alternate_screen_transitions_publish_a_tui_hint() -> TestResult {
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("ctmux.sock");
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
  assert_eq!(shell_state.tui_hint, ctmux_proto::TuiHint::Unknown);

  wait_for_tui_hint(&mut attachment, ctmux_proto::TuiHint::AlternateScreen).await?;
  wait_for_tui_hint(&mut attachment, ctmux_proto::TuiHint::Inline).await?;
  wait_for_session_end(&mut attachment).await?;
  drop(attachment);
  wait_for_daemon_exit(daemon, "ctmuxd did not exit after alternate-screen test").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn secondary_attachment_cannot_control_owned_session_but_receives_authorized_output()
-> TestResult {
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("ctmux.sock");
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

  wait_for_daemon_exit(daemon, "ctmuxd did not exit after ownership test").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn geometry_changes_are_ordered_for_viewers_and_checkpointed_on_resume() -> TestResult {
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("ctmux.sock");
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

  wait_for_daemon_exit(daemon, "ctmuxd did not exit after geometry test").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicitly_released_leases_can_be_acquired_by_another_attachment() -> TestResult {
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("ctmux.sock");
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

  // Establish a rendered boundary before exercising lease transfer and resize.
  read_output_until(&mut first_attach, b"ready").await?;
  read_output_until(&mut second_attach, b"ready").await?;

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
  // Resize can deliver a geometry checkpoint. Observe and acknowledge it on
  // both clients before the deliberate slow-reader phase at session exit.
  wait_for_geometry_change(&mut first_attach, &new_size).await?;
  wait_for_geometry_change(&mut second_attach, &new_size).await?;
  heartbeat(&mut first_attach, 1).await?;
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
  // Drain the final frames only after the daemon has closed both attachments.
  // This exercises a slow reader without depending on thread scheduling.
  wait_for_daemon_exit(daemon, "ctmuxd did not exit after release test").await?;
  let first_output = wait_for_session_end(&mut first_attach).await?;
  let second_output = wait_for_session_end(&mut second_attach).await?;
  assert!(contains_bytes(&first_output, b"authorized:from-second"));
  assert!(contains_bytes(&second_output, b"authorized:from-second"));
  drop(first_attach);
  drop(second_attach);

  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnected_attachment_releases_its_leases_for_another_attachment() -> TestResult {
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("ctmux.sock");
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
  let output = wait_for_session_end(&mut second_attach).await?;
  assert!(contains_bytes(&output, b"authorized:after-disconnect"));
  drop(second_attach);

  wait_for_daemon_exit(daemon, "ctmuxd did not exit after disconnect test").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnect_token_rebinds_the_attachment_and_preserves_both_leases() -> TestResult {
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("ctmux.sock");
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
        return Ok::<(), ctmux_proto::CodecError>(());
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
  let output = wait_for_session_end(&mut resumed).await?;
  assert!(contains_bytes(&output, b"authorized:after-resume"));
  drop(resumed);

  wait_for_daemon_exit(daemon, "ctmuxd did not exit after token-resume test").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn silent_open_attachment_expires_and_cannot_renew_its_leases_late() -> TestResult {
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("ctmux.sock");
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
  let output = wait_for_session_end(&mut contender).await?;
  assert!(contains_bytes(&output, b"authorized:after-expiry"));
  drop(stale_owner);
  drop(contender);

  wait_for_daemon_exit(daemon, "ctmuxd did not exit after silent owner expiry").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn healthy_heartbeating_attachment_retains_its_leases() -> TestResult {
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("ctmux.sock");
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

  wait_for_daemon_exit(daemon, "ctmuxd did not exit after healthy owner test").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_control_restart_ends_sessions_and_closes_existing_attachments() -> TestResult {
  let _test_guard = pty_test_lock().await;
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("ctmux.sock");
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
    "ctmuxd did not exit after cooperative restart drained every connection",
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
  let socket_path = test_directory.path.join("ctmux.sock");
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
    "ctmuxd did not cancel a stalled data connection during cooperative restart",
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
) -> tokio::task::JoinHandle<Result<(), ctmuxd::DaemonError>> {
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
) -> tokio::task::JoinHandle<Result<(), ctmuxd::DaemonError>> {
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
      eprintln!("test ctmuxd failed to start or serve: {error}");
    }
    result
  })
}

async fn create_shell_session(
  socket_path: &Path,
  name: &str,
  script: &str,
) -> TestResult<ctmux_proto::SessionInfo> {
  create_shell_session_with_name(socket_path, Some(name), script).await
}

async fn create_shell_session_with_name(
  socket_path: &Path,
  name: Option<&str>,
  script: &str,
) -> TestResult<ctmux_proto::SessionInfo> {
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

async fn attach_with_pending_checkpoint(
  socket_path: &Path,
  session: &str,
  presentation_window_bytes: u64,
) -> TestResult<(UnixStream, u64)> {
  let mut stream = connect_when_ready(socket_path).await?;
  handshake(&mut stream).await?;
  write_frame(
    &mut stream,
    &ClientMessage::AttachSession {
      session: session.into(),
      resume_from: None,
      terminal_size: TerminalSize::default(),
      request_input_lease: true,
      request_layout_lease: false,
      request_command_line: false,
      request_running_command: false,
      presentation_window_bytes,
    },
  )
  .await?;
  let ServerMessage::Attached {
    checkpoint: Some(checkpoint),
    ..
  } = required_message(&mut stream).await?
  else {
    return Err("expected an initial checkpoint".into());
  };
  Ok((stream, checkpoint.sequence))
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
      presentation_window_bytes: DEFAULT_PRESENTATION_WINDOW_BYTES,
    },
  )
  .await?;
  let attached = required_message(&mut stream).await?;
  if let ServerMessage::Attached {
    checkpoint: Some(checkpoint),
    ..
  } = &attached
  {
    write_frame(
      &mut stream,
      &ClientMessage::PresentationApplied {
        sequence: checkpoint.sequence,
      },
    )
    .await?;
  }
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
      presentation_window_bytes: DEFAULT_PRESENTATION_WINDOW_BYTES,
    },
  )
  .await?;
  let attached = required_message(&mut stream).await?;
  if let ServerMessage::Attached {
    checkpoint: Some(checkpoint),
    ..
  } = &attached
  {
    write_frame(
      &mut stream,
      &ClientMessage::PresentationApplied {
        sequence: checkpoint.sequence,
      },
    )
    .await?;
  }
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
  wait_for_shell_state_matching(socket_path, session, |state| {
    state.shell.integration_version.is_some()
  })
  .await
}

async fn wait_for_shell_state_matching(
  socket_path: &Path,
  session: &str,
  matches: impl Fn(&ShellState) -> bool,
) -> TestResult<ShellState> {
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
    if matches(&shell_state) {
      return Ok(shell_state);
    }
    if Instant::now() >= deadline {
      return Err("shell reporter did not publish state".into());
    }
    sleep(Duration::from_millis(10)).await;
  }
}

async fn wait_for_tui_hint(stream: &mut UnixStream, expected: ctmux_proto::TuiHint) -> TestResult {
  loop {
    match presented_message(stream).await? {
      ServerMessage::ShellStateChanged { state } if state.tui_hint == expected => return Ok(()),
      ServerMessage::ViewSnapshot { .. }
      | ServerMessage::ShellStateChanged { .. }
      | ServerMessage::Output { .. }
      | ServerMessage::Checkpoint { .. }
      | ServerMessage::PtyGeometryChanged { .. }
      | ServerMessage::LeaseStatus {
        notification: true, ..
      } => {}
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
    match presented_message(stream).await? {
      ServerMessage::ShellStateChanged { state }
        if state.revision > after_revision
          && !state.command_line_redacted
          && state.current_command_line.is_some() =>
      {
        return Ok(state);
      }
      ServerMessage::ViewSnapshot { .. }
      | ServerMessage::ShellStateChanged { .. }
      | ServerMessage::Output { .. }
      | ServerMessage::Checkpoint { .. }
      | ServerMessage::PtyGeometryChanged { .. }
      | ServerMessage::LeaseStatus {
        notification: true, ..
      } => {}
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
    match presented_message(stream).await? {
      ServerMessage::ShellStateChanged { state }
        if state.revision > after_revision
          && !state.running_command_redacted
          && state.running_command.is_some() =>
      {
        return Ok(state);
      }
      ServerMessage::ViewSnapshot { .. }
      | ServerMessage::ShellStateChanged { .. }
      | ServerMessage::Output { .. }
      | ServerMessage::Checkpoint { .. }
      | ServerMessage::PtyGeometryChanged { .. }
      | ServerMessage::LeaseStatus {
        notification: true, ..
      } => {}
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
    match presented_message(stream).await? {
      ServerMessage::ShellStateChanged { state }
        if state.revision > after_revision
          && state.running_command_redacted
          && state.running_command.is_none() =>
      {
        return Ok(state);
      }
      ServerMessage::ViewSnapshot { .. }
      | ServerMessage::ShellStateChanged { .. }
      | ServerMessage::Output { .. }
      | ServerMessage::Checkpoint { .. }
      | ServerMessage::PtyGeometryChanged { .. }
      | ServerMessage::LeaseStatus {
        notification: true, ..
      } => {}
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

async fn wait_for_session_removal(socket_path: &Path, session_id: &str) -> TestResult {
  let deadline = Instant::now() + Duration::from_secs(3);
  loop {
    let mut stream = connect_when_ready(socket_path).await?;
    handshake(&mut stream).await?;
    write_frame(&mut stream, &ClientMessage::ListSessions).await?;
    let ServerMessage::SessionList { sessions } = required_message(&mut stream).await? else {
      return Err("expected session list while waiting for child exit".into());
    };
    if sessions
      .iter()
      .all(|session| session.session_id != session_id)
    {
      return Ok(());
    }
    if Instant::now() >= deadline {
      return Err("session did not exit".into());
    }
    sleep(Duration::from_millis(10)).await;
  }
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
    match presented_message(stream).await? {
      ServerMessage::HeartbeatAck {
        nonce: acknowledged,
      } => {
        assert_eq!(acknowledged, nonce);
        return Ok(());
      }
      ServerMessage::Output { .. }
      | ServerMessage::Checkpoint { .. }
      | ServerMessage::ViewSnapshot { .. }
      | ServerMessage::ShellStateChanged { .. }
      | ServerMessage::PtyGeometryChanged { .. }
      | ServerMessage::LeaseStatus {
        notification: true, ..
      } => {}
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
    match presented_message(stream).await? {
      ServerMessage::LeaseStatus {
        lease,
        status,
        notification: false,
      } if lease == expected_lease => return Ok(status),
      ServerMessage::LeaseStatus {
        notification: false,
        ..
      }
      | ServerMessage::Output { .. }
      | ServerMessage::Checkpoint { .. }
      | ServerMessage::ViewSnapshot { .. }
      | ServerMessage::ShellStateChanged { .. }
      | ServerMessage::PtyGeometryChanged { .. }
      | ServerMessage::LeaseStatus {
        notification: true, ..
      } => {}
      response => return Err(format!("expected lease status, received {response:?}").into()),
    }
  }
}

/// A heartbeat is a stream-order barrier: capture all ownership changes sent
/// before it, including accidental duplicate replies to the requester.
async fn layout_statuses_before_heartbeat(
  stream: &mut UnixStream,
  nonce: u64,
) -> TestResult<Vec<LeaseStatus>> {
  write_frame(stream, &ClientMessage::Heartbeat { nonce }).await?;
  let mut statuses = Vec::new();
  loop {
    match presented_message(stream).await? {
      ServerMessage::LeaseStatus {
        lease: LeaseKind::Layout,
        status,
        ..
      } => statuses.push(status),
      ServerMessage::HeartbeatAck {
        nonce: acknowledged,
      } if acknowledged == nonce => {
        return Ok(statuses);
      }
      ServerMessage::Output { .. }
      | ServerMessage::Checkpoint { .. }
      | ServerMessage::ViewSnapshot { .. }
      | ServerMessage::ShellStateChanged { .. }
      | ServerMessage::PtyGeometryChanged { .. }
      | ServerMessage::LeaseStatus {
        notification: true, ..
      } => {}
      message => {
        return Err(format!("expected layout notification or barrier, got {message:?}").into());
      }
    }
  }
}

async fn wait_for_layout_status(stream: &mut UnixStream, held: bool, owned: bool) -> TestResult {
  loop {
    if let ServerMessage::LeaseStatus {
      lease: LeaseKind::Layout,
      status,
      notification,
    } = presented_message(stream).await?
    {
      assert!(
        notification,
        "observer updates must be marked as notifications"
      );
      assert_lease_status(&status, held, owned);
      return Ok(());
    }
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn layout_lease_notifications_reach_observers_without_duplicate_replies() -> TestResult {
  let _guard = pty_test_lock().await;
  let directory = TestDirectory::new();
  let socket = directory.path.join("ctmux.sock");
  let daemon = spawn_daemon(&socket, 64 * 1024, 4 * 1024);
  let session = create_shell_session(&socket, "lease-watch", "IFS= read -r line").await?;
  let (mut observer, _) = attach_session(&socket, &session.session_id, None, false, false).await?;
  assert_eq!(
    layout_statuses_before_heartbeat(&mut observer, 1).await?,
    Vec::<LeaseStatus>::new()
  );
  let (mut owner, _) = attach_session(&socket, &session.session_id, None, true, true).await?;
  wait_for_layout_status(&mut observer, true, false).await?;
  assert_eq!(
    layout_statuses_before_heartbeat(&mut owner, 2).await?,
    Vec::<LeaseStatus>::new()
  );

  write_frame(
    &mut owner,
    &ClientMessage::ReleaseLease {
      lease: LeaseKind::Layout,
    },
  )
  .await?;
  let replies = layout_statuses_before_heartbeat(&mut owner, 3).await?;
  assert_eq!(
    replies.len(),
    1,
    "release must have exactly one requester reply"
  );
  assert_lease_status(&replies[0], false, false);
  wait_for_layout_status(&mut observer, false, false).await?;
  assert_eq!(
    layout_statuses_before_heartbeat(&mut observer, 4).await?,
    Vec::<LeaseStatus>::new()
  );

  write_frame(
    &mut owner,
    &ClientMessage::AcquireLease {
      lease: LeaseKind::Layout,
    },
  )
  .await?;
  let replies = layout_statuses_before_heartbeat(&mut owner, 5).await?;
  assert_eq!(
    replies.len(),
    1,
    "acquisition must have exactly one requester reply"
  );
  assert_lease_status(&replies[0], true, true);
  wait_for_layout_status(&mut observer, true, false).await?;
  assert!(
    acquire_lease(&mut owner, LeaseKind::Layout)
      .await?
      .owned_by_client
  );
  assert_eq!(
    layout_statuses_before_heartbeat(&mut owner, 6).await?,
    Vec::<LeaseStatus>::new()
  );
  assert_eq!(
    layout_statuses_before_heartbeat(&mut observer, 7).await?,
    Vec::<LeaseStatus>::new()
  );

  release_lease(&mut owner, LeaseKind::Layout).await?;
  timeout(Duration::from_secs(3), observer.readable()).await??;
  // A release notification is already buffered when this observer requests
  // ownership. It must never be mistaken for the following acquisition reply.
  write_frame(
    &mut observer,
    &ClientMessage::AcquireLease {
      lease: LeaseKind::Layout,
    },
  )
  .await?;
  wait_for_layout_status(&mut observer, false, false).await?;
  let acquired = lease_status_response(&mut observer, LeaseKind::Layout).await?;
  assert_lease_status(&acquired, true, true);
  wait_for_layout_status(&mut owner, true, false).await?;
  write_frame(&mut owner, &ClientMessage::Detach).await?;
  wait_for_detached(&mut owner).await?;
  assert_eq!(
    layout_statuses_before_heartbeat(&mut observer, 8).await?,
    Vec::<LeaseStatus>::new()
  );
  kill_shell_session(&socket, &session.session_id).await?;
  drop(owner);
  drop(observer);
  wait_for_daemon_exit(daemon, "layout watch daemon did not exit").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn historical_layout_lease_peers_keep_response_only_delivery() -> TestResult {
  let _guard = pty_test_lock().await;
  let directory = TestDirectory::new();
  let socket = directory.path.join("ctmux.sock");
  let daemon = spawn_daemon(&socket, 64 * 1024, 4 * 1024);
  let session = create_shell_session(&socket, "lease-compat", "IFS= read -r line").await?;
  let (mut owner, _) = attach_session(&socket, &session.session_id, None, true, true).await?;
  for contract in [
    ctmux_proto::CONTRACT_V1_0_13,
    ctmux_proto::CONTRACT_V1_1_14,
    ctmux_proto::CONTRACT_V1_1_15,
  ] {
    let mut old = historical_connection(&socket, contract).await?;
    write_frame(
      &mut old,
      &ClientMessage::AttachSession {
        session: session.session_id.clone(),
        resume_from: None,
        terminal_size: TerminalSize::default(),
        request_input_lease: false,
        request_layout_lease: false,
        request_command_line: false,
        request_running_command: false,
        presentation_window_bytes: DEFAULT_PRESENTATION_WINDOW_BYTES,
      },
    )
    .await?;
    let ServerMessage::Attached { checkpoint, .. } = required_message(&mut old).await? else {
      panic!("historical attachment expected");
    };
    if let Some(checkpoint) = checkpoint {
      acknowledge_output(&mut old, checkpoint.sequence).await?;
    }
    release_lease(&mut owner, LeaseKind::Layout).await?;
    assert_eq!(
      layout_statuses_before_heartbeat(&mut old, 16).await?,
      Vec::<LeaseStatus>::new()
    );
    acquire_lease(&mut owner, LeaseKind::Layout).await?;
    assert_eq!(
      layout_statuses_before_heartbeat(&mut old, 17).await?,
      Vec::<LeaseStatus>::new()
    );
    let status = acquire_lease(&mut old, LeaseKind::Layout).await?;
    assert_lease_status(&status, true, false);
    write_frame(&mut old, &ClientMessage::Detach).await?;
    wait_for_detached(&mut old).await?;
  }
  kill_shell_session(&socket, &session.session_id).await?;
  drop(owner);
  wait_for_daemon_exit(daemon, "historical lease daemon did not exit").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn layout_lease_notifications_refresh_transferred_attachments() -> TestResult {
  let _guard = pty_test_lock().await;
  let directory = TestDirectory::new();
  let socket = directory.path.join("ctmux.sock");
  let daemon = spawn_daemon(&socket, 64 * 1024, 4 * 1024);
  let source = create_shell_session(&socket, "lease-source", "IFS= read -r line").await?;
  let split = split_topology_shell(&socket, &source).await?;
  let (mut moved_owner, _) = attach_session(&socket, &source.terminal_id, None, true, true).await?;
  let (mut source_observer, _) =
    attach_session(&socket, &split.terminals[1].terminal_id, None, false, false).await?;
  let ServerMessage::ViewSnapshot { view: promoted } = topology_request(
    &socket,
    ClientMessage::PromoteTerminal {
      terminal_id: source.terminal_id.clone(),
      name: Some("lease-promoted".into()),
    },
  )
  .await?
  else {
    panic!("promoted view expected");
  };
  wait_for_layout_status(&mut moved_owner, false, false).await?;
  wait_for_layout_status(&mut source_observer, false, false).await?;
  assert!(
    acquire_lease(&mut moved_owner, LeaseKind::Layout)
      .await?
      .owned_by_client
  );

  let destination = create_shell_session(&socket, "lease-destination", "IFS= read -r line").await?;
  let (mut destination_owner, _) =
    attach_session(&socket, &destination.session_id, None, true, true).await?;
  topology_request(
    &socket,
    ClientMessage::MergeSessions {
      source: promoted.session_id,
      destination: destination.session_id.clone(),
    },
  )
  .await?;
  wait_for_layout_status(&mut moved_owner, true, false).await?;
  assert_eq!(
    layout_statuses_before_heartbeat(&mut destination_owner, 9).await?,
    Vec::<LeaseStatus>::new()
  );
  assert_eq!(
    layout_statuses_before_heartbeat(&mut source_observer, 10).await?,
    Vec::<LeaseStatus>::new()
  );
  assert!(
    !acquire_lease(&mut moved_owner, LeaseKind::Layout)
      .await?
      .owned_by_client
  );
  write_frame(&mut destination_owner, &ClientMessage::Detach).await?;
  wait_for_detached(&mut destination_owner).await?;
  wait_for_layout_status(&mut moved_owner, false, false).await?;
  assert!(
    acquire_lease(&mut moved_owner, LeaseKind::Layout)
      .await?
      .owned_by_client
  );
  kill_shell_session(&socket, &source.session_id).await?;
  kill_shell_session(&socket, &destination.session_id).await?;
  drop(moved_owner);
  drop(source_observer);
  drop(destination_owner);
  wait_for_daemon_exit(daemon, "transferred lease daemon did not exit").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn layout_lease_expiry_notifies_waiting_observer_without_polling() -> TestResult {
  let _guard = pty_test_lock().await;
  let directory = TestDirectory::new();
  let socket = directory.path.join("ctmux.sock");
  let daemon = spawn_daemon_with_liveness(&socket, 64 * 1024, 4 * 1024, Duration::from_millis(300));
  let session = create_shell_session(&socket, "lease-expiry", "IFS= read -r line").await?;
  let (owner, _) = attach_session(&socket, &session.session_id, None, true, true).await?;
  let (mut observer, _) = attach_session(&socket, &session.session_id, None, false, false).await?;
  drop(owner);
  // Keep only the observer alive. It sends no lease query or acquisition.
  heartbeat(&mut observer, 11).await?;
  // The read and write halves keep the same transport while expiry publishes
  // its unsolicited ownership update.
  let (mut reader, mut writer) = observer.into_split();
  let renewal = async {
    for nonce in 12..=15 {
      sleep(Duration::from_millis(100)).await;
      write_frame(&mut writer, &ClientMessage::Heartbeat { nonce }).await?;
    }
    Ok::<_, Box<dyn Error + Send + Sync>>(())
  };
  let receive = async {
    loop {
      let Some(message) = read_frame(&mut reader).await? else {
        return Err("observer disconnected before layout lease expired".into());
      };
      if let ServerMessage::LeaseStatus {
        lease: LeaseKind::Layout,
        status,
        notification,
      } = message
      {
        assert!(notification);
        assert_lease_status(&status, false, false);
        return Ok::<_, Box<dyn Error + Send + Sync>>(());
      }
    }
  };
  let (renewed, received) = tokio::join!(renewal, timeout(Duration::from_secs(2), receive));
  renewed?;
  received??;
  kill_shell_session(&socket, &session.session_id).await?;
  drop(reader);
  drop(writer);
  wait_for_daemon_exit(daemon, "lease expiry daemon did not exit").await
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
    match presented_message(stream).await? {
      ServerMessage::Error { code, .. } => {
        assert_eq!(code, expected_code);
        return Ok(());
      }
      ServerMessage::Output { .. }
      | ServerMessage::Checkpoint { .. }
      | ServerMessage::ViewSnapshot { .. }
      | ServerMessage::ShellStateChanged { .. }
      | ServerMessage::PtyGeometryChanged { .. }
      | ServerMessage::LeaseStatus {
        notification: true, ..
      } => {}
      response => return Err(format!("expected error response, received {response:?}").into()),
    }
  }
}

async fn wait_for_session_end(stream: &mut UnixStream) -> TestResult<Vec<u8>> {
  // Keep returning presentation credit while draining. The final frames may
  // already be buffered after ctmuxd closes; late acknowledgements are harmless.
  let mut output = Vec::new();
  loop {
    match presented_message(stream).await? {
      ServerMessage::SessionEnded { .. } => return Ok(output),
      ServerMessage::Output { data, .. } => output.extend(data),
      ServerMessage::Checkpoint { checkpoint, .. } => output = checkpoint.payload,
      ServerMessage::ViewSnapshot { .. }
      | ServerMessage::ShellStateChanged { .. }
      | ServerMessage::PtyGeometryChanged { .. }
      | ServerMessage::LeaseStatus {
        notification: true, ..
      } => {}
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
      .map_err(|_| "timed out waiting for ctmuxd to end or close the attachment")??;
    let Some(message) = message else {
      return Ok(());
    };
    match message {
      ServerMessage::SessionEnded { .. } => return Ok(()),
      ServerMessage::Output { .. }
      | ServerMessage::Checkpoint { .. }
      | ServerMessage::ViewSnapshot { .. }
      | ServerMessage::ShellStateChanged { .. }
      | ServerMessage::PtyGeometryChanged { .. }
      | ServerMessage::LeaseStatus {
        notification: true, ..
      } => {}
      response => {
        return Err(
          format!("expected output, session end, or close, received {response:?}").into(),
        );
      }
    }
  }
}

async fn wait_for_daemon_exit(
  daemon: tokio::task::JoinHandle<Result<(), ctmuxd::DaemonError>>,
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
      protocol: ctmux_proto::protocol_offer(),
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

async fn raw_message(stream: &mut UnixStream) -> TestResult<ServerMessage> {
  timeout(Duration::from_secs(3), read_frame(stream))
    .await
    .map_err(|_| "timed out waiting for ctmuxd")??
    .ok_or_else(|| "ctmuxd closed the connection unexpectedly".into())
}

async fn required_message(stream: &mut UnixStream) -> TestResult<ServerMessage> {
  loop {
    let message = raw_message(stream).await?;
    match &message {
      ServerMessage::Attached {
        history_manifest: Some(manifest),
        ..
      }
      | ServerMessage::Checkpoint {
        history_manifest: Some(manifest),
        ..
      } if manifest.total_bytes > 0 => {
        write_if_connected(
          stream,
          &ClientMessage::HistoryRequest {
            snapshot_id: manifest.snapshot_id.clone(),
            offset: 0,
            max_bytes: ctmux_proto::MAX_HISTORY_PAGE_BYTES as u64,
          },
        )
        .await?;
      }
      ServerMessage::HistoryPage {
        snapshot_id,
        next_offset,
        ..
      } => {
        if let Some(offset) = next_offset {
          write_if_connected(
            stream,
            &ClientMessage::HistoryRequest {
              snapshot_id: snapshot_id.clone(),
              offset: *offset,
              max_bytes: ctmux_proto::MAX_HISTORY_PAGE_BYTES as u64,
            },
          )
          .await?;
        }
        continue;
      }
      _ => {}
    }
    return Ok(message);
  }
}

// Helpers that consume presentation frames behave like an active renderer.
// Tests that intentionally withhold credit use required_message directly.
async fn presented_message(stream: &mut UnixStream) -> TestResult<ServerMessage> {
  let message = required_message(stream).await?;
  match &message {
    ServerMessage::Output { sequence_end, .. } => acknowledge_output(stream, *sequence_end).await?,
    ServerMessage::Checkpoint { checkpoint, .. } => {
      acknowledge_output(stream, checkpoint.sequence).await?;
    }
    _ => {}
  }
  Ok(message)
}

// The daemon may close after queuing final output and SessionEnded. A late
// acknowledgement must not stop us from draining those buffered messages.
async fn acknowledge_output(stream: &mut UnixStream, sequence: u64) -> TestResult {
  write_if_connected(stream, &ClientMessage::PresentationApplied { sequence }).await
}

async fn write_if_connected(stream: &mut UnixStream, message: &ClientMessage) -> TestResult {
  match write_frame(stream, message).await {
    Ok(()) => Ok(()),
    Err(ctmux_proto::CodecError::Io(error))
      if matches!(
        error.kind(),
        std::io::ErrorKind::BrokenPipe
          | std::io::ErrorKind::ConnectionReset
          | std::io::ErrorKind::NotConnected
      ) =>
    {
      Ok(())
    }
    Err(error) => Err(error.into()),
  }
}

#[tokio::test]
async fn final_output_is_drained_when_presentation_acknowledgement_finds_a_closed_peer()
-> TestResult {
  let (mut client, mut server) = UnixStream::pair()?;
  write_frame(
    &mut server,
    &ServerMessage::Output {
      sequence_start: 0,
      sequence_end: 5,
      data: b"final".to_vec(),
    },
  )
  .await?;
  write_frame(
    &mut server,
    &ServerMessage::SessionEnded {
      session_id: "finished".into(),
      exit_code: Some(0),
    },
  )
  .await?;
  drop(server);

  let (output, sequence) = read_output_until(&mut client, b"final").await?;
  assert_eq!(output, b"final");
  assert_eq!(sequence, 5);
  wait_for_session_end(&mut client).await?;
  Ok(())
}

#[tokio::test]
async fn checkpoint_output_waits_for_remaining_marker_bytes() -> TestResult {
  let (mut client, mut server) = UnixStream::pair()?;
  write_frame(
    &mut server,
    &ServerMessage::Output {
      sequence_start: 11,
      sequence_end: 16,
      data: b"ready".to_vec(),
    },
  )
  .await?;
  let (output, sequence) = read_output_until_from(
    &mut client,
    b"checkpoint-ready",
    b"checkpoint-".to_vec(),
    11,
  )
  .await?;
  assert_eq!(output, b"checkpoint-ready");
  assert_eq!(sequence, 16);
  assert!(matches!(
    timeout(
      Duration::from_secs(3),
      read_frame::<_, ClientMessage>(&mut server)
    )
    .await??,
    Some(ClientMessage::PresentationApplied { sequence: 16 })
  ));
  Ok(())
}

async fn read_output_until(stream: &mut UnixStream, expected: &[u8]) -> TestResult<(Vec<u8>, u64)> {
  // The initial screen may already contain the marker in Attached. Request
  // its authoritative current presentation instead of relying on the shell
  // racing the initial attachment with another raw output frame.
  write_if_connected(stream, &ClientMessage::RequestCheckpoint).await?;
  read_output_until_from(stream, expected, Vec::new(), 0).await
}

async fn read_output_until_from(
  stream: &mut UnixStream,
  expected: &[u8],
  mut output: Vec<u8>,
  initial_sequence: u64,
) -> TestResult<(Vec<u8>, u64)> {
  if contains_bytes(&output, expected) {
    return Ok((output, initial_sequence));
  }
  loop {
    match required_message(stream).await? {
      ServerMessage::Output {
        sequence_end, data, ..
      } => {
        output.extend(data);
        acknowledge_output(stream, sequence_end).await?;
        if contains_bytes(&output, expected) {
          return Ok((output, sequence_end));
        }
      }
      ServerMessage::Checkpoint { checkpoint, .. } => {
        output = checkpoint.payload.clone();
        acknowledge_output(stream, checkpoint.sequence).await?;
        if contains_bytes(&output, expected) {
          return Ok((output, checkpoint.sequence));
        }
      }
      ServerMessage::ViewSnapshot { .. }
      | ServerMessage::ShellStateChanged { .. }
      | ServerMessage::PtyGeometryChanged { .. }
      | ServerMessage::LeaseStatus {
        notification: true, ..
      } => {}
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
    match presented_message(stream).await? {
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
      ServerMessage::ViewSnapshot { .. }
      | ServerMessage::ShellStateChanged { .. }
      | ServerMessage::Checkpoint { .. }
      | ServerMessage::PtyGeometryChanged { .. }
      | ServerMessage::LeaseStatus {
        notification: true, ..
      } => {}
      message => return Err(format!("expected output, received {message:?}").into()),
    }
  }
}

async fn wait_for_geometry_change(
  stream: &mut UnixStream,
  expected_size: &TerminalSize,
) -> TestResult<u64> {
  loop {
    match presented_message(stream).await? {
      ServerMessage::PtyGeometryChanged {
        terminal_size,
        observed_sequence,
      } => {
        assert_eq!(&terminal_size, expected_size);
        return Ok(observed_sequence);
      }
      ServerMessage::Checkpoint { checkpoint, .. }
        if checkpoint.terminal_size == *expected_size =>
      {
        return Ok(checkpoint.sequence);
      }
      ServerMessage::Output { .. }
      | ServerMessage::ViewSnapshot { .. }
      | ServerMessage::ShellStateChanged { .. }
      | ServerMessage::Checkpoint { .. }
      | ServerMessage::LeaseStatus {
        notification: true, ..
      } => {}
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
        "ctmux-t-{}",
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

async fn topology_request(socket: &Path, message: ClientMessage) -> TestResult<ServerMessage> {
  let mut stream = connect_when_ready(socket).await?;
  handshake(&mut stream).await?;
  write_frame(&mut stream, &message).await?;
  required_message(&mut stream).await
}

async fn topology_view(socket: &Path, session: &str) -> TestResult<ctmux_proto::ViewInfo> {
  match topology_request(
    socket,
    ClientMessage::GetView {
      session: session.into(),
    },
  )
  .await?
  {
    ServerMessage::ViewSnapshot { view } => Ok(view),
    other => Err(format!("expected view, got {other:?}").into()),
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn view_membership_moves_preserve_terminal_identity_and_live_attachments() -> TestResult {
  let _guard = pty_test_lock().await;
  let directory = TestDirectory::new();
  let socket = directory.path.join("ctmux.sock");
  let daemon = spawn_daemon(&socket, 64 * 1024, 4 * 1024);
  let root = create_shell_session(
    &socket,
    "root",
    "while IFS= read -r line; do printf 'root:%s\\n' \"$line\"; done",
  )
  .await?;
  assert_ne!(root.session_id, root.terminal_id);
  assert_ne!(root.view_id, root.session_id);
  let view = split_topology_shell(&socket, &root).await?;
  assert_eq!(view.terminals.len(), 2);
  let child_id = view.terminals[1].terminal_id.clone();
  assert_eq!(view.session_id, root.session_id);
  let listed = topology_request(&socket, ClientMessage::ListSessions).await?;
  assert!(
    matches!(listed, ServerMessage::SessionList { sessions } if sessions.len() == 1 && sessions[0].session_id == root.session_id)
  );

  assert_view_layout_updates(&socket, &root, &view, &child_id).await?;

  let (mut attached, _) = attach_session(&socket, &child_id, None, true, false).await?;
  let promoted = topology_request(
    &socket,
    ClientMessage::PromoteTerminal {
      terminal_id: child_id.clone(),
      name: Some("promoted".into()),
    },
  )
  .await?;
  let ServerMessage::ViewSnapshot { view: promoted } = promoted else {
    panic!("expected promoted view");
  };
  assert_eq!(promoted.terminals[0].terminal_id, child_id);
  assert_eq!(
    topology_view(&socket, &root.session_id)
      .await?
      .terminals
      .len(),
    1
  );
  write_frame(
    &mut attached,
    &ClientMessage::Input {
      data: b"moved\n".to_vec(),
    },
  )
  .await?;
  let (output, _) = read_output_until(&mut attached, b"child:moved").await?;
  assert!(contains_bytes(&output, b"child:moved"));

  let merged = topology_request(
    &socket,
    ClientMessage::MergeSessions {
      source: promoted.session_id.clone(),
      destination: root.session_id.clone(),
    },
  )
  .await?;
  assert_merged_split_geometry(merged);
  assert!(matches!(
    topology_request(
      &socket,
      ClientMessage::GetView {
        session: promoted.session_id
      }
    )
    .await?,
    ServerMessage::Error {
      code: ErrorCode::SessionNotFound,
      ..
    }
  ));

  assert_eq!(
    topology_request(
      &socket,
      ClientMessage::KillTerminal {
        terminal_id: child_id
      }
    )
    .await?,
    ServerMessage::Success
  );
  wait_for_session_end(&mut attached).await?;
  wait_for_single_terminal(&socket, &root.session_id).await?;
  assert_root_termination(&socket, &root).await?;
  timeout(Duration::from_secs(3), daemon).await???;
  Ok(())
}

fn assert_merged_split_geometry(merged: ServerMessage) {
  let ServerMessage::ViewSnapshot { view: merged } = merged else {
    panic!("expected merged view");
  };
  assert_eq!(merged.terminals.len(), 2);
  assert!(matches!(
    merged.layout,
    ctmux_proto::ViewLayout::Split {
      axis: ctmux_proto::SplitAxis::Horizontal,
      ..
    }
  ));
  assert_eq!(merged.panes.len(), 2);
  assert_eq!(
    merged.panes[0].left + merged.panes[0].columns + 1,
    merged.panes[1].left
  );
  assert_eq!(
    merged.panes[1].left + merged.panes[1].columns,
    merged.canvas_size.columns
  );
  for pane in &merged.panes {
    let terminal = merged
      .terminals
      .iter()
      .find(|terminal| terminal.terminal_id == pane.terminal_id)
      .unwrap();
    assert_eq!(terminal.terminal_size.columns, pane.columns);
    assert_eq!(terminal.terminal_size.rows, pane.rows);
  }
}

async fn assert_view_layout_updates(
  socket: &Path,
  root: &SessionInfo,
  view: &ctmux_proto::ViewInfo,
  child_id: &str,
) -> TestResult {
  let invalid = topology_request(
    socket,
    ClientMessage::UpdateView {
      session: root.session_id.clone(),
      expected_revision: view.revision,
      layout: ctmux_proto::ViewLayout::Terminal {
        terminal_id: child_id.to_owned(),
      },
    },
  )
  .await?;
  assert!(matches!(
    invalid,
    ServerMessage::Error {
      code: ErrorCode::InvalidRequest,
      ..
    }
  ));
  let stale = topology_request(
    socket,
    ClientMessage::UpdateView {
      session: root.session_id.clone(),
      expected_revision: 0,
      layout: view.layout.clone(),
    },
  )
  .await?;
  assert!(matches!(
    stale,
    ServerMessage::Error {
      code: ErrorCode::InvalidRequest,
      ..
    }
  ));

  let duplicate = topology_request(
    socket,
    ClientMessage::UpdateView {
      session: root.session_id.clone(),
      expected_revision: view.revision,
      layout: ctmux_proto::ViewLayout::Split {
        axis: ctmux_proto::SplitAxis::Horizontal,
        weights: Vec::new(),
        children: vec![
          ctmux_proto::ViewLayout::Terminal {
            terminal_id: child_id.to_owned(),
          },
          ctmux_proto::ViewLayout::Terminal {
            terminal_id: child_id.to_owned(),
          },
        ],
      },
    },
  )
  .await?;
  assert!(matches!(
    duplicate,
    ServerMessage::Error {
      code: ErrorCode::InvalidRequest,
      ..
    }
  ));
  let reordered = topology_request(
    socket,
    ClientMessage::UpdateView {
      session: root.session_id.clone(),
      expected_revision: view.revision,
      layout: ctmux_proto::ViewLayout::Split {
        axis: ctmux_proto::SplitAxis::Vertical,
        weights: Vec::new(),
        children: vec![
          ctmux_proto::ViewLayout::Terminal {
            terminal_id: child_id.to_owned(),
          },
          ctmux_proto::ViewLayout::Terminal {
            terminal_id: root.terminal_id.clone(),
          },
        ],
      },
    },
  )
  .await?;
  assert!(
    matches!(reordered, ServerMessage::ViewSnapshot { view: updated } if updated.revision == view.revision + 1)
  );
  let listed = topology_request(socket, ClientMessage::ListSessions).await?;
  assert!(
    matches!(listed, ServerMessage::SessionList { sessions } if sessions[0].terminal_id == child_id && sessions[0].created_at_ms == root.created_at_ms && sessions[0].name == root.name)
  );

  Ok(())
}

async fn assert_root_termination(socket: &Path, root: &SessionInfo) -> TestResult {
  // Killing the root terminates every member, not just the first layout leaf.
  let split_again = topology_request(
    socket,
    ClientMessage::SplitTerminal {
      terminal_id: root.terminal_id.clone(),
      axis: ctmux_proto::SplitAxis::Vertical,
      command: Some(CommandSpec {
        program: "/bin/sh".into(),
        arguments: vec!["-c".into(), "IFS= read -r line".into()],
      }),
      working_directory: None,
      terminal_size: TerminalSize::default(),
    },
  )
  .await?;
  let ServerMessage::ViewSnapshot { view: split_again } = split_again else {
    panic!("expected view");
  };
  let other = &split_again.terminals[1].terminal_id;
  let (mut root_attachment, _) =
    attach_session(socket, &root.terminal_id, None, false, false).await?;
  let (mut other_attachment, _) = attach_session(socket, other, None, false, false).await?;
  kill_shell_session(socket, &root.session_id).await?;
  wait_for_session_end(&mut root_attachment).await?;
  wait_for_session_end(&mut other_attachment).await?;
  Ok(())
}

async fn split_topology_shell(
  socket: &Path,
  root: &SessionInfo,
) -> TestResult<ctmux_proto::ViewInfo> {
  let response = topology_request(
    socket,
    ClientMessage::SplitTerminal {
      terminal_id: root.terminal_id.clone(),
      axis: ctmux_proto::SplitAxis::Horizontal,
      command: Some(CommandSpec {
        program: "/bin/sh".into(),
        arguments: vec![
          "-c".into(),
          "while IFS= read -r line; do printf 'child:%s\\n' \"$line\"; done".into(),
        ],
      }),
      working_directory: None,
      terminal_size: TerminalSize::default(),
    },
  )
  .await?;
  let ServerMessage::ViewSnapshot { view } = response else {
    panic!("expected split view");
  };
  Ok(view)
}

async fn wait_for_single_terminal(socket: &Path, session_id: &str) -> TestResult {
  timeout(Duration::from_secs(3), async {
    loop {
      if topology_view(socket, session_id).await?.terminals.len() == 1 {
        return Ok::<_, Box<dyn Error + Send + Sync>>(());
      }
      sleep(Duration::from_millis(10)).await;
    }
  })
  .await??;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn view_resize_lease_spans_panes_and_resizes_the_whole_canvas() -> TestResult {
  let _guard = pty_test_lock().await;
  let directory = TestDirectory::new();
  let socket = directory.path.join("ctmux.sock");
  let daemon = spawn_daemon(&socket, 64 * 1024, 4 * 1024);
  let root = create_shell_session(
    &socket,
    "canvas",
    "while IFS= read -r line; do printf '%s\\n' \"$line\"; done",
  )
  .await?;
  let view = split_topology_shell(&socket, &root).await?;
  assert_eq!(view.panes[0].rows, view.panes[1].rows);
  assert_eq!(
    view.panes[0].columns + 1 + view.panes[1].columns,
    view.canvas_size.columns
  );
  let (mut first, first_info) =
    attach_session(&socket, &root.terminal_id, None, true, true).await?;
  let (mut second, second_info) =
    attach_session(&socket, &view.terminals[1].terminal_id, None, true, true).await?;
  assert!(matches!(
    first_info,
    ServerMessage::Attached {
      layout_lease: LeaseStatus {
        owned_by_client: true,
        ..
      },
      ..
    }
  ));
  assert!(matches!(
    second_info,
    ServerMessage::Attached {
      layout_lease: LeaseStatus {
        held: true,
        owned_by_client: false
      },
      input_lease: LeaseStatus {
        owned_by_client: true,
        ..
      },
      ..
    }
  ));
  write_frame(
    &mut second,
    &ClientMessage::Resize {
      terminal_size: terminal_size(100, 40),
    },
  )
  .await?;
  expect_error(&mut second, ErrorCode::LayoutLeaseRequired).await?;
  write_frame(
    &mut first,
    &ClientMessage::Resize {
      terminal_size: terminal_size(100, 40),
    },
  )
  .await?;
  wait_for_geometry_change(&mut first, &terminal_size(50, 40)).await?;
  wait_for_geometry_change(&mut second, &terminal_size(49, 40)).await?;
  let resized = topology_view(&socket, &root.session_id).await?;
  assert_eq!(resized.canvas_size, terminal_size(100, 40));
  for pane in &resized.panes {
    let terminal = resized
      .terminals
      .iter()
      .find(|entry| entry.terminal_id == pane.terminal_id)
      .unwrap();
    assert_eq!(
      terminal.terminal_size,
      terminal_size(pane.columns, pane.rows)
    );
  }
  release_lease(&mut first, LeaseKind::Layout).await?;
  assert_lease_status(
    &acquire_lease(&mut second, LeaseKind::Layout).await?,
    true,
    true,
  );
  write_frame(
    &mut second,
    &ClientMessage::Resize {
      terminal_size: terminal_size(120, 32),
    },
  )
  .await?;
  wait_for_geometry_change(&mut first, &terminal_size(60, 32)).await?;
  wait_for_geometry_change(&mut second, &terminal_size(59, 32)).await?;
  assert_minimum_canvas(&socket, &root.session_id, &mut first, &mut second).await?;
  kill_shell_session(&socket, &root.session_id).await?;
  drop(first);
  drop(second);
  wait_for_daemon_exit(daemon, "canvas daemon did not exit").await
}

async fn assert_minimum_canvas(
  socket: &Path,
  session_id: &str,
  first: &mut UnixStream,
  second: &mut UnixStream,
) -> TestResult {
  write_frame(
    second,
    &ClientMessage::Resize {
      terminal_size: terminal_size(2, 1),
    },
  )
  .await?;
  wait_for_geometry_change(first, &terminal_size(2, 1)).await?;
  wait_for_geometry_change(second, &terminal_size(2, 1)).await?;
  assert_eq!(
    topology_view(socket, session_id).await?.canvas_size,
    terminal_size(5, 1)
  );
  Ok(())
}

async fn resize_pane_result(
  stream: &mut UnixStream,
  terminal_id: &str,
  direction: ctmux_proto::ResizeDirection,
  amount: u16,
) -> TestResult<ctmux_proto::PaneResizeOutcome> {
  let request_id = Uuid::new_v4().to_string();
  resize_pane_with_id(stream, terminal_id, direction, amount, request_id).await
}

async fn resize_pane_with_id(
  stream: &mut UnixStream,
  terminal_id: &str,
  direction: ctmux_proto::ResizeDirection,
  amount: u16,
  request_id: String,
) -> TestResult<ctmux_proto::PaneResizeOutcome> {
  write_frame(
    stream,
    &ClientMessage::ResizePane {
      request_id: request_id.clone(),
      terminal_id: terminal_id.into(),
      direction,
      amount,
    },
  )
  .await?;
  wait_for_resize_result(stream, &request_id).await
}

async fn wait_for_resize_result(
  stream: &mut UnixStream,
  request_id: &str,
) -> TestResult<ctmux_proto::PaneResizeOutcome> {
  loop {
    match presented_message(stream).await? {
      ServerMessage::PaneResizeResult {
        request_id: received,
        outcome,
      } => {
        assert_eq!(received, request_id);
        return Ok(outcome);
      }
      ServerMessage::ViewSnapshot { .. }
      | ServerMessage::Checkpoint { .. }
      | ServerMessage::ShellStateChanged { .. }
      | ServerMessage::Output { .. } => {}
      other => return Err(format!("expected correlated pane resize result, got {other:?}").into()),
    }
  }
}

async fn resize_divider_result(
  stream: &mut UnixStream,
  view: &ctmux_proto::ViewInfo,
  split_path: &[u16],
  boundary: u16,
  position: u16,
) -> TestResult<ctmux_proto::PaneResizeOutcome> {
  let request_id = Uuid::new_v4().to_string();
  write_frame(
    stream,
    &ClientMessage::ResizeDivider {
      request_id: request_id.clone(),
      divider: ctmux_proto::DividerResize {
        view_id: view.view_id.clone(),
        expected_revision: view.revision,
        split_path: split_path.to_vec(),
        boundary,
        position,
      },
    },
  )
  .await?;
  wait_for_resize_result(stream, &request_id).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn divider_drag_moves_exact_outer_split_and_preserves_zoom_on_noop_or_error() -> TestResult {
  let _guard = pty_test_lock().await;
  let directory = TestDirectory::new();
  let socket = directory.path.join("ctmux.sock");
  let daemon = spawn_daemon(&socket, 64 * 1024, 4 * 1024);
  let root = create_shell_session(
    &socket,
    "mouse-divider",
    "while IFS= read -r line; do printf '%s\\n' \"$line\"; done",
  )
  .await?;
  let first_split = split_topology_shell(&socket, &root).await?;
  let outer_second = first_split.terminals[1].terminal_id.clone();
  split_topology_shell(&socket, &root).await?;
  let (mut owner, _) = attach_session(&socket, &root.terminal_id, None, true, true).await?;
  let (mut observer, _) = attach_session(&socket, &outer_second, None, true, true).await?;
  let baseline = topology_view(&socket, &root.session_id).await?;
  assert_resize_rejected(
    resize_divider_result(&mut observer, &baseline, &[], 0, 60).await?,
    &ErrorCode::LayoutLeaseRequired,
  );
  assert_eq!(topology_view(&socket, &root.session_id).await?, baseline);
  let resized = applied_resize(resize_divider_result(&mut owner, &baseline, &[], 0, 60).await?);
  assert_eq!(
    resized
      .panes
      .iter()
      .map(|pane| pane.columns)
      .collect::<Vec<_>>(),
    [30, 29, 19]
  );
  assert_eq!(resized.panes[2].left, 61);
  assert_eq!(resized.revision, baseline.revision + 1);
  assert_unzoomed_geometry(&resized);
  let shared_view = wait_for_view_zoom_at_revision(&mut observer, None, resized.revision).await?;
  assert_eq!(shared_view.layout, resized.layout);
  let zoomed = set_view_zoom(
    &socket,
    &root.session_id,
    &mut owner,
    Some(&root.terminal_id),
  )
  .await?;
  assert_invalid_divider_keeps_view(&socket, &mut owner, &zoomed).await?;
  assert_eq!(
    applied_resize(resize_divider_result(&mut owner, &zoomed, &[], 0, 60).await?),
    zoomed
  );
  let clamped = applied_resize(resize_divider_result(&mut owner, &zoomed, &[], 0, u16::MAX).await?);
  assert_eq!(clamped.panes[2].columns, 2);
  assert_unzoomed_geometry(&clamped);
  let restored = applied_resize(resize_divider_result(&mut owner, &clamped, &[], 0, 60).await?);
  assert_eq!(restored.panes, resized.panes);
  write_frame(
    &mut owner,
    &ClientMessage::Input {
      data: b"typing-after-divider-errors\n".to_vec(),
    },
  )
  .await?;
  read_output_until(&mut owner, b"typing-after-divider-errors").await?;
  kill_shell_session(&socket, &root.session_id).await?;
  drop(owner);
  drop(observer);
  wait_for_daemon_exit(daemon, "divider drag daemon did not exit").await
}

async fn assert_invalid_divider_keeps_view(
  socket: &Path,
  owner: &mut UnixStream,
  view: &ctmux_proto::ViewInfo,
) -> TestResult {
  let mut stale_revision = view.clone();
  stale_revision.revision -= 1;
  assert_resize_rejected(
    resize_divider_result(owner, &stale_revision, &[], 0, 50).await?,
    &ErrorCode::InvalidRequest,
  );
  let mut wrong_view = view.clone();
  wrong_view.view_id = "another-view".into();
  assert_resize_rejected(
    resize_divider_result(owner, &wrong_view, &[], 0, 50).await?,
    &ErrorCode::InvalidRequest,
  );
  for (path, boundary) in [
    (vec![99], 0),
    (vec![0, 0], 0),
    (vec![0; 17], 0),
    (vec![], 1),
  ] {
    assert_resize_rejected(
      resize_divider_result(owner, view, &path, boundary, 50).await?,
      &ErrorCode::InvalidRequest,
    );
  }
  assert_eq!(topology_view(socket, &view.session_id).await?, *view);
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn divider_drag_rejects_transferred_view_even_when_revision_matches() -> TestResult {
  let _guard = pty_test_lock().await;
  let directory = TestDirectory::new();
  let socket = directory.path.join("ctmux.sock");
  let daemon = spawn_daemon(&socket, 64 * 1024, 4 * 1024);
  let root = create_shell_session(&socket, "divider-move", "IFS= read -r line").await?;
  let original = topology_view(&socket, &root.session_id).await?;
  split_topology_shell(&socket, &root).await?;
  let (mut owner, _) = attach_session(&socket, &root.terminal_id, None, true, true).await?;
  let ServerMessage::ViewSnapshot { view: moved } = topology_request(
    &socket,
    ClientMessage::PromoteTerminal {
      terminal_id: root.terminal_id.clone(),
      name: Some("divider-moved".into()),
    },
  )
  .await?
  else {
    panic!("promoted view expected");
  };
  assert_ne!(original.view_id, moved.view_id);
  assert_eq!(original.revision, moved.revision);
  assert!(
    acquire_lease(&mut owner, LeaseKind::Layout)
      .await?
      .owned_by_client
  );
  assert_resize_rejected(
    resize_divider_result(&mut owner, &original, &[], 0, 70).await?,
    &ErrorCode::InvalidRequest,
  );
  assert_eq!(topology_view(&socket, &moved.session_id).await?, moved);
  kill_shell_session(&socket, &root.session_id).await?;
  kill_shell_session(&socket, &moved.session_id).await?;
  drop(owner);
  wait_for_daemon_exit(daemon, "divider move daemon did not exit").await
}

fn applied_resize(outcome: ctmux_proto::PaneResizeOutcome) -> ctmux_proto::ViewInfo {
  let ctmux_proto::PaneResizeOutcome::Applied { view } = outcome else {
    panic!("resize must succeed: {outcome:?}");
  };
  *view
}

fn assert_resize_rejected(outcome: ctmux_proto::PaneResizeOutcome, expected: &ErrorCode) {
  assert!(
    matches!(outcome, ctmux_proto::PaneResizeOutcome::Rejected { code, .. } if &code == expected)
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pane_resize_is_shared_owned_and_persistent_across_resume() -> TestResult {
  use ctmux_proto::ResizeDirection;
  let _guard = pty_test_lock().await;
  let directory = TestDirectory::new();
  let socket = directory.path.join("ctmux.sock");
  let daemon = spawn_daemon(&socket, 64 * 1024, 4 * 1024);
  let root = create_shell_session(
    &socket,
    "proportions",
    "while IFS= read -r line; do printf '%s\\n' \"$line\"; done",
  )
  .await?;
  let initial = split_topology_shell(&socket, &root).await?;
  let second_id = initial.terminals[1].terminal_id.clone();
  let (mut first, _) = attach_session(&socket, &root.terminal_id, None, true, true).await?;
  let (mut second, second_attachment) =
    attach_session(&socket, &second_id, None, true, true).await?;
  let before = topology_view(&socket, &root.session_id).await?;
  assert_resize_rejected(
    resize_pane_result(&mut second, &root.terminal_id, ResizeDirection::Right, 5).await?,
    &ErrorCode::LayoutLeaseRequired,
  );
  assert_eq!(topology_view(&socket, &root.session_id).await?, before);
  let resized = applied_resize(
    resize_pane_result(&mut first, &root.terminal_id, ResizeDirection::Right, 5).await?,
  );
  assert_eq!(
    (resized.panes[0].columns, resized.panes[1].columns),
    (45, 34)
  );
  assert_eq!(resized.revision, before.revision + 1);
  assert_unzoomed_geometry(&resized);
  let observed = wait_for_view_zoom_at_revision(&mut second, None, resized.revision).await?;
  assert_eq!(observed.layout, resized.layout);
  release_lease(&mut first, LeaseKind::Layout).await?;
  assert!(
    acquire_lease(&mut second, LeaseKind::Layout)
      .await?
      .owned_by_client
  );
  let ServerMessage::Attached {
    attachment_token, ..
  } = second_attachment
  else {
    panic!("attachment expected");
  };
  drop(second);
  let (mut second, _) = resume_attachment(&socket, &second_id, &attachment_token, None).await?;
  for _ in 0..5 {
    resize_pane_result(&mut second, &root.terminal_id, ResizeDirection::Right, 1).await?;
  }
  let repeated = topology_view(&socket, &root.session_id).await?;
  assert_eq!(
    (repeated.panes[0].columns, repeated.panes[1].columns),
    (50, 29)
  );
  assert_eq!(repeated.revision, resized.revision + 5);
  write_frame(
    &mut second,
    &ClientMessage::Resize {
      terminal_size: terminal_size(160, 24),
    },
  )
  .await?;
  let expected = repeated
    .layout
    .pane_geometry(&terminal_size(160, 24))?
    .remove(1);
  wait_for_geometry_change(&mut second, &terminal_size(expected.columns, expected.rows)).await?;
  let wider = topology_view(&socket, &root.session_id).await?;
  assert_eq!(wider.layout, repeated.layout);
  assert_unzoomed_geometry(&wider);
  kill_shell_session(&socket, &root.session_id).await?;
  drop(first);
  drop(second);
  wait_for_daemon_exit(daemon, "pane resize daemon did not exit").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pane_resize_validates_before_unzoom_and_acknowledges_unchanged_limits() -> TestResult {
  use ctmux_proto::ResizeDirection;
  let _guard = pty_test_lock().await;
  let directory = TestDirectory::new();
  let socket = directory.path.join("ctmux.sock");
  let daemon = spawn_daemon(&socket, 64 * 1024, 4 * 1024);
  let root = create_shell_session(
    &socket,
    "resize-zoom",
    "while IFS= read -r line; do printf '%s\\n' \"$line\"; done",
  )
  .await?;
  split_topology_shell(&socket, &root).await?;
  let (mut owner, _) = attach_session(&socket, &root.terminal_id, None, true, true).await?;
  let zoomed = set_view_zoom(
    &socket,
    &root.session_id,
    &mut owner,
    Some(&root.terminal_id),
  )
  .await?;
  for request_id in [String::new(), "a".repeat(257)] {
    assert_resize_rejected(
      resize_pane_with_id(
        &mut owner,
        &root.terminal_id,
        ResizeDirection::Right,
        1,
        request_id,
      )
      .await?,
      &ErrorCode::InvalidRequest,
    );
  }
  assert_resize_rejected(
    resize_pane_result(&mut owner, "absent", ResizeDirection::Right, 5).await?,
    &ErrorCode::InvalidRequest,
  );
  assert_resize_rejected(
    resize_pane_result(&mut owner, &root.terminal_id, ResizeDirection::Right, 0).await?,
    &ErrorCode::InvalidRequest,
  );
  assert_eq!(topology_view(&socket, &root.session_id).await?, zoomed);
  let unchanged = applied_resize(
    resize_pane_result(&mut owner, &root.terminal_id, ResizeDirection::Up, 1).await?,
  );
  assert_eq!(unchanged, zoomed);
  let resized = applied_resize(
    resize_pane_result(
      &mut owner,
      &root.terminal_id,
      ResizeDirection::Left,
      u16::MAX,
    )
    .await?,
  );
  assert_eq!(
    (resized.panes[0].columns, resized.panes[1].columns),
    (2, 77)
  );
  assert_eq!(resized.revision, zoomed.revision + 1);
  assert_unzoomed_geometry(&resized);
  let unchanged = applied_resize(
    resize_pane_result(&mut owner, &root.terminal_id, ResizeDirection::Left, 1).await?,
  );
  assert_eq!(unchanged, resized);
  write_frame(
    &mut owner,
    &ClientMessage::Input {
      data: b"still-typing-after-resize-errors\n".to_vec(),
    },
  )
  .await?;
  read_output_until(&mut owner, b"still-typing-after-resize-errors").await?;
  kill_shell_session(&socket, &root.session_id).await?;
  drop(owner);
  wait_for_daemon_exit(daemon, "resize zoom daemon did not exit").await
}

async fn historical_connection(
  socket: &Path,
  contract: ctl_core::protocol::ProtocolVersion,
) -> TestResult<UnixStream> {
  let mut stream = connect_when_ready(socket).await?;
  write_frame(
    &mut stream,
    &ClientMessage::Handshake {
      protocol: ctl_core::protocol::ProtocolOffer::new(contract.build, contract, &[contract]),
      client_name: "historical-proportions".into(),
      client_version: "test".into(),
    },
  )
  .await?;
  assert!(
    matches!(required_message(&mut stream).await?, ServerMessage::HandshakeAccepted { protocol_version, .. } if protocol_version == contract)
  );
  Ok(stream)
}

async fn historical_view_request(
  socket: &Path,
  contract: ctl_core::protocol::ProtocolVersion,
  request: ClientMessage,
) -> TestResult<ServerMessage> {
  let mut stream = historical_connection(socket, contract).await?;
  write_frame(&mut stream, &request).await?;
  required_message(&mut stream).await
}

async fn assert_legacy_layout_preserves_weights(
  socket: &Path,
  root: &SessionInfo,
  contract: ctl_core::protocol::ProtocolVersion,
) -> TestResult {
  let authoritative = topology_view(socket, &root.session_id).await?;
  let ServerMessage::ViewSnapshot { mut view } = historical_view_request(
    socket,
    contract,
    ClientMessage::GetView {
      session: root.session_id.clone(),
    },
  )
  .await?
  else {
    panic!("view expected");
  };
  assert_eq!(view.panes, authoritative.panes);
  assert!(!view.layout.has_weights());
  let ctmux_proto::ViewLayout::Split { children, .. } = &mut view.layout else {
    panic!("split expected");
  };
  children.swap(0, 1);
  let ServerMessage::ViewSnapshot { view: swapped } = historical_view_request(
    socket,
    contract,
    ClientMessage::UpdateView {
      session: root.session_id.clone(),
      expected_revision: view.revision,
      layout: view.layout,
    },
  )
  .await?
  else {
    panic!("swap expected");
  };
  assert!(!swapped.layout.has_weights());
  let actual = topology_view(socket, &root.session_id).await?;
  let ctmux_proto::ViewLayout::Split { weights, .. } = &authoritative.layout else {
    panic!("weights expected");
  };
  assert!(
    matches!(&actual.layout, ctmux_proto::ViewLayout::Split { weights: actual_weights, .. } if actual_weights == weights)
  );
  assert_eq!((actual.panes[0].columns, actual.panes[1].columns), (45, 34));
  assert!(matches!(
    historical_view_request(
      socket,
      contract,
      ClientMessage::UpdateView {
        session: root.session_id.clone(),
        expected_revision: actual.revision,
        layout: actual.layout.clone()
      }
    )
    .await?,
    ServerMessage::Error {
      code: ErrorCode::InvalidRequest,
      ..
    }
  ));
  let mut restructured = actual.layout;
  restructured.clear_weights();
  let ctmux_proto::ViewLayout::Split { axis, .. } = &mut restructured else {
    panic!("split expected");
  };
  *axis = ctmux_proto::SplitAxis::Vertical;
  assert!(matches!(
    historical_view_request(
      socket,
      contract,
      ClientMessage::UpdateView {
        session: root.session_id.clone(),
        expected_revision: actual.revision,
        layout: restructured
      }
    )
    .await?,
    ServerMessage::Error {
      code: ErrorCode::InvalidRequest,
      ..
    }
  ));
  assert_legacy_pane_resize_unavailable(socket, root, contract).await
}

async fn assert_legacy_pane_resize_unavailable(
  socket: &Path,
  root: &SessionInfo,
  contract: ctl_core::protocol::ProtocolVersion,
) -> TestResult {
  let mut stream = historical_connection(socket, contract).await?;
  write_frame(
    &mut stream,
    &ClientMessage::AttachSession {
      session: root.terminal_id.clone(),
      resume_from: None,
      terminal_size: TerminalSize::default(),
      request_input_lease: false,
      request_layout_lease: false,
      request_command_line: false,
      request_running_command: false,
      presentation_window_bytes: DEFAULT_PRESENTATION_WINDOW_BYTES,
    },
  )
  .await?;
  let ServerMessage::Attached { checkpoint, .. } = required_message(&mut stream).await? else {
    panic!("attached expected");
  };
  if let Some(checkpoint) = checkpoint {
    acknowledge_output(&mut stream, checkpoint.sequence).await?;
  }
  write_frame(
    &mut stream,
    &ClientMessage::ResizePane {
      request_id: "old-mutation".into(),
      terminal_id: root.terminal_id.clone(),
      direction: ctmux_proto::ResizeDirection::Right,
      amount: 5,
    },
  )
  .await?;
  expect_error(&mut stream, ErrorCode::InvalidRequest).await?;
  write_frame(
    &mut stream,
    &ClientMessage::ResizeDivider {
      request_id: "old-divider".into(),
      divider: ctmux_proto::DividerResize {
        view_id: root.view_id.clone(),
        expected_revision: 0,
        split_path: Vec::new(),
        boundary: 0,
        position: 50,
      },
    },
  )
  .await?;
  expect_error(&mut stream, ErrorCode::InvalidRequest).await?;
  write_frame(&mut stream, &ClientMessage::Detach).await?;
  wait_for_detached(&mut stream).await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn historical_contracts_preserve_new_proportions_and_cannot_resize_unleased() -> TestResult {
  let _guard = pty_test_lock().await;
  let directory = TestDirectory::new();
  let socket = directory.path.join("ctmux.sock");
  let daemon = spawn_daemon(&socket, 64 * 1024, 4 * 1024);
  let root = create_shell_session(&socket, "proportion-compat", "IFS= read -r line").await?;
  split_topology_shell(&socket, &root).await?;
  let (mut owner, _) = attach_session(&socket, &root.terminal_id, None, true, true).await?;
  resize_pane_result(
    &mut owner,
    &root.terminal_id,
    ctmux_proto::ResizeDirection::Right,
    5,
  )
  .await?;
  for contract in [
    ctmux_proto::CONTRACT_V1_0_13,
    ctmux_proto::CONTRACT_V1_1_14,
    ctmux_proto::CONTRACT_V1_1_15,
  ] {
    assert_legacy_layout_preserves_weights(&socket, &root, contract).await?;
  }
  let before = topology_view(&socket, &root.session_id).await?;
  let mut forged = before.layout.clone();
  let ctmux_proto::ViewLayout::Split { weights, .. } = &mut forged else {
    panic!("split expected");
  };
  *weights = vec![1, 1];
  assert!(matches!(
    topology_request(
      &socket,
      ClientMessage::UpdateView {
        session: root.session_id.clone(),
        expected_revision: before.revision,
        layout: forged
      }
    )
    .await?,
    ServerMessage::Error {
      code: ErrorCode::InvalidRequest,
      ..
    }
  ));
  assert_eq!(topology_view(&socket, &root.session_id).await?, before);
  for weights in [vec![0, 1], vec![1], vec![1, 2, 3]] {
    let mut malformed = before.layout.clone();
    let ctmux_proto::ViewLayout::Split {
      weights: values, ..
    } = &mut malformed
    else {
      panic!("split expected");
    };
    *values = weights;
    assert!(matches!(
      topology_request(
        &socket,
        ClientMessage::UpdateView {
          session: root.session_id.clone(),
          expected_revision: before.revision,
          layout: malformed
        }
      )
      .await?,
      ServerMessage::Error {
        code: ErrorCode::InvalidRequest,
        ..
      }
    ));
  }
  kill_shell_session(&socket, &root.session_id).await?;
  drop(owner);
  wait_for_daemon_exit(daemon, "proportion compatibility daemon did not exit").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn weighted_topology_keeps_ratios_through_split_removal_and_merge() -> TestResult {
  let _guard = pty_test_lock().await;
  let directory = TestDirectory::new();
  let socket = directory.path.join("ctmux.sock");
  let daemon = spawn_daemon(&socket, 64 * 1024, 4 * 1024);
  let root = create_shell_session(&socket, "weighted-topology", "IFS= read -r line").await?;
  split_topology_shell(&socket, &root).await?;
  let (mut owner, _) = attach_session(&socket, &root.terminal_id, None, true, true).await?;
  let resized = applied_resize(
    resize_pane_result(
      &mut owner,
      &root.terminal_id,
      ctmux_proto::ResizeDirection::Right,
      5,
    )
    .await?,
  );
  let divided = split_topology_shell(&socket, &root).await?;
  assert!(
    matches!(&divided.layout, ctmux_proto::ViewLayout::Split { weights, .. } if weights == &[45, 34])
  );
  let added = divided
    .terminals
    .iter()
    .find(|terminal| {
      !resized
        .terminals
        .iter()
        .any(|previous| previous.terminal_id == terminal.terminal_id)
    })
    .unwrap()
    .terminal_id
    .clone();
  topology_request(&socket, ClientMessage::KillTerminal { terminal_id: added }).await?;
  timeout(Duration::from_secs(3), async {
    loop {
      if topology_view(&socket, &root.session_id)
        .await?
        .terminals
        .len()
        == 2
      {
        return Ok::<_, Box<dyn Error + Send + Sync>>(());
      }
      sleep(Duration::from_millis(10)).await;
    }
  })
  .await??;
  let restored = topology_view(&socket, &root.session_id).await?;
  assert_eq!(restored.layout, resized.layout);
  let source = create_shell_session(&socket, "weighted-source", "IFS= read -r line").await?;
  let ServerMessage::ViewSnapshot { view: merged } = topology_request(
    &socket,
    ClientMessage::MergeSessions {
      source: source.session_id,
      destination: root.session_id.clone(),
    },
  )
  .await?
  else {
    panic!("merged view expected");
  };
  let ctmux_proto::ViewLayout::Split {
    children, weights, ..
  } = &merged.layout
  else {
    panic!("split expected");
  };
  assert_eq!(weights.as_slice(), &[] as &[u32]);
  assert_eq!(children[0], resized.layout);
  assert_unzoomed_geometry(&merged);
  kill_shell_session(&socket, &root.session_id).await?;
  drop(owner);
  wait_for_daemon_exit(daemon, "weighted topology daemon did not exit").await
}

async fn set_view_zoom(
  socket: &Path,
  session_id: &str,
  stream: &mut UnixStream,
  terminal_id: Option<&str>,
) -> TestResult<ctmux_proto::ViewInfo> {
  let current = topology_view(socket, session_id).await?;
  let minimum_revision =
    current.revision + u64::from(current.zoomed_terminal_id.as_deref() != terminal_id);
  write_frame(
    stream,
    &ClientMessage::SetViewZoom {
      terminal_id: terminal_id.map(str::to_owned),
    },
  )
  .await?;
  wait_for_view_zoom_at_revision(stream, terminal_id, minimum_revision).await
}

async fn wait_for_view_zoom(
  stream: &mut UnixStream,
  terminal_id: Option<&str>,
) -> TestResult<ctmux_proto::ViewInfo> {
  wait_for_view_zoom_at_revision(stream, terminal_id, 0).await
}

async fn wait_for_view_zoom_at_revision(
  stream: &mut UnixStream,
  terminal_id: Option<&str>,
  minimum_revision: u64,
) -> TestResult<ctmux_proto::ViewInfo> {
  loop {
    match presented_message(stream).await? {
      ServerMessage::ViewSnapshot { view }
        if view.revision >= minimum_revision
          && view.zoomed_terminal_id.as_deref() == terminal_id =>
      {
        return Ok(view);
      }
      ServerMessage::ViewSnapshot { .. }
      | ServerMessage::PtyGeometryChanged { .. }
      | ServerMessage::Checkpoint { .. }
      | ServerMessage::ShellStateChanged { .. }
      | ServerMessage::Output { .. } => {}
      other => return Err(format!("expected view zoom update, got {other:?}").into()),
    }
  }
}

fn assert_terminal_size(view: &ctmux_proto::ViewInfo, id: &str, columns: u16, rows: u16) {
  assert_eq!(
    view
      .terminals
      .iter()
      .find(|terminal| terminal.terminal_id == id)
      .unwrap()
      .terminal_size,
    terminal_size(columns, rows)
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn view_zoom_requires_layout_ownership_and_preserves_hidden_terminals() -> TestResult {
  let _guard = pty_test_lock().await;
  let directory = TestDirectory::new();
  let socket = directory.path.join("ctmux.sock");
  let daemon = spawn_daemon(&socket, 64 * 1024, 4 * 1024);
  let root = create_shell_session(
    &socket,
    "zoom",
    "while IFS= read -r line; do printf '%s\\n' \"$line\"; done",
  )
  .await?;
  let saved = split_topology_shell(&socket, &root).await?;
  let second_id = saved.terminals[1].terminal_id.clone();
  let foreign = create_shell_session(&socket, "other-view", "IFS= read -r line").await?;
  let (mut first, _) = attach_session(&socket, &root.terminal_id, None, true, true).await?;
  let (mut second, _) = attach_session(&socket, &second_id, None, true, true).await?;
  write_frame(
    &mut second,
    &ClientMessage::SetViewZoom {
      terminal_id: Some(second_id.clone()),
    },
  )
  .await?;
  expect_error(&mut second, ErrorCode::LayoutLeaseRequired).await?;
  write_frame(
    &mut first,
    &ClientMessage::SetViewZoom {
      terminal_id: Some(foreign.terminal_id.clone()),
    },
  )
  .await?;
  expect_error(&mut first, ErrorCode::InvalidRequest).await?;
  let zoomed = set_view_zoom(
    &socket,
    &root.session_id,
    &mut first,
    Some(&root.terminal_id),
  )
  .await?;
  assert_eq!(zoomed.panes, saved.panes);
  assert_eq!(zoomed.layout, saved.layout);
  assert_eq!(zoomed.terminals.len(), 2);
  assert_eq!(zoomed.visible_panes().len(), 1);
  assert_terminal_size(&zoomed, &root.terminal_id, 80, 24);
  assert_terminal_size(&zoomed, &second_id, 39, 24);
  // The observer receives zoom immediately through attached metadata, without
  // polling GetView or acquiring the owner's layout lease.
  let observer = wait_for_view_zoom(&mut second, Some(&root.terminal_id)).await?;
  assert_eq!(observer.revision, zoomed.revision);
  write_frame(
    &mut first,
    &ClientMessage::Resize {
      terminal_size: terminal_size(100, 30),
    },
  )
  .await?;
  wait_for_geometry_change(&mut first, &terminal_size(100, 30)).await?;
  let resized = topology_view(&socket, &root.session_id).await?;
  assert_eq!(resized.canvas_size, terminal_size(100, 30));
  assert_terminal_size(&resized, &root.terminal_id, 100, 30);
  assert_terminal_size(&resized, &second_id, 39, 24);
  write_frame(
    &mut second,
    &ClientMessage::Input {
      data: b"hidden-is-running\n".to_vec(),
    },
  )
  .await?;
  read_output_until(&mut second, b"child:hidden-is-running").await?;
  let retargeted = set_view_zoom(&socket, &root.session_id, &mut first, Some(&second_id)).await?;
  assert_terminal_size(&retargeted, &root.terminal_id, 50, 30);
  assert_terminal_size(&retargeted, &second_id, 100, 30);
  let restored = set_view_zoom(&socket, &root.session_id, &mut first, None).await?;
  assert_eq!(restored.layout, saved.layout);
  assert_terminal_size(&restored, &root.terminal_id, 50, 30);
  assert_terminal_size(&restored, &second_id, 49, 30);
  kill_shell_session(&socket, &foreign.session_id).await?;
  kill_shell_session(&socket, &root.session_id).await?;
  drop(first);
  drop(second);
  wait_for_daemon_exit(daemon, "zoom daemon did not exit").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn view_zoom_survives_attachment_resume_and_clears_when_target_exits() -> TestResult {
  let _guard = pty_test_lock().await;
  let directory = TestDirectory::new();
  let socket = directory.path.join("ctmux.sock");
  let daemon = spawn_daemon(&socket, 64 * 1024, 4 * 1024);
  let root = create_shell_session(
    &socket,
    "zoom-resume",
    "while IFS= read -r line; do printf '%s\\n' \"$line\"; done",
  )
  .await?;
  let saved = split_topology_shell(&socket, &root).await?;
  let second_id = saved.terminals[1].terminal_id.clone();
  let (mut child, _) = attach_session(&socket, &second_id, None, true, false).await?;
  let (mut owner, attached) = attach_session(&socket, &root.terminal_id, None, true, true).await?;
  let ServerMessage::Attached {
    attachment_token, ..
  } = attached
  else {
    panic!("expected attached");
  };
  set_view_zoom(&socket, &root.session_id, &mut owner, Some(&second_id)).await?;
  drop(owner);
  let (mut resumed, _) =
    resume_attachment(&socket, &root.terminal_id, &attachment_token, None).await?;
  let current = topology_view(&socket, &root.session_id).await?;
  assert_eq!(
    current.zoomed_terminal_id.as_deref(),
    Some(second_id.as_str())
  );
  assert_eq!(current.panes, saved.panes);
  // EOF at an empty canonical input line makes the fixture finish naturally.
  write_frame(&mut child, &ClientMessage::Input { data: vec![4] }).await?;
  wait_for_session_end(&mut child).await?;
  wait_for_single_terminal(&socket, &root.session_id).await?;
  wait_for_view_zoom(&mut resumed, None).await?;
  let cleared = topology_view(&socket, &root.session_id).await?;
  assert_eq!(cleared.terminals.len(), 1);
  assert_terminal_size(&cleared, &root.terminal_id, 80, 24);
  kill_shell_session(&socket, &root.session_id).await?;
  drop(resumed);
  drop(child);
  wait_for_daemon_exit(daemon, "zoom resume daemon did not exit").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn historical_contracts_omit_zoom_and_reject_zoom_mutations() -> TestResult {
  use ctl_core::protocol::ProtocolOffer;
  let _guard = pty_test_lock().await;
  let directory = TestDirectory::new();
  let socket = directory.path.join("ctmux.sock");
  let daemon = spawn_daemon(&socket, 64 * 1024, 4 * 1024);
  let root = create_shell_session(&socket, "zoom-compat", "IFS= read -r line").await?;
  let saved = split_topology_shell(&socket, &root).await?;
  let (mut owner, _) = attach_session(&socket, &root.terminal_id, None, true, true).await?;
  set_view_zoom(
    &socket,
    &root.session_id,
    &mut owner,
    Some(&root.terminal_id),
  )
  .await?;
  for contract in [ctmux_proto::CONTRACT_V1_0_13, ctmux_proto::CONTRACT_V1_1_14] {
    let mut old = connect_when_ready(&socket).await?;
    write_frame(
      &mut old,
      &ClientMessage::Handshake {
        protocol: ProtocolOffer::new(contract.build, contract, &[contract]),
        client_name: "historical".into(),
        client_version: "test".into(),
      },
    )
    .await?;
    assert!(matches!(required_message(&mut old).await?,
      ServerMessage::HandshakeAccepted { protocol_version, .. } if protocol_version == contract));
    write_frame(
      &mut old,
      &ClientMessage::GetView {
        session: root.session_id.clone(),
      },
    )
    .await?;
    let raw: serde_json::Value = timeout(Duration::from_secs(3), read_frame(&mut old))
      .await??
      .unwrap();
    assert!(raw["view"].get("zoomed_terminal_id").is_none());
    let ServerMessage::ViewSnapshot { view } = serde_json::from_value(raw)? else {
      panic!("expected view");
    };
    assert_eq!(view.panes, saved.panes);
    assert_eq!(view.terminals.len(), 2);
    let mut old = connect_when_ready(&socket).await?;
    write_frame(
      &mut old,
      &ClientMessage::Handshake {
        protocol: ProtocolOffer::new(contract.build, contract, &[contract]),
        client_name: "historical-attached".into(),
        client_version: "test".into(),
      },
    )
    .await?;
    required_message(&mut old).await?;
    write_frame(
      &mut old,
      &ClientMessage::AttachSession {
        session: root.terminal_id.clone(),
        resume_from: None,
        terminal_size: TerminalSize::default(),
        request_input_lease: false,
        request_layout_lease: false,
        request_command_line: false,
        request_running_command: false,
        presentation_window_bytes: DEFAULT_PRESENTATION_WINDOW_BYTES,
      },
    )
    .await?;
    let attached = required_message(&mut old).await?;
    if let ServerMessage::Attached {
      checkpoint: Some(checkpoint),
      ..
    } = &attached
    {
      acknowledge_output(&mut old, checkpoint.sequence).await?;
    }
    write_frame(&mut old, &ClientMessage::SetViewZoom { terminal_id: None }).await?;
    expect_error(&mut old, ErrorCode::InvalidRequest).await?;
    assert_eq!(
      topology_view(&socket, &root.session_id)
        .await?
        .zoomed_terminal_id
        .as_deref(),
      Some(root.terminal_id.as_str())
    );
    write_frame(&mut old, &ClientMessage::Detach).await?;
    wait_for_detached(&mut old).await?;
  }
  kill_shell_session(&socket, &root.session_id).await?;
  drop(owner);
  wait_for_daemon_exit(daemon, "zoom compatibility daemon did not exit").await
}

fn assert_unzoomed_geometry(view: &ctmux_proto::ViewInfo) {
  assert_eq!(view.zoomed_terminal_id, None);
  for pane in &view.panes {
    assert_terminal_size(view, &pane.terminal_id, pane.columns, pane.rows);
  }
}

async fn assert_zoom_cleared_by_layout_changes(
  socket: &Path,
  owner: &mut UnixStream,
  root: &SessionInfo,
) -> TestResult<ctmux_proto::ViewInfo> {
  set_view_zoom(socket, &root.session_id, owner, Some(&root.terminal_id)).await?;
  let split = split_topology_shell(socket, root).await?;
  assert_unzoomed_geometry(&split);
  let current = set_view_zoom(
    socket,
    &root.session_id,
    owner,
    Some(&split.terminals[2].terminal_id),
  )
  .await?;
  let updated = topology_request(
    socket,
    ClientMessage::UpdateView {
      session: root.session_id.clone(),
      expected_revision: current.revision,
      layout: current.layout,
    },
  )
  .await?;
  let ServerMessage::ViewSnapshot { view } = updated else {
    panic!("expected updated view");
  };
  assert_unzoomed_geometry(&view);
  Ok(view)
}

async fn assert_zoom_cleared_by_membership_moves(
  socket: &Path,
  owner: &mut UnixStream,
  root: &SessionInfo,
  view: &ctmux_proto::ViewInfo,
) -> TestResult {
  let moved_id = &view.terminals[2].terminal_id;
  set_view_zoom(socket, &root.session_id, owner, Some(moved_id)).await?;
  let promoted = topology_request(
    socket,
    ClientMessage::PromoteTerminal {
      terminal_id: moved_id.clone(),
      name: Some("zoom-promoted".into()),
    },
  )
  .await?;
  let ServerMessage::ViewSnapshot { view: promoted } = promoted else {
    panic!("expected promoted view");
  };
  assert_unzoomed_geometry(&promoted);
  assert_unzoomed_geometry(&topology_view(socket, &root.session_id).await?);
  set_view_zoom(socket, &root.session_id, owner, Some(&root.terminal_id)).await?;
  let merged = topology_request(
    socket,
    ClientMessage::MergeSessions {
      source: promoted.session_id,
      destination: root.session_id.clone(),
    },
  )
  .await?;
  let ServerMessage::ViewSnapshot { view: merged } = merged else {
    panic!("expected merged view");
  };
  assert_unzoomed_geometry(&merged);
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn topology_edits_clear_zoom_before_reflowing_saved_layout() -> TestResult {
  let _guard = pty_test_lock().await;
  let directory = TestDirectory::new();
  let socket = directory.path.join("ctmux.sock");
  let daemon = spawn_daemon(&socket, 64 * 1024, 4 * 1024);
  let root = create_shell_session(
    &socket,
    "zoom-topology",
    "while IFS= read -r line; do printf '%s\\n' \"$line\"; done",
  )
  .await?;
  split_topology_shell(&socket, &root).await?;
  let (mut owner, _) = attach_session(&socket, &root.terminal_id, None, true, true).await?;
  let view = assert_zoom_cleared_by_layout_changes(&socket, &mut owner, &root).await?;
  assert_zoom_cleared_by_membership_moves(&socket, &mut owner, &root, &view).await?;
  set_view_zoom(
    &socket,
    &root.session_id,
    &mut owner,
    Some(&root.terminal_id),
  )
  .await?;
  // Removing any hidden pane also restores the surviving split geometry.
  let response = topology_request(
    &socket,
    ClientMessage::KillTerminal {
      terminal_id: view.terminals[1].terminal_id.clone(),
    },
  )
  .await?;
  assert_eq!(response, ServerMessage::Success);
  let removed = wait_for_view_zoom(&mut owner, None).await?;
  assert_eq!(removed.terminals.len(), 2);
  assert_unzoomed_geometry(&removed);
  kill_shell_session(&socket, &root.session_id).await?;
  drop(owner);
  wait_for_daemon_exit(daemon, "zoom topology daemon did not exit").await
}

fn encoded_heartbeat(nonce: u64) -> TestResult<Vec<u8>> {
  let payload = serde_json::to_vec(&ClientMessage::Heartbeat { nonce })?;
  let mut frame = u32::try_from(payload.len())?.to_be_bytes().to_vec();
  frame.extend(payload);
  Ok(frame)
}

async fn fill_ready_control_queue(writer: &tokio::net::unix::OwnedWriteHalf) -> TestResult<usize> {
  // Far more than the Unix socket's queue capacity. Keep the producer paused
  // once full; the completed input-frame count is an exact wire barrier.
  let frame = encoded_heartbeat(1)?;
  let bytes = frame.repeat(100_000);
  writer.writable().await?;
  let mut written = 0;
  while written < bytes.len() {
    match writer.try_write(&bytes[written..]) {
      Ok(0) => return Err("observer control queue closed".into()),
      Ok(count) => written += count,
      Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
        let complete_frames = written / frame.len();
        assert!(
          complete_frames > 1,
          "queue must contain ready control frames"
        );
        return Ok(complete_frames);
      }
      Err(error) => return Err(error.into()),
    }
  }
  Err("observer control queue never reached backpressure".into())
}

async fn assert_view_precedes_control_barrier(
  reader: &mut tokio::net::unix::OwnedReadHalf,
  terminal_id: &str,
  queued_frames: usize,
) -> TestResult {
  let mut acknowledged_frames = 0;
  loop {
    let message = timeout(
      Duration::from_secs(3),
      read_frame::<_, ServerMessage>(reader),
    )
    .await
    .map_err(|_| "view update starved behind ready control traffic")??
    .ok_or("observer closed before receiving shared zoom")?;
    match message {
      ServerMessage::ViewSnapshot { view }
        if view.zoomed_terminal_id.as_deref() == Some(terminal_id) =>
      {
        return Ok(());
      }
      ServerMessage::HeartbeatAck { nonce: 1 } => {
        acknowledged_frames += 1;
        if acknowledged_frames >= queued_frames {
          return Err("view update waited until all ready control traffic drained".into());
        }
      }
      ServerMessage::ViewSnapshot { .. }
      | ServerMessage::ShellStateChanged { .. }
      | ServerMessage::LeaseStatus {
        notification: true, ..
      } => {}
      other => return Err(format!("unexpected observer frame: {other:?}").into()),
    }
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_view_zoom_progresses_while_control_messages_remain_ready() -> TestResult {
  let _guard = pty_test_lock().await;
  let directory = TestDirectory::new();
  let socket = directory.path.join("ctmux.sock");
  let daemon = spawn_daemon(&socket, 64 * 1024, 4 * 1024);
  let root = create_shell_session(&socket, "zoom-control-fairness", "IFS= read -r line").await?;
  let saved = split_topology_shell(&socket, &root).await?;
  let (mut owner, _) = attach_session(&socket, &root.terminal_id, None, true, true).await?;
  let (mut observer, _) =
    attach_session(&socket, &saved.terminals[1].terminal_id, None, false, false).await?;
  heartbeat(&mut observer, 99).await?;
  let (mut reader, writer) = observer.into_split();
  let queued_frames = fill_ready_control_queue(&writer).await?;
  set_view_zoom(
    &socket,
    &root.session_id,
    &mut owner,
    Some(&root.terminal_id),
  )
  .await?;
  let result =
    assert_view_precedes_control_barrier(&mut reader, &root.terminal_id, queued_frames).await;
  drop(writer);
  drop(reader);
  kill_shell_session(&socket, &root.session_id).await?;
  drop(owner);
  wait_for_daemon_exit(daemon, "zoom fairness daemon did not exit").await?;
  result
}
