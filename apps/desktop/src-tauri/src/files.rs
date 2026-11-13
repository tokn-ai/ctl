//! Window-owned read-only file operations, isolated from terminal attachments.
#![allow(clippy::needless_pass_by_value)]

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ctl_proto::files::{Request, Response};
use serde::Deserialize;
use tauri::{State, WebviewWindow};
use tokio::sync::watch;

use crate::dto::ConnectionTargetDto;
use crate::error::{CommandErrorDto, CommandResult};

const MAX_ACTIVE_REQUESTS: usize = 32;
const MAX_PENDING_CANCELLATIONS: usize = 256;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Default)]
struct WindowRequests {
  generation: u64,
  document_id: String,
  active: HashMap<String, watch::Sender<bool>>,
  pending_cancel: VecDeque<String>,
}

#[derive(Clone, Default)]
pub struct FileRequests(Arc<Mutex<HashMap<String, WindowRequests>>>);

impl FileRequests {
  pub fn page_started(&self, owner: &str) {
    let mut registry = self.0.lock().unwrap();
    let window = registry.entry(owner.into()).or_default();
    for cancel in window.active.values() {
      cancel.send_replace(true);
    }
    window.active.clear();
    window.pending_cancel.clear();
    window.generation = window.generation.wrapping_add(1);
    window.document_id = uuid::Uuid::new_v4().to_string();
  }

  pub fn close_window(&self, owner: &str) {
    if let Some(window) = self.0.lock().unwrap().remove(owner) {
      for cancel in window.active.into_values() {
        cancel.send_replace(true);
      }
    }
  }

  fn register(
    &self,
    owner: &str,
    document_id: &str,
    request_id: &str,
  ) -> CommandResult<(RequestGuard, watch::Receiver<bool>)> {
    validate_id(request_id)?;
    let mut registry = self.0.lock().unwrap();
    let window = registry.get_mut(owner).ok_or_else(closed)?;
    if window.document_id != document_id {
      return Err(closed());
    }
    if window.active.contains_key(request_id) {
      return Err(CommandErrorDto::new(
        "file_request_exists",
        "This file request is already running.",
      ));
    }
    if window.active.len() >= MAX_ACTIVE_REQUESTS {
      return Err(CommandErrorDto::new(
        "file_request_capacity",
        "Too many file requests are running in this window.",
      ));
    }
    let (cancel, cancelled) = watch::channel(false);
    if let Some(index) = window.pending_cancel.iter().position(|id| id == request_id) {
      window.pending_cancel.remove(index);
      cancel.send_replace(true);
    }
    window.active.insert(request_id.into(), cancel);
    Ok((
      RequestGuard {
        registry: self.clone(),
        owner: owner.into(),
        request_id: request_id.into(),
        generation: window.generation,
      },
      cancelled,
    ))
  }

  fn document(&self, owner: &str) -> CommandResult<String> {
    self
      .0
      .lock()
      .unwrap()
      .get(owner)
      .map(|window| window.document_id.clone())
      .ok_or_else(closed)
  }

  fn cancel(&self, owner: &str, document_id: &str, request_id: &str) {
    if validate_id(request_id).is_err() {
      return;
    }
    let mut registry = self.0.lock().unwrap();
    let Some(window) = registry.get_mut(owner) else {
      return;
    };
    if window.document_id != document_id {
      return;
    }
    if let Some(cancel) = window.active.get(request_id) {
      cancel.send_replace(true);
    } else if !window.pending_cancel.iter().any(|id| id == request_id) {
      if window.pending_cancel.len() == MAX_PENDING_CANCELLATIONS {
        window.pending_cancel.pop_front();
      }
      window.pending_cancel.push_back(request_id.into());
    }
  }
}

struct RequestGuard {
  registry: FileRequests,
  owner: String,
  request_id: String,
  generation: u64,
}

impl Drop for RequestGuard {
  fn drop(&mut self) {
    if let Some(window) = self.registry.0.lock().unwrap().get_mut(&self.owner)
      && window.generation == self.generation
    {
      window.active.remove(&self.request_id);
    }
  }
}

fn validate_id(value: &str) -> CommandResult<()> {
  uuid::Uuid::parse_str(value)
    .map(|_| ())
    .map_err(|_| CommandErrorDto::new("invalid_request", "File requests need a valid request ID."))
}

