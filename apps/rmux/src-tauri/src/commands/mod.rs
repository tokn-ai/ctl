use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use rmux_client::{
  AttachRequest, AttachmentController, AttachmentControllerOptions, ClientIdentity,
  DEFAULT_PRESENTATION_WINDOW_BYTES, begin_attach, get_shell_state, request as rmux_request,
};
use rmux_proto::{ClientMessage, ServerMessage};
use tauri::ipc::Channel;
use tauri::{State, WebviewWindow};
use tokio::task::JoinSet;
use tokio::time::timeout;

use crate::dto::{
  AcknowledgeAttachmentEventRequestDto, AttachmentEventDto, AttachmentLeaseRequestDto,
  AttachmentRequestDto, CreateSessionRequestDto, KillSessionRequestDto, OpenAttachmentRequestDto,
  OpenAttachmentResponseDto, ResizeAttachmentRequestDto, RestartLocalDaemonResponseDto,
  SendInputRequestDto, SessionDto, SessionListDto, ShellStateDto, TargetRequestDto, decode_input,
  parse_sequence,
};
use crate::error::{CommandErrorDto, CommandResult};
use crate::local_transport;
use crate::state::{AppState, AttachmentActor, forward_attachment_events};
use crate::transport;

const CLIENT_NAME: &str = "rmux-app";
const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const LOCAL_SESSION_SHELL_STATE_INSPECTION_TIMEOUT: Duration = Duration::from_millis(250);
const REMOTE_SESSION_SHELL_STATE_INSPECTION_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_CONCURRENT_SESSION_SHELL_STATE_INSPECTIONS: usize = 4;

#[tauri::command]
pub async fn list_sessions(request: TargetRequestDto) -> CommandResult<SessionListDto> {
  let stream = transport::connect(&request.target).await?;
  let response = rmux_request(stream, &client_identity(), ClientMessage::ListSessions)
    .await
    .map_err(CommandErrorDto::client)?;
  match response {
    ServerMessage::SessionList { sessions } => Ok(SessionListDto {
      shell_states: inspect_session_shell_states(&request.target, &sessions).await,
      sessions: sessions
        .into_iter()
        .map(|session| SessionDto::new(session, request.target.clone()))
        .collect(),
    }),
    response => Err(unexpected_response("session_list", &response)),
  }
}

/// Retrieves presentation metadata without making the session list fragile.
///
/// Listing is authoritative. Individual sessions can naturally exit after it
/// returns, so an unavailable endpoint, a failed handshake, or a missing
/// session merely omits that shell snapshot. The frontend then falls back to a
/// neutral title while keeping the list usable.
async fn inspect_session_shell_states(
  target: &crate::dto::ConnectionTargetDto,
  sessions: &[rmux_proto::SessionInfo],
) -> BTreeMap<String, ShellStateDto> {
  let mut shell_states = BTreeMap::new();
  let mut session_ids = sessions.iter().map(|session| session.session_id.clone());
  let mut inspections = JoinSet::new();

  // A stale lookup must not turn a refresh into one timeout per listed row.
  for _ in 0..MAX_CONCURRENT_SESSION_SHELL_STATE_INSPECTIONS {
    let Some(session_id) = session_ids.next() else {
      break;
    };
    inspections.spawn(inspect_session_shell_state(target.clone(), session_id));
  }

  while let Some(result) = inspections.join_next().await {
    if let Ok(Some((session_id, shell_state))) = result {
      shell_states.insert(session_id, shell_state);
    }

    if let Some(session_id) = session_ids.next() {
      inspections.spawn(inspect_session_shell_state(target.clone(), session_id));
    }
  }

  shell_states
}

async fn inspect_session_shell_state(
  target: crate::dto::ConnectionTargetDto,
  session_id: String,
) -> Option<(String, ShellStateDto)> {
  let identity = client_identity();
  let inspection_timeout = if target.is_local() {
    LOCAL_SESSION_SHELL_STATE_INSPECTION_TIMEOUT
  } else {
    REMOTE_SESSION_SHELL_STATE_INSPECTION_TIMEOUT
  };
  let snapshot = timeout(inspection_timeout, async {
    let stream = transport::connect_existing(&target).await?;
    get_shell_state(stream, &identity, &session_id)
      .await
      .map_err(CommandErrorDto::client)
  })
  .await
  .ok()?
  .ok()?;

  (snapshot.session.session_id == session_id).then(|| (session_id, snapshot.shell_state.into()))
}

