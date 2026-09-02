//! Opt-in live smoke test. Uses two separate native client processes and only
//! removes the two temporary sessions it creates on the development target.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rmux_client::{AttachRequest, ClientIdentity, DEFAULT_PRESENTATION_WINDOW_BYTES, begin_attach};
use tokio::process::Command;
use tokio::time::timeout;

use super::repository::Repository;
use super::{
  SessionReference, UpdateWorkspaceRequest, WorkspaceDocument, WorkspaceHost, WorkspaceSession,
};
use crate::commands::inspection::{InspectKnownSessionsRequest, inspect_known_sessions};
use crate::commands::{create_session, kill_session};
use crate::dto::{
  ConnectionTargetDto, CreateSessionRequestDto, KillSessionRequestDto, TerminalSizeDto,
};

const TEST_NAME: &str = "workspace::remote_test::docker_workspace_survives_client_restart";

#[tokio::test]
#[ignore = "requires the explicit local Docker SSH fixture and RMUX_WORKSPACE_TEST_IDENTITY"]
async fn docker_workspace_survives_client_restart() -> Result<(), String> {
  let identity_file = std::env::var("RMUX_WORKSPACE_TEST_IDENTITY").map_err(
    |_| "Set RMUX_WORKSPACE_TEST_IDENTITY to the development container's SSH identity path.",
  )?;
  let target = ConnectionTargetDto::Ssh {
    destination: "workspace-smoke".into(),
    hostname: Some("127.0.0.1".into()),
    user: Some("rmux".into()),
    port: Some(2222),
    identity_file: Some(identity_file),
  };
  if let Ok(phase) = std::env::var("RMUX_WORKSPACE_TEST_PHASE") {
    let directory = PathBuf::from(
      std::env::var("RMUX_WORKSPACE_TEST_DIRECTORY").map_err(|error| error.to_string())?,
    );
    return match phase.as_str() {
      "create" => create_phase(&directory, &target).await,
      "restore" => restore_phase(&directory, &target).await,
      _ => Err("Unknown smoke-test phase".into()),
    };
  }

  let directory =
    std::env::temp_dir().join(format!("rmux-live-workspace-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir(&directory).map_err(|error| error.to_string())?;
  let result = async {
    for phase in ["create", "restore"] {
      let mut child = Command::new(std::env::current_exe().map_err(|error| error.to_string())?);
      child
        .args([TEST_NAME, "--exact", "--ignored", "--nocapture"])
        .env("RMUX_WORKSPACE_TEST_PHASE", phase)
        .env("RMUX_WORKSPACE_TEST_DIRECTORY", &directory)
        .kill_on_drop(true);
      let output = timeout(Duration::from_secs(45), child.output())
        .await
        .map_err(|_| format!("{phase} client timed out"))?
        .map_err(|error| error.to_string())?;
      if !output.status.success() {
        return Err(format!(
          "{phase} client failed: {} {}",
          String::from_utf8_lossy(&output.stdout),
          String::from_utf8_lossy(&output.stderr)
        ));
      }
    }
    Ok(())
  }
  .await;
  // Cleanup also runs when either client fails. Never enumerate or kill
  // unrelated sessions on the shared remote daemon.
  let mut cleanup_error = None;
  if let Ok(bytes) = std::fs::read(directory.join("created_ids.json")) {
    let ids: Vec<String> = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    for session_id in ids {
      if let Err(error) = kill_session(KillSessionRequestDto {
        target: target.clone(),
        session_id,
      })
      .await
      {
        cleanup_error = Some(error.message);
      }
    }
  }
  if let Some(error) = cleanup_error {
    return Err(format!(
      "Test cleanup failed; references preserved at {}: {error}",
      directory.display()
    ));
  }
  std::fs::remove_dir_all(directory).map_err(|error| error.to_string())?;
  result
}

async fn create_phase(directory: &Path, target: &ConnectionTargetDto) -> Result<(), String> {
  let mut ids = Vec::new();
  let mut first = None;
  for _ in 0..2 {
    let session = create_session(CreateSessionRequestDto {
      target: target.clone(),
      working_directory: None,
      terminal_size: TerminalSizeDto {
        columns: 80,
        rows: 24,
        pixel_width: None,
        pixel_height: None,
      },
    })
    .await
    .map_err(|error| error.message)?;
    ids.push(session.session_id.clone());
    std::fs::write(
      directory.join("created_ids.json"),
      serde_json::to_vec(&ids).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if first.is_none() {
      first = Some(session);
    }
  }
  let session = first.ok_or("No test session created")?;
  let reference = SessionReference {
    host_id: "remote".into(),
    session_id: session.session_id.clone(),
  };
  let mut document = WorkspaceDocument::default();
  document.hosts.push(WorkspaceHost {
    host_id: "remote".into(),
    target: target.clone(),
  });
  document.sessions.push(WorkspaceSession {
    host_id: "remote".into(),
    session_id: session.session_id,
    name: session.name,
    last_known_cwd: None,
    last_known_cwd_display: None,
  });
  document.tabs.push(reference.clone());
  document.active_tab = Some(reference);
  Repository::new(directory.to_path_buf())
    .update(UpdateWorkspaceRequest {
      expected_revision: None,
      document,
    })
    .map_err(|error| error.message)?;
  // The whole native client process now exits, including all SSH children.
  Ok(())
}

async fn restore_phase(directory: &Path, target: &ConnectionTargetDto) -> Result<(), String> {
  let restored = Repository::new(directory.to_path_buf())
    .load()
    .map_err(|error| error.message)?;
  let session_id = restored
    .document
    .sessions
    .first()
    .ok_or("No remembered session")?
    .session_id
    .clone();
  let results = inspect_known_sessions(InspectKnownSessionsRequest {
    target: target.clone(),
    session_ids: vec![session_id.clone()],
  })
  .await
  .map_err(|error| error.message)?;
  if results.len() != 1
    || results[0]
      .session
      .as_ref()
      .is_none_or(|session| session.session_id != session_id)
  {
    return Err(format!("Expected only the remembered session: {results:?}"));
  }
  let stream = crate::transport::connect(target)
    .await
    .map_err(|error| error.message)?;
  let (stream, attached) = begin_attach(
    stream,
    &ClientIdentity {
      name: "workspace-restart-smoke".into(),
      version: "test".into(),
    },
    AttachRequest {
      session: session_id.clone(),
      resume_from: None,
      terminal_size: rmux_proto::TerminalSize::default(),
      request_input_lease: false,
      request_layout_lease: false,
      request_command_line: false,
      request_running_command: false,
      presentation_window_bytes: DEFAULT_PRESENTATION_WINDOW_BYTES,
    },
  )
  .await
  .map_err(|error| error.to_string())?;
  if attached.session.session_id != session_id || attached.checkpoint.is_none() {
    return Err("Fresh attachment did not restore the remembered session from a checkpoint".into());
  }
  drop(stream);
  Ok(())
}
