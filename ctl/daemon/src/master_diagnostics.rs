//! Bounded in-memory SSH diagnostics; persisted loss reasons are fixed codes.

use std::path::Path;
use std::time::Duration;

use ctl_core::observability::{ConnectionContext, Event, Level, Outcome};
use tokio::io::{AsyncRead, AsyncReadExt as _};

use super::target_lifecycle::{AttemptStatus, TargetAttempt};
use super::{MasterEndpoint, State, history_endpoint, socket_identity, target_key};

const CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);
const CLEANUP_POLL_INTERVAL: Duration = Duration::from_millis(100);

pub(super) async fn collect(mut stderr: impl AsyncRead + Unpin) -> String {
  let mut retained = Vec::new();
  let mut buffer = [0_u8; 1024];
  let mut truncated = false;
  while let Ok(count) = stderr.read(&mut buffer).await {
    if count == 0 {
      break;
    }
    let discarded = (retained.len() + count).saturating_sub(super::MAX_DIAGNOSTICS);
    if discarded > 0 {
      retained.drain(..discarded);
      truncated = true;
    }
    retained.extend_from_slice(&buffer[..count]);
  }
  if truncated {
    // Cutting through a debug line must not turn its fragment into an unknown
    // startup error, hiding a later complete authentication-required message.
    if let Some(newline) = retained.iter().position(|byte| *byte == b'\n') {
      retained.drain(..=newline);
    } else {
      retained.clear();
    }
  }
  String::from_utf8_lossy(&retained).trim().to_owned()
}

pub(super) fn watch(
  diagnostics: tokio::task::JoinHandle<String>,
  state: &State,
  target: &ctl_ipc::SshTarget,
  endpoint: &MasterEndpoint,
) {
  let path = endpoint.control_path.clone();
  let identity = socket_identity(&path);
  let attempt = state.target(target).attempt();
  let connection = ConnectionContext {
    subject: target_key(target),
    endpoint: history_endpoint(target),
    correlation_id: state.correlation(endpoint),
  };
  tokio::spawn(async move {
    let Ok(diagnostics) = diagnostics.await else {
      return;
    };
    // EOF can mean daemonization or the end of our shared anchor, rather than
    // master loss. An explicit Disconnect already has its own history event.
    let Some(loss) = await_loss(&path, identity, &diagnostics, &attempt, CLEANUP_TIMEOUT).await
    else {
      return;
    };
    ctl_core::observability::connection_observed(
      "987df63a-b929-4c0a-a9d2-b80ebd963a4e",
      Event::ConnectionLoss,
      &connection,
      loss.level,
      loss.outcome,
      Some(loss.code),
      None,
    );
  });
}

async fn await_loss(
  path: &Path,
  previous: Option<(u64, u64)>,
  diagnostics: &str,
  attempt: &TargetAttempt,
  cleanup_timeout: Duration,
) -> Option<Loss> {
  let deadline = tokio::time::Instant::now() + cleanup_timeout;
  loop {
    if !matches!(attempt.status(), AttemptStatus::Current) {
      return None;
    }
    if let Some(loss) = confirmed_loss(path, previous, diagnostics) {
      return Some(loss);
    }
    if tokio::time::Instant::now() >= deadline {
      return None;
    }
    tokio::time::sleep(
      CLEANUP_POLL_INTERVAL.min(deadline.saturating_duration_since(tokio::time::Instant::now())),
    )
    .await;
  }
}

#[derive(Debug, PartialEq, Eq)]
struct Loss {
  code: &'static str,
  level: Level,
  outcome: Outcome,
}

fn confirmed_loss(path: &Path, previous: Option<(u64, u64)>, diagnostics: &str) -> Option<Loss> {
  match path.try_exists() {
    Ok(false) => Some(classify(diagnostics)),
    Ok(true)
      if previous.is_some() && socket_identity(path).is_some_and(|now| Some(now) != previous) =>
    {
      Some(classify(diagnostics))
    }
    // An existing endpoint or failed observation cannot prove loss. In
    // particular, never label an ssh -f foreground exit as master death.
    _ => None,
  }
}

