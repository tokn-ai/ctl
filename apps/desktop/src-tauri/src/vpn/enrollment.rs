//! Window-owned, unsaved Tailscale connections with explicit save or discard.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use ctl_ipc::{VpnConnection, VpnProvider, VpnSettings, VpnSnapshot, VpnState, VpnStatus};
use tokio::sync::Mutex as AsyncMutex;

use super::coordinator::{Coordinator, cancelled};
use super::models::{BeginVpnEnrollmentRequest, VpnEnrollmentSnapshot};
use super::{COORDINATORS, Repository, VpnConnectionsSnapshot, client, runtime_error};
use crate::error::{CommandErrorDto, CommandResult};

type RuntimeFuture<'a, T> = Pin<Box<dyn Future<Output = CommandResult<T>> + Send + 'a>>;
const CLOSED_WINDOW_CLEANUP_GRACE: Duration = Duration::from_secs(16);
const CLOSED_WINDOW_CLEANUP_DEADLINE: Duration = Duration::from_secs(25);

trait Runtime: Send + Sync {
  fn supported(&self) -> RuntimeFuture<'_, ()>;
  fn start(&self, connection: VpnConnection) -> RuntimeFuture<'_, VpnStatus>;
  fn list(&self) -> RuntimeFuture<'_, VpnSnapshot>;
  fn stop(&self, connection_id: &str) -> RuntimeFuture<'_, VpnStatus>;
  fn forget(&self, connection_id: &str) -> RuntimeFuture<'_, ()>;
}

struct NativeRuntime(ctl_ipc::vpn::Client);

impl Runtime for NativeRuntime {
  fn supported(&self) -> RuntimeFuture<'_, ()> {
    Box::pin(async {
      self
        .0
        .ensure_tailscale_enrollment_supported()
        .await
        .map_err(runtime_error)
    })
  }

  fn start(&self, connection: VpnConnection) -> RuntimeFuture<'_, VpnStatus> {
    Box::pin(async {
      self
        .0
        .start_connection(connection)
        .await
        .map_err(runtime_error)
    })
  }

  fn list(&self) -> RuntimeFuture<'_, VpnSnapshot> {
    Box::pin(async { self.0.list().await.map_err(runtime_error) })
  }

  fn stop(&self, connection_id: &str) -> RuntimeFuture<'_, VpnStatus> {
    let connection_id = connection_id.to_owned();
    Box::pin(async move { self.0.stop_id(&connection_id).await.map_err(runtime_error) })
  }

  fn forget(&self, connection_id: &str) -> RuntimeFuture<'_, ()> {
    let connection_id = connection_id.to_owned();
    Box::pin(async move {
      self
        .0
        .forget_tailscale_identity(&connection_id)
        .await
        .map_err(runtime_error)
    })
  }
}

#[derive(Default)]
pub(super) struct Enrollments {
  entries: Arc<Mutex<HashMap<String, Arc<Enrollment>>>>,
}

struct Enrollment {
  id: String,
  window_label: String,
  directory: PathBuf,
  connection: VpnConnection,
  runtime: Arc<dyn Runtime>,
  coordinator: Arc<Coordinator>,
  // Saving and cancelling must be serialized separately from startup: Cancel
  // must interrupt startup, but must never interrupt a successful Save.
  operations: Arc<AsyncMutex<()>>,
  state: Arc<Mutex<EnrollmentState>>,
}

struct EnrollmentState {
  starting: bool,
  dispatched: bool,
  lifecycle: Lifecycle,
  error: Option<CommandErrorDto>,
  cleanup_retry_scheduled: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lifecycle {
  Draft,
  Discarding,
  Adopted,
}

impl Enrollments {
  pub(super) fn begin(
    &self,
    directory: PathBuf,
    window_label: String,
    request: BeginVpnEnrollmentRequest,
  ) -> CommandResult<VpnEnrollmentSnapshot> {
    self.begin_with(
      directory,
      window_label,
      request,
      Arc::new(NativeRuntime(client::client().map_err(runtime_error)?)),
    )
  }

