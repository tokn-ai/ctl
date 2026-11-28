//! A live application lane reaches a handoff barrier only between complete
//! records. Input drains first so watchers can deliver every resulting event.
use crate::desktop_input::DriverCheckpoint;
use serde::{Deserialize, Serialize};
use std::io;
use tokio::sync::watch;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::daemon) enum Phase {
  Running,
  DrainInput,
  Freeze,
  Commit,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::daemon) struct LaneState {
  pub(super) driver: Option<DriverCheckpoint>,
  pub(super) expired_hold: bool,
}

#[derive(Clone)]
struct Status {
  phase: Phase,
  reached: Phase,
  saved: LaneState,
  closed: bool,
  cycle: u64,
}

#[derive(Clone)]
pub(in crate::daemon) struct Gate {
  id: Option<Uuid>,
  resumed: Option<LaneState>,
  status: watch::Sender<Status>,
}

impl Gate {
  fn with_id(id: Option<Uuid>, resumed: Option<LaneState>) -> Self {
    Self {
      id,
      resumed,
      status: watch::channel(Status {
        phase: Phase::Running,
        reached: Phase::Running,
        saved: LaneState::default(),
        closed: false,
        cycle: 0,
      })
      .0,
    }
  }

  pub(in crate::daemon) fn new(id: Uuid, resumed: Option<LaneState>) -> Self {
    Self::with_id(Some(id), resumed)
  }

  pub(in crate::daemon) fn legacy() -> Self {
    Self::with_id(None, None)
  }

  pub(super) fn id(&self) -> Option<Uuid> {
    self.id
  }

  pub(super) fn resumed(&self) -> bool {
    self.resumed.is_some()
  }

  pub(super) fn saved(&self) -> Option<&LaneState> {
    self.resumed.as_ref()
  }

  pub(in crate::daemon) fn phase(&self) -> Phase {
    self.status.borrow().phase
  }

  pub(in crate::daemon) fn set_phase(&self, phase: Phase) {
    self.status.send_modify(|state| {
      if phase == Phase::Running {
        state.cycle += 1;
        state.reached = Phase::Running;
        state.saved = LaneState::default();
      }
      state.phase = phase;
    });
  }

  pub(in crate::daemon) async fn wait_paused(&self, phase: Phase) -> io::Result<LaneState> {
    let mut status = self.status.subscribe();
    let state = status
      .wait_for(|s| s.closed || s.reached >= phase)
      .await
      .map_err(io::Error::other)?;
    if state.closed {
      Err(io::Error::new(
        io::ErrorKind::NotConnected,
        "handoff lane closed",
      ))
    } else {
      Ok(state.saved.clone())
    }
  }

  pub(in crate::daemon) fn finish(&self) {
    self.status.send_modify(|s| s.closed = true);
  }

  pub(in crate::daemon) async fn requested(&self, phase: Phase) {
    let mut status = self.status.subscribe();
    let _ = status.wait_for(|s| s.phase >= phase).await;
  }

  // Observers and capture lanes continue delivering while mutations drain.
  pub(in crate::daemon) fn input_idle(&self) {
    self.status.send_if_modified(|s| {
      if s.phase == Phase::DrainInput && s.reached < Phase::DrainInput {
        s.reached = Phase::DrainInput;
        true
      } else {
        false
      }
    });
  }

  // Keep the same handler and OS session alive if standby validation rolls
  // back. A new cycle invalidates the previous barrier acknowledgement.
  pub(in crate::daemon) async fn park(
    &self,
    requested: Phase,
    reached: Phase,
    saved: Option<LaneState>,
  ) -> bool {
    let cycle = self.status.borrow().cycle;
    self.status.send_modify(|s| {
      if s.phase >= requested && s.cycle == cycle {
        s.reached = s.reached.max(reached);
        if let Some(saved) = saved {
          s.saved = saved;
        }
      }
    });
    let mut status = self.status.subscribe();
    let state = status
      .wait_for(|s| s.phase == Phase::Commit || s.phase < requested || s.cycle != cycle)
      .await
      .expect("gate owns sender");
    state.phase == Phase::Commit
  }

  // The read-only output loop also has to acknowledge drainage of its input
  // (which is only used for EOF) anew after each rollback.
  pub(in crate::daemon) async fn next_output_barrier(&self) {
    let mut status = self.status.subscribe();
    loop {
      let pending = status
        .wait_for(|s| {
          s.phase >= Phase::Freeze
            || (s.phase == Phase::DrainInput && s.reached < Phase::DrainInput)
        })
        .await
        .expect("gate owns sender")
        .phase;
      if pending >= Phase::Freeze {
        return;
      }
      self.input_idle();
    }
  }
}