fn classify(diagnostics: &str) -> Loss {
  let mut code = "ssh_master_closed_unknown";
  for line in diagnostics.lines().map(str::trim) {
    if line == "debug1: ControlPersist timeout expired" {
      return Loss {
        code: "ssh_master_idle_expired",
        level: Level::Info,
        outcome: Outcome::Succeeded,
      };
    }
    if line.starts_with("Timeout, server ") && line.ends_with(" not responding.") {
      code = "ssh_master_keepalive_timeout";
    } else if line.starts_with("Connection reset by ")
      || line.ends_with(": Connection reset by peer")
    {
      code = "ssh_master_connection_reset";
    } else if line == "client_loop: send disconnect: Broken pipe" {
      code = "ssh_master_broken_pipe";
    } else if code == "ssh_master_closed_unknown"
      && (line.starts_with("Received disconnect from ")
        || line.starts_with("Connection to ") && line.ends_with(" closed by remote host.")
        || line.starts_with("Connection closed by "))
    {
      code = "ssh_master_remote_closed";
    }
  }
  Loss {
    code,
    level: Level::Warn,
    outcome: Outcome::Failed,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn diagnostics_retain_a_bounded_tail_after_large_startup_output() {
    let mut output = vec![b'x'; super::super::MAX_DIAGNOSTICS * 3];
    output.extend_from_slice(b"\nTimeout, server fixture.invalid not responding.\n");
    let tail = collect(output.as_slice()).await;
    assert!(tail.len() <= super::super::MAX_DIAGNOSTICS);
    assert!(tail.ends_with("Timeout, server fixture.invalid not responding."));
    assert_eq!(classify(&tail).code, "ssh_master_keepalive_timeout");
  }

  #[test]
  fn unknown_and_empty_output_never_claim_network_failure_or_idle_expiration() {
    for output in [
      "",
      "SECRET_MUST_NOT_REACH_LOGS",
      "debug1: Sending keepalive",
    ] {
      let loss = classify(output);
      assert_eq!(loss.code, "ssh_master_closed_unknown");
      assert_eq!(loss.outcome, Outcome::Failed);
      assert_eq!(loss.level, Level::Warn);
    }
  }

  #[test]
  fn classified_transport_reasons_do_not_copy_untrusted_peer_text() {
    for (diagnostic, code) in [
      (
        "Timeout, server SECRET_PEER not responding.",
        "ssh_master_keepalive_timeout",
      ),
      (
        "Connection reset by SECRET_PEER port 22",
        "ssh_master_connection_reset",
      ),
      (
        "client_loop: send disconnect: Broken pipe",
        "ssh_master_broken_pipe",
      ),
      (
        "Received disconnect from fixture port 22:11: SECRET_MESSAGE",
        "ssh_master_remote_closed",
      ),
      (
        "Connection to fixture closed by remote host.",
        "ssh_master_remote_closed",
      ),
    ] {
      assert_eq!(classify(diagnostic).code, code);
    }
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn eof_with_the_same_socket_is_not_a_connection_loss() {
    let path = std::env::temp_dir().join(format!("ctld-diagnostics-{}", uuid::Uuid::new_v4()));
    let guard = super::super::SocketGuard(path);
    let listener = tokio::net::UnixListener::bind(&guard.0).unwrap();
    let identity = socket_identity(&guard.0);
    let diagnostics = collect(&b""[..]).await;
    let lifecycle = super::super::TargetLifecycle::default();
    assert!(
      await_loss(
        &guard.0,
        identity,
        &diagnostics,
        &lifecycle.attempt(),
        Duration::from_millis(20)
      )
      .await
      .is_none()
    );
    drop(listener);
    std::fs::remove_file(&guard.0).unwrap();
    assert_eq!(
      confirmed_loss(&guard.0, identity, &diagnostics)
        .unwrap()
        .code,
      "ssh_master_closed_unknown"
    );
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn eof_observation_waits_for_delayed_socket_cleanup() {
    let path = std::env::temp_dir().join(format!("ctld-diagnostics-{}", uuid::Uuid::new_v4()));
    let guard = super::super::SocketGuard(path);
    let listener = tokio::net::UnixListener::bind(&guard.0).unwrap();
    let identity = socket_identity(&guard.0);
    let path = guard.0.clone();
    let cleanup = tokio::spawn(async move {
      tokio::time::sleep(Duration::from_millis(25)).await;
      drop(listener);
      std::fs::remove_file(path).unwrap();
    });
    let lifecycle = super::super::TargetLifecycle::default();
    let loss = await_loss(
      &guard.0,
      identity,
      "Timeout, server fixture not responding.",
      &lifecycle.attempt(),
      Duration::from_secs(1),
    )
    .await
    .unwrap();
    assert_eq!(loss.code, "ssh_master_keepalive_timeout");
    cleanup.await.unwrap();
  }

  #[cfg(unix)]
  #[test]
  fn a_replaced_socket_confirms_loss_of_only_the_original_master() {
    let path = std::env::temp_dir().join(format!("ctld-diagnostics-{}", uuid::Uuid::new_v4()));
    let guard = super::super::SocketGuard(path);
    let original = std::os::unix::net::UnixListener::bind(&guard.0).unwrap();
    let identity = socket_identity(&guard.0);
    std::fs::remove_file(&guard.0).unwrap();
    // Keep the old inode alive while binding the replacement at the same path.
    let replacement = std::os::unix::net::UnixListener::bind(&guard.0).unwrap();
    assert_ne!(socket_identity(&guard.0), identity);
    assert_eq!(
      confirmed_loss(&guard.0, identity, "Connection closed by fixture port 22")
        .unwrap()
        .code,
      "ssh_master_remote_closed"
    );
    assert!(confirmed_loss(&guard.0, socket_identity(&guard.0), "").is_none());
    drop((original, replacement));
  }

  #[tokio::test]
  async fn disconnect_then_connect_does_not_report_the_old_master_as_an_automatic_loss() {
    let path = std::env::temp_dir().join(format!("ctld-diagnostics-{}", uuid::Uuid::new_v4()));
    let lifecycle = super::super::TargetLifecycle::default();
    let old = lifecycle.attempt();
    lifecycle.pause();
    lifecycle.resume(&lifecycle.attempt()).unwrap();
    assert!(
      await_loss(&path, None, "", &old, Duration::ZERO)
        .await
        .is_none()
    );
  }

  #[test]
  fn missing_endpoints_preserve_explicit_idle_and_transport_classifications() {
    let path = std::env::temp_dir().join(format!("ctld-diagnostics-{}", uuid::Uuid::new_v4()));
    let idle = confirmed_loss(&path, None, "debug1: ControlPersist timeout expired").unwrap();
    assert_eq!(idle.code, "ssh_master_idle_expired");
    assert_eq!(idle.outcome, Outcome::Succeeded);
    let lost = confirmed_loss(&path, None, "Timeout, server fixture not responding.").unwrap();
    assert_eq!(lost.code, "ssh_master_keepalive_timeout");
    assert_eq!(lost.outcome, Outcome::Failed);
  }
}