#[tauri::command]
pub async fn create_session(request: CreateSessionRequestDto) -> CommandResult<SessionDto> {
  let terminal_size = request.terminal_size.into_proto()?;
  let working_directory = match (request.working_directory, request.target.is_local()) {
    (Some(directory), _) => Some(directory),
    (None, true) => Some(local_transport::default_working_directory()?),
    (None, false) => None,
  };
  let stream = transport::connect(&request.target).await?;
  let response = rmux_request(
    stream,
    &client_identity(),
    ClientMessage::CreateSession {
      name: None,
      command: None,
      working_directory,
      terminal_size,
    },
  )
  .await
  .map_err(CommandErrorDto::client)?;
  match response {
    ServerMessage::SessionCreated { session } => Ok(SessionDto::new(session, request.target)),
    response => Err(unexpected_response("session_created", &response)),
  }
}

#[tauri::command]
pub async fn kill_session(request: KillSessionRequestDto) -> CommandResult<()> {
  let stream = transport::connect(&request.target).await?;
  let response = rmux_request(
    stream,
    &client_identity(),
    ClientMessage::KillSession {
      session: request.session_id,
    },
  )
  .await
  .map_err(CommandErrorDto::client)?;
  match response {
    ServerMessage::Success => Ok(()),
    response => Err(unexpected_response("success", &response)),
  }
}

/// Gracefully replaces the local `rmuxd` process after terminating all of its
/// sessions through its owner-only local-control endpoint.
///
/// It first probes the endpoint without touching the active attachment. A
/// legacy daemon therefore returns a typed unsupported error without being
/// detached. Only a daemon that advertises cooperative restart is detached
/// before the destructive request, which clears any pending presentation ACK
/// and lets the daemon drain naturally without PID signals or live-socket
/// removal.
#[tauri::command]
pub async fn restart_local_daemon(
  window: WebviewWindow,
  state: State<'_, AppState>,
) -> CommandResult<RestartLocalDaemonResponseDto> {
  let daemon_restart_transition = state.daemon_restart_transition();
  let _daemon_restart_guard = daemon_restart_transition.lock().await;

  let preflight = local_transport::preflight_restart_daemon().await?;
  if preflight.requires_attachment_detach() {
    let window_label = window.label().to_owned();
    let window_transition = state.window_transition(&window_label).await;
    let _window_transition_guard = window_transition.lock().await;
    state.detach_active_local_window(&window_label).await?;
  }

  let outcome = local_transport::restart_daemon(preflight).await?;
  Ok(RestartLocalDaemonResponseDto {
    terminated_sessions: outcome.terminated_sessions,
  })
}

#[tauri::command]
pub async fn open_attachment(
  window: WebviewWindow,
  state: State<'_, AppState>,
  request: OpenAttachmentRequestDto,
  on_event: Channel<AttachmentEventDto>,
) -> CommandResult<OpenAttachmentResponseDto> {
  let attachment_id = uuid::Uuid::new_v4().to_string();
  let window_label = window.label().to_owned();
  let transition = state.window_transition(&window_label).await;
  let _transition_guard = transition.lock().await;
  state.detach_active_window(&window_label).await?;
  state.reserve_window(&window_label, &attachment_id).await?;

  let result = open_reserved_attachment(
    state.inner().clone(),
    window_label.clone(),
    attachment_id.clone(),
    request,
    on_event,
  )
  .await;
  if result.is_err() {
    state.release(&window_label, &attachment_id).await;
  }
  result
}

