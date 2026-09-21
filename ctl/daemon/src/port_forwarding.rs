//! A forward has one owner even when its host's SSH connection method changes.

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::future::Future;

use ctld_ipc::{LocalPortForward, PortForwardState, PortForwardStatus, SshTarget};

use super::{RequestError, control_master_is_ready, control_path, run_forward_command};

struct OwnedForward {
  target: SshTarget,
  status: PortForwardStatus,
  // Status polls may report an unavailable master without proving that its
  // listener stopped. Keep cancellation responsibility independently.
  listener_present: bool,
}

/// Callers hold `State::forwards` throughout each operation, including control
/// commands. Master activation cannot replay a stale list after a move/disable.
#[derive(Default)]
pub(super) struct ForwardRegistry {
  records: HashMap<String, OwnedForward>,
  paused: HashSet<SshTarget>,
}

pub(super) trait ForwardControl: Sync {
  fn is_ready(&self, target: &SshTarget) -> impl Future<Output = bool> + Send;

  fn change(
    &self,
    target: &SshTarget,
    forward: &LocalPortForward,
    cancel: bool,
  ) -> impl Future<Output = Result<(), RequestError>> + Send;
}

pub(super) struct SshForwardControl;

impl ForwardControl for SshForwardControl {
  async fn is_ready(&self, target: &SshTarget) -> bool {
    control_master_is_ready(target, &control_path(target)).await
  }

  async fn change(
    &self,
    target: &SshTarget,
    forward: &LocalPortForward,
    cancel: bool,
  ) -> Result<(), RequestError> {
    let path = control_path(target);
    if cancel && !path.exists() {
      return Ok(());
    }
    run_forward_command(target, &path, forward, cancel).await
  }
}

impl ForwardRegistry {
  pub(super) async fn configure(
    &mut self,
    control: &impl ForwardControl,
    target: SshTarget,
    forward: LocalPortForward,
    enabled: bool,
  ) -> Result<PortForwardStatus, RequestError> {
    if let Some(existing) = self.records.get_mut(&forward.forward_id)
      && existing.listener_present
    {
      if enabled
        && !self.paused.contains(&target)
        && existing.target == target
        && existing.status.forward == forward
        && control.is_ready(&existing.target).await
      {
        existing.status = forward_status(existing.status.forward.clone(), Ok(()));
        return Ok(existing.status.clone());
      }
      // Use the retained owner and definition, including an edited/deleted
      // connection method. A failed cancellation must retain that ownership.
      control
        .change(&existing.target, &existing.status.forward, true)
        .await?;
    }

    if !enabled {
      self.records.remove(&forward.forward_id);
      return Ok(PortForwardStatus {
        forward,
        state: PortForwardState::WaitingForAuthentication,
        message: None,
      });
    }

    let status = if !self.paused.contains(&target) && control.is_ready(&target).await {
      let result = control.change(&target, &forward, false).await;
      forward_status(forward, result)
    } else {
      waiting_status(forward)
    };
    self.records.insert(
      status.forward.forward_id.clone(),
      OwnedForward {
        target,
        listener_present: status.state == PortForwardState::Active,
        status: status.clone(),
      },
    );
    Ok(status)
  }

  pub(super) async fn list(
    &mut self,
    control: &impl ForwardControl,
    target: &SshTarget,
  ) -> Vec<PortForwardStatus> {
    let ready = !self.paused.contains(target) && control.is_ready(target).await;
    let mut statuses = Vec::new();
    for record in self
      .records
      .values_mut()
      .filter(|record| record.target == *target)
    {
      if !ready {
        record.status = waiting_status(record.status.forward.clone());
      } else if record.listener_present {
        record.status = forward_status(record.status.forward.clone(), Ok(()));
      }
      statuses.push(record.status.clone());
    }
    statuses.sort_by(|left, right| left.forward.forward_id.cmp(&right.forward.forward_id));
    statuses
  }

  pub(super) async fn activate(&mut self, control: &impl ForwardControl, target: &SshTarget) {
    if self.paused.contains(target) {
      return;
    }
    for record in self
      .records
      .values_mut()
      .filter(|record| record.target == *target && !record.listener_present)
    {
      let result = control.change(target, &record.status.forward, false).await;
      record.listener_present = result.is_ok();
      record.status = forward_status(record.status.forward.clone(), result);
    }
  }

  pub(super) fn pause(&mut self, target: &SshTarget) {
    self.paused.insert(target.clone());
    for record in self
      .records
      .values_mut()
      .filter(|record| record.target == *target)
    {
      record.status = waiting_status(record.status.forward.clone());
    }
  }

  pub(super) fn resume(&mut self, target: &SshTarget) {
    self.paused.remove(target);
  }

  /// Called under the same registry lock when an unavailable control socket
  /// is replaced. New listeners may subsequently be created before auth's
  /// save-offer UI finishes; `activate()` must leave those listeners alone.
  pub(super) fn master_replaced(&mut self, target: &SshTarget) {
    for record in self
      .records
      .values_mut()
      .filter(|record| record.target == *target)
    {
      record.listener_present = false;
      record.status = waiting_status(record.status.forward.clone());
    }
  }
}

fn waiting_status(forward: LocalPortForward) -> PortForwardStatus {
  PortForwardStatus {
    forward,
    state: PortForwardState::WaitingForAuthentication,
    message: Some("Connect this host to activate the forward.".into()),
  }
}

fn forward_status(
  forward: LocalPortForward,
  result: Result<(), RequestError>,
) -> PortForwardStatus {
  match result {
    Ok(()) => PortForwardStatus {
      forward,
      state: PortForwardState::Active,
      message: None,
    },
    Err(error) => PortForwardStatus {
      forward,
      state: PortForwardState::Error,
      message: Some(error.to_string()),
    },
  }
}