fn closed() -> CommandErrorDto {
  CommandErrorDto::new(
    "file_request_cancelled",
    "The file request's window has closed.",
  )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRequest {
  document_id: String,
  request_id: String,
  target: ConnectionTargetDto,
  operation: Request,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelRequest {
  document_id: String,
  request_id: String,
}

#[tauri::command]
pub fn file_document(
  window: WebviewWindow,
  state: State<'_, FileRequests>,
) -> CommandResult<String> {
  state.document(window.label())
}

#[tauri::command]
pub async fn file_request(
  window: WebviewWindow,
  state: State<'_, FileRequests>,
  request: FileRequest,
) -> CommandResult<Response> {
  let (_guard, mut cancelled) =
    state.register(window.label(), &request.document_id, &request.request_id)?;
  if *cancelled.borrow() {
    return Err(closed());
  }
  tokio::select! {
    biased;
    _ = cancelled.changed() => Err(closed()),
    result = tokio::time::timeout(REQUEST_TIMEOUT, execute(request.target, request.operation)) => {
      result.map_err(|_| CommandErrorDto::new("file_request_timeout", "The file request timed out."))?
    },
  }
}

#[tauri::command]
pub fn cancel_file_request(
  window: WebviewWindow,
  state: State<'_, FileRequests>,
  request: CancelRequest,
) {
  state.cancel(window.label(), &request.document_id, &request.request_id);
}

async fn execute(target: ConnectionTargetDto, operation: Request) -> CommandResult<Response> {
  if target.is_local() {
    return ctl_files::execute(operation)
      .await
      .map_err(CommandErrorDto::backend);
  }
  let ctl_client::ConnectionTarget::Ssh {
    destination,
    options,
  } = target.to_core()
  else {
    return Err(CommandErrorDto::new(
      "invalid_ssh_target",
      "Select an SSH host.",
    ));
  };
  #[cfg(unix)]
  let interaction = ctl_client::SshInteraction::Brokered {
    target: target.to_ssh_target()?,
    control_path: crate::ssh_auth::existing_master(&target).await?,
  };
  #[cfg(not(unix))]
  let interaction = ctl_client::SshInteraction::Batch;
  let mut stream = ctl_client::files::open(&destination, &options, &interaction)
    .await
    .map_err(|error| CommandErrorDto::transport(&error))?;
  let identity = stream.remote_identity.as_ref().ok_or_else(|| {
    CommandErrorDto::new(
      "invalid_remote_identity",
      "The file service did not identify its remote account.",
    )
  })?;
  target.verify_remote_identity(identity)?;
  ctl_proto::files::request(&mut stream, &operation)
    .await
    .map_err(CommandErrorDto::backend)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn id() -> String {
    uuid::Uuid::new_v4().to_string()
  }

  #[tokio::test]
  async fn cancellation_is_scoped_and_handles_early_delivery() {
    let state = FileRequests::default();
    state.page_started("first");
    state.page_started("second");
    let first_document = state.document("first").unwrap();
    let second_document = state.document("second").unwrap();
    let request_id = id();
    state.cancel("first", &first_document, &request_id);
    let (_first, first) = state
      .register("first", &first_document, &request_id)
      .unwrap();
    let (_second, second) = state
      .register("second", &second_document, &request_id)
      .unwrap();
    assert!(*first.borrow());
    assert!(!*second.borrow());
    let sibling_id = id();
    let (_sibling, sibling) = state
      .register("second", &second_document, &sibling_id)
      .unwrap();
    state.cancel("second", &second_document, &request_id);
    assert!(*second.borrow());
    assert!(!*sibling.borrow());
  }

  #[tokio::test]
  async fn reload_and_destroy_cancel_owned_requests_and_reject_stale_owners() {
    let state = FileRequests::default();
    state.page_started("preview");
    let old_document = state.document("preview").unwrap();
    let request_id = id();
    let (old, cancelled) = state
      .register("preview", &old_document, &request_id)
      .unwrap();
    state.page_started("preview");
    let new_document = state.document("preview").unwrap();
    assert!(*cancelled.borrow());
    assert!(state.register("preview", &old_document, &id()).is_err());
    let (_new, current) = state
      .register("preview", &new_document, &request_id)
      .unwrap();
    state.cancel("preview", &old_document, &request_id);
    assert!(!*current.borrow());
    drop(old);
    assert!(
      state
        .register("preview", &new_document, &request_id)
        .is_err()
    );
    state.close_window("preview");
    assert!(*current.borrow());
    assert!(state.register("preview", &new_document, &id()).is_err());
  }

  #[test]
  fn bounds_active_work_and_pending_cancel_memory() {
    let state = FileRequests::default();
    state.page_started("main");
    let document_id = state.document("main").unwrap();
    let guards: Vec<_> = (0..MAX_ACTIVE_REQUESTS)
      .map(|_| state.register("main", &document_id, &id()).unwrap())
      .collect();
    assert!(state.register("main", &document_id, &id()).is_err());
    for _ in 0..MAX_PENDING_CANCELLATIONS + 10 {
      state.cancel("main", &document_id, &id());
    }
    assert_eq!(
      state.0.lock().unwrap()["main"].pending_cancel.len(),
      MAX_PENDING_CANCELLATIONS
    );
    drop(guards);
    assert!(state.register("main", &document_id, &id()).is_ok());
  }
}