async fn open_reserved_attachment(
  state: AppState,
  window_label: String,
  attachment_id: String,
  request: OpenAttachmentRequestDto,
  on_event: Channel<AttachmentEventDto>,
) -> CommandResult<OpenAttachmentResponseDto> {
  let target = request.target.clone();
  let terminal_size = request.terminal_size.into_proto()?;
  let resume_from = parse_sequence(request.resume_from)?;
  let stream = transport::connect(&target).await?;
  let (stream, attached) = begin_attach(
    stream,
    &client_identity(),
    AttachRequest {
      session: request.session,
      resume_from,
      terminal_size,
      request_input_lease: request.request_input_lease,
      request_layout_lease: request.request_layout_lease,
      request_command_line: false,
      request_running_command: true,
      presentation_window_bytes: DEFAULT_PRESENTATION_WINDOW_BYTES,
    },
  )
  .await
  .map_err(CommandErrorDto::client)?;

  let options = AttachmentControllerOptions {
    // This bridge is paired with the GUI's xterm presenter, which always
    // creates a renderer at the authoritative attached grid before draining
    // queued events. An initial checkpoint still invalidates the cursor until
    // the frontend applies and acknowledges that checkpoint.
    renderer_starts_compatible: true,
    // Lease buttons express the user's current intent. Automatic reacquire
    // would immediately undo an explicit release on the next heartbeat.
    reacquire_input_lease: false,
    reacquire_layout_lease: false,
    resize_after_layout_reacquire: None,
    ..AttachmentControllerOptions::default()
  };
  let (controller, control, events) =
    AttachmentController::new(stream, &attached, options).map_err(CommandErrorDto::client)?;
  let response = OpenAttachmentResponseDto::new(attachment_id.clone(), &attached, target.clone());
  let actor = Arc::new(AttachmentActor::new(
    attachment_id.clone(),
    window_label.clone(),
    target,
    control,
  ));
  state
    .activate(&window_label, &attachment_id, Arc::clone(&actor))
    .await?;

  tauri::async_runtime::spawn(forward_attachment_events(
    state,
    actor,
    events,
    on_event,
    controller.run(),
  ));
  Ok(response)
}

#[tauri::command]
pub async fn send_input(
  window: WebviewWindow,
  state: State<'_, AppState>,
  request: SendInputRequestDto,
) -> CommandResult<()> {
  let actor = state.actor(window.label(), &request.attachment_id).await?;
  let data = decode_input(&request.data_base64)?;
  actor
    .control
    .input(data)
    .await
    .map_err(CommandErrorDto::backend)
}

#[tauri::command]
pub async fn resize_attachment(
  window: WebviewWindow,
  state: State<'_, AppState>,
  request: ResizeAttachmentRequestDto,
) -> CommandResult<()> {
  let actor = state.actor(window.label(), &request.attachment_id).await?;
  let terminal_size = request.terminal_size.into_proto()?;
  actor
    .control
    .resize(terminal_size)
    .await
    .map_err(CommandErrorDto::backend)
}

#[tauri::command]
pub async fn acquire_attachment_lease(
  window: WebviewWindow,
  state: State<'_, AppState>,
  request: AttachmentLeaseRequestDto,
) -> CommandResult<()> {
  let actor = state.actor(window.label(), &request.attachment_id).await?;
  actor
    .control
    .acquire_lease(request.lease.into())
    .await
    .map_err(CommandErrorDto::backend)
}

#[tauri::command]
pub async fn release_attachment_lease(
  window: WebviewWindow,
  state: State<'_, AppState>,
  request: AttachmentLeaseRequestDto,
) -> CommandResult<()> {
  let actor = state.actor(window.label(), &request.attachment_id).await?;
  actor
    .control
    .release_lease(request.lease.into())
    .await
    .map_err(CommandErrorDto::backend)
}

#[tauri::command]
pub async fn acknowledge_attachment_event(
  window: WebviewWindow,
  state: State<'_, AppState>,
  request: AcknowledgeAttachmentEventRequestDto,
) -> CommandResult<()> {
  let actor = state.actor(window.label(), &request.attachment_id).await?;
  actor.acknowledge(&request.event_id).await
}

#[tauri::command]
pub async fn detach_attachment(
  window: WebviewWindow,
  state: State<'_, AppState>,
  request: AttachmentRequestDto,
) -> CommandResult<()> {
  let transition = state.window_transition(window.label()).await;
  let _transition_guard = transition.lock().await;
  let actor = state.actor(window.label(), &request.attachment_id).await?;
  actor.detach_and_wait().await
}

fn client_identity() -> ClientIdentity {
  ClientIdentity {
    name: CLIENT_NAME.into(),
    version: CLIENT_VERSION.into(),
  }
}

fn unexpected_response(expected: &str, _actual: &ServerMessage) -> CommandErrorDto {
  CommandErrorDto::new(
    "unexpected_rmux_response",
    format!("expected {expected}, received another response type"),
  )
}