  fn begin_with(
    &self,
    directory: PathBuf,
    window_label: String,
    request: BeginVpnEnrollmentRequest,
    runtime: Arc<dyn Runtime>,
  ) -> CommandResult<VpnEnrollmentSnapshot> {
    let connection_id = uuid::Uuid::new_v4().to_string();
    let connection = VpnConnection {
      connection_id: connection_id.clone(),
      name: request.name,
      settings: VpnSettings::Tailscale {
        hostname: request.hostname,
        accept_routes: request.accept_routes,
      },
    };
    connection
      .validate()
      .map_err(|message| CommandErrorDto::new("vpn_invalid_connection", message))?;
    let enrollment_id = uuid::Uuid::new_v4().to_string();
    let enrollment = Arc::new(Enrollment {
      id: enrollment_id.clone(),
      window_label,
      directory,
      coordinator: COORDINATORS.get(&connection_id),
      connection,
      runtime,
      operations: Arc::new(AsyncMutex::new(())),
      state: Arc::new(Mutex::new(EnrollmentState {
        starting: true,
        dispatched: false,
        lifecycle: Lifecycle::Draft,
        error: None,
        cleanup_retry_scheduled: false,
      })),
    });
    let initial = enrollment.snapshot(enrollment.pending(), None);
    {
      let mut entries = lock(&self.entries);
      if entries
        .values()
        .filter(|entry| lock(&entry.state).lifecycle != Lifecycle::Adopted)
        .count()
        >= 16
      {
        return Err(CommandErrorDto::new(
          "vpn_enrollment_limit",
          "Finish or cancel another VPN sign-in before starting a new one.",
        ));
      }
      entries.insert(enrollment_id, Arc::clone(&enrollment));
    }
    tauri::async_runtime::spawn(async move {
      enrollment.start().await;
    });
    Ok(initial)
  }

  fn get(&self, window_label: &str, enrollment_id: &str) -> CommandResult<Arc<Enrollment>> {
    lock(&self.entries)
      .get(enrollment_id)
      .filter(|entry| entry.window_label == window_label)
      .cloned()
      .ok_or_else(|| {
        CommandErrorDto::new(
          "vpn_enrollment_not_found",
          "This VPN sign-in is no longer available. Start it again.",
        )
      })
  }

  pub(super) async fn status(
    &self,
    window_label: &str,
    enrollment_id: &str,
  ) -> CommandResult<VpnEnrollmentSnapshot> {
    self.get(window_label, enrollment_id)?.status().await
  }

  pub(super) async fn save(
    &self,
    window_label: &str,
    enrollment_id: &str,
    expected_revision: Option<String>,
  ) -> CommandResult<VpnConnectionsSnapshot> {
    self
      .get(window_label, enrollment_id)?
      .save(expected_revision)
      .await
  }

  pub(super) async fn cancel(&self, window_label: &str, enrollment_id: &str) -> CommandResult<()> {
    let enrollment = self.get(window_label, enrollment_id)?;
    enrollment.cancel().await?;
    lock(&self.entries).remove(enrollment_id);
    Ok(())
  }

  pub(super) async fn close_window(&self, window_label: &str) {
    let ids: Vec<_> = lock(&self.entries)
      .values()
      .filter(|entry| entry.window_label == window_label)
      .map(|entry| entry.id.clone())
      .collect();
    for id in ids {
      // Keep failed cleanup registered and leave identity intact if storage or
      // the owner is unavailable. A closed app must never guess that it is safe
      // to discard a profile whose save result was interrupted.
      if let Err(error) = self.cancel(window_label, &id).await
        && error.code == "vpn_cleanup_pending"
        && let Ok(enrollment) = self.get(window_label, &id)
      {
        let schedule = {
          let mut state = lock(&enrollment.state);
          if state.cleanup_retry_scheduled {
            false
          } else {
            state.cleanup_retry_scheduled = true;
            true
          }
        };
        if !schedule {
          continue;
        }
        let entries = Arc::clone(&self.entries);
        tokio::spawn(async move {
          retry_closed_enrollment(&entries, &enrollment).await;
          lock(&enrollment.state).cleanup_retry_scheduled = false;
        });
      }
    }
  }
}

impl Enrollment {
  async fn start(&self) {
    let result = self
      .coordinator
      .connect(|cancellation| async move {
        if lock(&self.state).lifecycle == Lifecycle::Discarding {
          return Err(cancelled());
        }
        self.runtime.supported().await?;
        cancellation.check()?;
        {
          let mut state = lock(&self.state);
          if state.lifecycle == Lifecycle::Discarding {
            return Err(cancelled());
          }
          state.dispatched = true;
        }
        let result = self.runtime.start(self.connection.clone()).await;
        cancellation.check()?;
        result
      })
      .await;
    let mut state = lock(&self.state);
    state.starting = false;
    if state.lifecycle != Lifecycle::Discarding {
      state.error = result.err();
    }
  }

