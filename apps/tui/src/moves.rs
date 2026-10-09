use super::{App, KeyState, Overlay};
use crate::Result;
use ctmux_proto::{LeaseKind, PaneMoveOutcome, PaneTarget, ViewInfo};
use std::time::Duration;
use tokio::time::Instant;

pub(super) struct PendingMove {
  owner: String,
  source_session: String,
  source_view: String,
  action: Move,
  deadline: Instant,
  source_snapshot: Option<ViewInfo>,
  promoted_snapshot: Option<ViewInfo>,
}

enum Move {
  Swap { focus: String },
  Break { terminal: String, detached: bool },
}

impl PendingMove {
  pub(super) fn promoting(&self, id: &str) -> bool {
    matches!(&self.action, Move::Break { terminal, .. } if terminal == id)
  }

  pub(super) fn observe_view(&mut self, view: &ViewInfo) {
    if view.session_id == self.source_session && view.view_id == self.source_view {
      if self
        .source_snapshot
        .as_ref()
        .is_none_or(|saved| saved.revision <= view.revision)
      {
        self.source_snapshot = Some(view.clone());
      }
    } else if let Move::Break { terminal, .. } = &self.action
      && view
        .terminals
        .iter()
        .any(|pane| &pane.terminal_id == terminal)
      && self.promoted_snapshot.as_ref().is_none_or(|saved| {
        saved.session_id != view.session_id
          || saved.view_id != view.view_id
          || saved.revision <= view.revision
      })
    {
      self.promoted_snapshot = Some(view.clone());
    }
  }
}

impl App<'_> {
  fn move_target(&self) -> Result<(String, PaneTarget)> {
    if self.pane_move.is_some() {
      return Err("Waiting for the previous pane move".into());
    }
    let view = self.view.as_ref().ok_or("No session to rearrange")?;
    let focused = self
      .panes
      .get(&self.focused)
      .ok_or("No pane to rearrange")?;
    if !focused.connected || focused.ended.is_some() {
      return Err("Connect to the pane before rearranging it".into());
    }
    let owner = self
      .panes
      .iter()
      .find(|(_, pane)| pane.connected && pane.control.state().leases().layout.owned_by_client)
      .map(|(id, _)| id.clone())
      .ok_or("Resize lease required to rearrange panes")?;
    if view.terminals.len() < 2 {
      return Err("The session has only one pane".into());
    }
    Ok((
      owner,
      PaneTarget {
        session_id: view.session_id.clone(),
        view_id: view.view_id.clone(),
        expected_revision: view.revision,
        terminal_id: self.focused.clone(),
      },
    ))
  }

  pub(super) fn cancel_pane_move(&mut self) {
    if let Some(pending) = self.pane_move.take()
      && let Some(pane) = self.panes.get_mut(&pending.owner)
    {
      pane.cancel_move();
    }
  }

  pub(super) fn cancel_replaced_move(&mut self, id: &str) {
    if self
      .pane_move
      .as_ref()
      .is_some_and(|pending| pending.owner == id)
    {
      self.cancel_pane_move();
      self.notice(
        "Pane move confirmation lost during reconnect; refreshing the current session".into(),
      );
    }
  }

  pub(super) async fn poll_pane_move(&mut self) -> Result<()> {
    let Some(pending) = &self.pane_move else {
      return Ok(());
    };
    if self.view.as_ref().is_none_or(|view| {
      view.session_id != pending.source_session || view.view_id != pending.source_view
    }) {
      self.cancel_pane_move();
      return Ok(());
    }
    let result = self
      .panes
      .get_mut(&pending.owner)
      .and_then(|pane| pane.move_result.take());
    if let Some(result) = result {
      let pending = self.pane_move.take().expect("pending reply");
      if let Some(pane) = self.panes.get_mut(&pending.owner) {
        pane.cancel_move();
      }
      match (pending.action, result) {
        (Move::Swap { focus }, PaneMoveOutcome::Swapped { view }) => {
          self.adopt_view(*view).await?;
          if self.panes.contains_key(&focus) {
            self.focused = focus;
          }
        }
        (Move::Break { terminal, detached }, PaneMoveOutcome::Promoted { view, source_view }) => {
          let view = newest_view(*view, pending.promoted_snapshot);
          let source_view = newest_view(*source_view, pending.source_snapshot);
          self
            .adopt_promotion(
              if detached { source_view } else { view },
              detached,
              terminal,
            )
            .await?;
        }
        (_, PaneMoveOutcome::Rejected { message, .. }) => return Err(message.into()),
        _ => return Err("Unexpected pane move reply".into()),
      }
    } else if self
      .panes
      .get(&pending.owner)
      .is_none_or(|pane| !pane.connected)
    {
      self.cancel_pane_move();
      return Err(
        "Disconnected before pane move confirmation; refreshing the current session".into(),
      );
    } else if pending.deadline <= Instant::now() {
      self.cancel_pane_move();
      return Err("Pane move acknowledgement timed out; refreshing the current session".into());
    }
    Ok(())
  }

  pub(super) async fn swap_pane(&mut self, previous: bool, stay: bool) -> Result<()> {
    let (owner, target) = self.move_target()?;
    let ids = &self.view.as_ref().expect("validated view").terminals;
    let index = ids
      .iter()
      .position(|terminal| terminal.terminal_id == self.focused)
      .ok_or("Focused pane is absent from the layout")?;
    let neighbor = if previous {
      (index + ids.len() - 1) % ids.len()
    } else {
      (index + 1) % ids.len()
    };
    let focus = if stay {
      ids[neighbor].terminal_id.clone()
    } else {
      self.focused.clone()
    };
    self.release_mouse().await?;
    self.resize_sequence = self.resize_sequence.wrapping_add(1);
    self
      .panes
      .get_mut(&owner)
      .expect("validated owner")
      .swap_pane(
        target.clone(),
        previous,
        format!("tui-pane-move-{}", self.resize_sequence),
      )
      .await?;
    self.pane_move = Some(PendingMove {
      owner,
      source_session: target.session_id,
      source_view: target.view_id,
      action: Move::Swap { focus },
      deadline: Instant::now() + Duration::from_secs(5),
      source_snapshot: None,
      promoted_snapshot: None,
    });
    Ok(())
  }

  pub(super) async fn break_pane(&mut self, name: Option<String>, detached: bool) -> Result<()> {
    let (owner, target) = self.move_target()?;
    self.release_mouse().await?;
    self.resize_sequence = self.resize_sequence.wrapping_add(1);
    self
      .panes
      .get_mut(&owner)
      .expect("validated owner")
      .break_pane(
        target.clone(),
        name,
        format!("tui-pane-move-{}", self.resize_sequence),
      )
      .await?;
    self.pane_move = Some(PendingMove {
      owner,
      source_session: target.session_id,
      source_view: target.view_id,
      action: Move::Break {
        terminal: target.terminal_id,
        detached,
      },
      deadline: Instant::now() + Duration::from_secs(5),
      source_snapshot: None,
      promoted_snapshot: None,
    });
    Ok(())
  }

  async fn adopt_promotion(
    &mut self,
    view: ViewInfo,
    detached: bool,
    terminal: String,
  ) -> Result<()> {
    self.release_mouse().await?;
    // A confirmed move changes membership without ending a PTY. Retain the
    // controllers and copy snapshots that belong to the displayed root.
    self.maintenance.cancel();
    self.divider_drag = None;
    self.layout_owner = None;
    self.overlay = Overlay::None;
    self.keys = KeyState::Root;
    self.selected_id.clone_from(&view.session_id);
    if detached {
      // The ACK proves only this live terminal moved. Omitted exited siblings
      // still need their final screen and copy selection until dismissal.
      self.migrated_panes.insert(terminal);
      self.adopt_view(view).await?;
    } else {
      self.migrated_panes.clear();
      self.view = Some(view);
      self.reconcile().await?;
    }
    for pane in self.panes.values_mut() {
      pane.view_update = None;
    }
    // Promotion releases the moved attachment's source resize lease. Request
    // ownership on a retained attachment; never displace another client.
    let has_owner = self
      .panes
      .values()
      .any(|pane| pane.connected && pane.control.state().leases().layout.owned_by_client);
    if !detached || !has_owner {
      for pane in self.panes.values_mut() {
        pane.request_lease(LeaseKind::Layout, false);
      }
      if let Some(pane) = self.panes.get_mut(&self.focused) {
        pane.request_lease(LeaseKind::Layout, true);
        if pane.connected {
          pane.control.acquire_lease(LeaseKind::Layout).await?;
        }
      }
    }
    self.renderer.invalidate();
    Ok(())
  }
}

