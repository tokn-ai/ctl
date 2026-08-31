use std::sync::Arc;

use rmux_client::{
  AttachRequest, AttachmentController, AttachmentControllerOptions, ClientIdentity, begin_attach,
  request as rmux_request,
};
use rmux_proto::{ClientMessage, ServerMessage};
use tauri::ipc::Channel;
use tauri::{State, WebviewWindow};

use crate::dto::{
  AcknowledgeAttachmentEventRequestDto, AttachmentEventDto, AttachmentLeaseRequestDto,
  AttachmentRequestDto, CreateSessionRequestDto, KillSessionRequestDto, OpenAttachmentRequestDto,
  OpenAttachmentResponseDto, ResizeAttachmentRequestDto, SendInputRequestDto, SessionDto,
  decode_input, parse_sequence,
};
use crate::error::{CommandErrorDto, CommandResult};
use crate::local_transport;
use crate::state::{AppState, AttachmentActor, forward_attachment_events};

const CLIENT_NAME: &str = "rmux-gui";
const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[tauri::command]
pub async fn list_sessions() -> CommandResult<Vec<SessionDto>> {
  let stream = local_transport::connect().await?;
  let response = rmux_request(stream, &client_identity(), ClientMessage::ListSessions)
    .await
    .map_err(CommandErrorDto::client)?;
  match response {
    ServerMessage::SessionList { sessions } => {
      Ok(sessions.into_iter().map(SessionDto::from).collect())
    }
    response => Err(unexpected_response("session_list", &response)),
  }
}

#[tauri::command]
pub async fn create_session(request: CreateSessionRequestDto) -> CommandResult<SessionDto> {
  let terminal_size = request.terminal_size.into_proto()?;
  let working_directory = match request.working_directory {
    Some(directory) => directory,
    None => local_transport::default_working_directory()?,
  };
  let stream = local_transport::connect().await?;
  let response = rmux_request(
    stream,
    &client_identity(),
    ClientMessage::CreateSession {
      name: None,
      command: None,
      working_directory: Some(working_directory),
      terminal_size,
    },
  )
  .await
  .map_err(CommandErrorDto::client)?;
  match response {
    ServerMessage::SessionCreated { session } => Ok(session.into()),
    response => Err(unexpected_response("session_created", &response)),
  }
}

#[tauri::command]
pub async fn kill_session(request: KillSessionRequestDto) -> CommandResult<()> {
  let stream = local_transport::connect().await?;
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
  let terminal_size = request.terminal_size.into_proto()?;
  let resume_from = parse_sequence(request.resume_from)?;
  let stream = local_transport::connect().await?;
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
  let response = OpenAttachmentResponseDto::new(attachment_id.clone(), &attached);
  let actor = Arc::new(AttachmentActor::new(
    attachment_id.clone(),
    window_label.clone(),
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