  fn pending(&self) -> VpnStatus {
    VpnStatus {
      provider: VpnProvider::Tailscale,
      vpn_id: Some(self.connection.connection_id.clone()),
      connection_id: Some(self.connection.connection_id.clone()),
      state: VpnState::Starting,
      message: Some("Starting Tailscale and preparing browser sign-in…".into()),
      ..VpnStatus::default()
    }
  }

  fn snapshot(&self, status: VpnStatus, error: Option<CommandErrorDto>) -> VpnEnrollmentSnapshot {
    VpnEnrollmentSnapshot {
      enrollment_id: self.id.clone(),
      connection_id: self.connection.connection_id.clone(),
      status,
      error,
    }
  }

  async fn status(&self) -> CommandResult<VpnEnrollmentSnapshot> {
    let snapshot = self.runtime.list().await?;
    let incomplete = !snapshot.discovery_warnings.is_empty();
    let state = lock(&self.state);
    let mut error = state.error.clone();
    let mut status = snapshot.connections.into_iter()
      .find(|status| matches_connection(status, &self.connection.connection_id))
      .unwrap_or_else(|| {
        let mut pending = self.pending();
        if state.lifecycle == Lifecycle::Discarding { pending.state = VpnState::Stopping; }
        else if !state.starting && !incomplete {
          pending.state = VpnState::Stopped;
          pending.message = None;
          if error.is_none() && state.lifecycle != Lifecycle::Adopted {
            error = Some(CommandErrorDto::new("vpn_enrollment_disconnected", "Tailscale disconnected before this connection was saved. Cancel and start sign-in again."));
          }
        }
        pending
      });
    if state.lifecycle == Lifecycle::Discarding {
      status.state = VpnState::Stopping;
      status.auth_url = None;
    }
    if incomplete {
      status.status_unavailable = true;
      status.auth_url = None;
      status.message =
        Some("Tailscale container status is unavailable. Refresh to check it again.".into());
    }
    Ok(self.snapshot(status, error))
  }

  async fn save(&self, expected_revision: Option<String>) -> CommandResult<VpnConnectionsSnapshot> {
    self
      .save_with(expected_revision, |directory, revision, connection| {
        Repository::new(directory).save_enrollment(revision.as_deref(), connection)
      })
      .await
  }

  async fn save_with<F>(
    &self,
    expected_revision: Option<String>,
    persist: F,
  ) -> CommandResult<VpnConnectionsSnapshot>
  where
    F: FnOnce(PathBuf, Option<String>, VpnConnection) -> CommandResult<VpnConnectionsSnapshot>
      + Send
      + 'static,
  {
    let operation = Arc::clone(&self.operations).lock_owned().await;
    if lock(&self.state).lifecycle == Lifecycle::Adopted {
      return self.load().await;
    }
    if lock(&self.state).lifecycle == Lifecycle::Discarding {
      return Err(cancelled());
    }
    let _change = self.coordinator.changes.lock().await;
    let snapshot = self.runtime.list().await?;
    if !snapshot.discovery_warnings.is_empty()
      || snapshot.connections.iter().any(|status| {
        matches_connection(status, &self.connection.connection_id) && status.status_unavailable
      })
    {
      return Err(CommandErrorDto::new(
        "vpn_discovery_incomplete",
        "Tailscale status could not be confirmed. Refresh before saving this connection.",
      ));
    }
    if !snapshot.connections.iter().any(|status| {
      matches_connection(status, &self.connection.connection_id)
        && status.state == VpnState::Connected
        && status.running
        && status.endpoint.is_some()
    }) {
      return Err(CommandErrorDto::new(
        "vpn_sign_in_required",
        "Finish Tailscale sign-in and any device approval before saving this connection.",
      ));
    }
    let directory = self.directory.clone();
    let connection = self.connection.clone();
    let state = Arc::clone(&self.state);
    let saved = tauri::async_runtime::spawn_blocking(move || {
      // The worker owns cancellation serialization through the actual commit,
      // even if the caller/window disappears while the blocking write runs.
      let _operation = operation;
      let saved = persist(directory, expected_revision, connection)?;
      lock(&state).lifecycle = Lifecycle::Adopted;
      Ok::<_, CommandErrorDto>(saved)
    })
    .await
    .map_err(super::task_error)??;
    Ok(saved)
  }