fn newest_view(reply: ViewInfo, observed: Option<ViewInfo>) -> ViewInfo {
  match observed {
    Some(view)
      if view.session_id == reply.session_id
        && view.view_id == reply.view_id
        && view.revision > reply.revision =>
    {
      view
    }
    _ => reply,
  }
}

#[cfg(all(test, unix))]
mod tests {
  use super::*;
  use crate::{input, test_daemon::TestDaemon};
  use ctmux_proto::{ClientMessage, ServerMessage, SplitAxis, TerminalSize};

  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn expired_move_ignores_late_focus_reply_and_keeps_live_input() -> Result<()> {
    let mut daemon = TestDaemon::start().await?;
    let size = TerminalSize::default();
    let session = daemon
      .create_echo_session("expired-move", "first", size.clone())
      .await?;
    let ServerMessage::ViewSnapshot { view } = daemon
      .request(ClientMessage::GetView {
        session: session.clone(),
      })
      .await?
    else {
      return Err("expected initial view".into());
    };
    let first = view.terminals[0].terminal_id.clone();
    daemon
      .split_echo(&first, SplitAxis::Horizontal, "second", size)
      .await?;
    let mut app = App::new(daemon.socket.clone(), false, input::parse_prefix("Ctrl+b")?);
    app.start(Some(session)).await?;
    app.swap_pane(false, true).await?;
    app.pane_move.as_mut().expect("queued move").deadline = Instant::now();
    assert!(
      app
        .poll_pane_move()
        .await
        .unwrap_err()
        .to_string()
        .contains("timed out")
    );
    assert!(app.pane_move.is_none());
    app.panes[&first]
      .control
      .input(b"after-timeout\n".to_vec())
      .await?;
    tokio::time::timeout(Duration::from_secs(5), async {
      loop {
        app.drain().await;
        if app
          .view
          .as_ref()
          .is_some_and(|view| view.terminals[0].terminal_id != first)
          && app.panes[&first]
            .model
            .vt
            .text()
            .iter()
            .any(|line| line.contains("first:after-timeout"))
        {
          return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
      }
    })
    .await?;
    assert_eq!(app.focused, first);
    assert!(app.panes[&first].move_result.is_none());
    assert!(app.panes[&first].connected);
    app.detach().await;
    drop(app);
    daemon.shutdown().await?;
    Ok(())
  }
}
