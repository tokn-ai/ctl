use super::{App, CopyTarget, MouseCapture};
use crate::Result;
use ctmux_proto::{ViewInfo, ViewLayout};
use std::collections::BTreeSet;

impl App<'_> {
  pub(super) fn observe_migrations(&mut self, updates: Vec<(String, ViewInfo)>) {
    let Some(current) = &self.view else {
      return;
    };
    for (id, view) in updates {
      if view.session_id != current.session_id
        && view.view_id != current.view_id
        && view
          .terminals
          .iter()
          .any(|terminal| terminal.terminal_id == id)
        && current
          .terminals
          .iter()
          .any(|terminal| terminal.terminal_id == id)
        && self.panes.get(&id).is_some_and(|pane| pane.ended.is_none())
      {
        // Keep the proof while a local break awaits its correlated reply. If
        // that reply is lost, the ordinary source refresh can still recover.
        self.migrated_panes.insert(id);
      }
    }
  }

  pub(super) async fn reconcile_migrations(&mut self, incoming: &ViewInfo) -> Result<()> {
    let Some(current) = &self.view else {
      return Ok(());
    };
    if incoming.session_id != current.session_id
      || incoming.view_id != current.view_id
      || incoming.revision < current.revision
    {
      return Ok(());
    }
    let removed: BTreeSet<_> = self
      .migrated_panes
      .iter()
      .filter(|id| {
        !incoming
          .terminals
          .iter()
          .any(|terminal| &terminal.terminal_id == *id)
          && self
            .pane_move
            .as_ref()
            .is_none_or(|pending| !pending.promoting(id))
      })
      .cloned()
      .collect();
    if removed.is_empty() {
      return Ok(());
    }
    let captured = match &self.mouse_capture {
      Some(MouseCapture::Application { terminal_id, .. }) => removed.contains(terminal_id),
      Some(MouseCapture::Selection {
        target: CopyTarget::Pane(id),
        ..
      }) => removed.contains(id),
      _ => false,
    };
    if captured {
      self.release_mouse().await?;
    }
    self.divider_drag = None;
    if let Some(current) = &mut self.view {
      // A different, genuinely exited sibling may still need the retained
      // view for final output. Remove only proven migrations from that view,
      // so reconciliation cannot reopen them while preserving the exit.
      if let Some(layout) = retain_layout(&current.layout, &removed) {
        current.layout = layout;
      }
      current
        .terminals
        .retain(|terminal| !removed.contains(&terminal.terminal_id));
      current
        .panes
        .retain(|pane| !removed.contains(&pane.terminal_id));
      if current
        .zoomed_terminal_id
        .as_ref()
        .is_some_and(|id| removed.contains(id))
      {
        current.zoomed_terminal_id = None;
      }
    }
    for id in removed {
      self.copies.remove(&id);
      if let Some(mut pane) = self.panes.remove(&id) {
        pane.close().await;
      }
      self.migrated_panes.remove(&id);
    }
    self.renderer.invalidate();
    Ok(())
  }
}

fn retain_layout(layout: &ViewLayout, removed: &BTreeSet<String>) -> Option<ViewLayout> {
  match layout {
    ViewLayout::Terminal { terminal_id } => {
      (!removed.contains(terminal_id)).then(|| layout.clone())
    }
    ViewLayout::Split {
      axis,
      children,
      weights,
    } => {
      let mut retained = Vec::new();
      let mut retained_weights = Vec::new();
      for (index, child) in children.iter().enumerate() {
        if let Some(child) = retain_layout(child, removed) {
          retained.push(child);
          if let Some(weight) = weights.get(index) {
            retained_weights.push(*weight);
          }
        }
      }
      match retained.len() {
        0 => None,
        1 => retained.pop(),
        _ => Some(ViewLayout::Split {
          axis: *axis,
          children: retained,
          weights: retained_weights,
        }),
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use ctmux_proto::SplitAxis;

  #[test]
  fn retained_layout_collapses_only_removed_children_and_preserves_slot_weights() {
    let leaf = |id: &str| ViewLayout::Terminal {
      terminal_id: id.into(),
    };
    let layout = ViewLayout::Split {
      axis: SplitAxis::Horizontal,
      children: vec![
        leaf("first"),
        ViewLayout::Split {
          axis: SplitAxis::Vertical,
          children: vec![leaf("moving"), leaf("exited")],
          weights: vec![3, 7],
        },
        leaf("last"),
      ],
      weights: vec![2, 5, 9],
    };
    assert_eq!(
      retain_layout(&layout, &BTreeSet::from(["moving".into(), "last".into()])),
      Some(ViewLayout::Split {
        axis: SplitAxis::Horizontal,
        children: vec![leaf("first"), leaf("exited")],
        weights: vec![2, 5],
      })
    );
  }
}