  async fn load(&self) -> CommandResult<VpnConnectionsSnapshot> {
    let directory = self.directory.clone();
    tauri::async_runtime::spawn_blocking(move || Repository::new(directory).load())
      .await
      .map_err(super::task_error)?
  }

  async fn persisted(&self) -> CommandResult<bool> {
    let directory = self.directory.clone();
    let connection_id = self.connection.connection_id.clone();
    tauri::async_runtime::spawn_blocking(move || {
      Repository::new(directory).contains(&connection_id)
    })
    .await
    .map_err(super::task_error)?
  }

  async fn cancel(&self) -> CommandResult<()> {
    let _operation = self.operations.lock().await;
    // A late UI cleanup or interrupted successful write must not stop or forget
    // an adopted connection. Read persisted IDs before making any runtime change.
    if lock(&self.state).lifecycle == Lifecycle::Adopted || self.persisted().await? {
      lock(&self.state).lifecycle = Lifecycle::Adopted;
      return Ok(());
    }
    lock(&self.state).lifecycle = Lifecycle::Discarding;
    self
      .coordinator
      .stop(|| async {
        if lock(&self.state).dispatched {
          self.runtime.stop(&self.connection.connection_id).await
        } else {
          Ok(VpnStatus::default())
        }
      })
      .await?;
    if !lock(&self.state).dispatched {
      // Capability checks and validation do not create a container or identity.
      // Closing their error state must not bootstrap an owner or invoke an API
      // that the preflight already established is unsupported.
      return Ok(());
    }
    // Recheck after waiting for startup/stop, since a saved profile may have
    // appeared in another window or through an external catalog write.
    if self.persisted().await? {
      lock(&self.state).lifecycle = Lifecycle::Adopted;
      return Ok(());
    }
    let inventory = self.runtime.list().await?;
    if !inventory.discovery_warnings.is_empty()
      || inventory.connections.iter().any(|status| {
        matches_connection(status, &self.connection.connection_id) && status.status_unavailable
      })
    {
      return Err(CommandErrorDto::new(
        "vpn_cleanup_pending",
        "The VPN connection was released, but container inventory is unavailable. Retry cancellation once its status can be checked.",
      ));
    }
    if inventory.connections.iter().any(|status| {
      matches_connection(status, &self.connection.connection_id)
        && (status.running || status.state != VpnState::Stopped)
    }) {
      // Releasing our heartbeat does not stop a container kept alive elsewhere.
      // Keep the draft registered and its identity intact until passive expiry.
      return Err(CommandErrorDto::new(
        "vpn_cleanup_pending",
        "The VPN connection was released, but its shared container is still running. Wait for it to expire, then retry cancellation to remove the unsaved identity.",
      ));
    }
    self.runtime.forget(&self.connection.connection_id).await
  }
}

async fn retry_closed_enrollment(
  entries: &Mutex<HashMap<String, Arc<Enrollment>>>,
  enrollment: &Arc<Enrollment>,
) {
  // Let the watchdog expire our released heartbeat before trying identity removal.
  // The process may exit before this best-effort task finishes; preserving the
  // identity is safer than removing a shared container or an interrupted save.
  let _ = tokio::time::timeout(CLOSED_WINDOW_CLEANUP_DEADLINE, async {
    tokio::time::sleep(CLOSED_WINDOW_CLEANUP_GRACE).await;
    loop {
      if !lock(entries)
        .get(&enrollment.id)
        .is_some_and(|current| Arc::ptr_eq(current, enrollment))
      {
        return;
      }
      match enrollment.cancel().await {
        Ok(()) => {
          lock(entries).remove(&enrollment.id);
          return;
        }
        Err(error) if error.code == "vpn_cleanup_pending" => {
          tokio::time::sleep(Duration::from_secs(1)).await;
        }
        Err(_) => return,
      }
    }
  })
  .await;
}

fn matches_connection(status: &VpnStatus, connection_id: &str) -> bool {
  status.provider == VpnProvider::Tailscale
    && status.vpn_id.as_deref() == Some(connection_id)
    && status.connection_id.as_deref() == Some(connection_id)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
  mutex
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
