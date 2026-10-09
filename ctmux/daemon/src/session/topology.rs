use super::{
  PaneMoveError, SessionControlError, SessionLifecycle, SessionManager, SessionManagerError,
  SessionRegistry, Terminal, TerminalOwner, lock, unix_time_ms,
};
use ctmux_proto::{LeaseKind, PaneTarget, SplitAxis, TerminalInfo, ViewInfo, ViewLayout};
use std::collections::HashSet;
use std::sync::Arc;
use uuid::Uuid;

pub(super) struct Session {
  pub closing: bool,
  pub id: String,
  pub name: String,
  pub view: View,
}

pub(super) struct View {
  pub id: String,
  pub revision: u64,
  pub layout: ViewLayout,
  pub canvas_size: ctmux_proto::TerminalSize,
  pub zoomed_terminal_id: Option<String>,
  pub leases: ctmux_core::AttachmentLeaseRegistry,
}

pub(super) trait LayoutExt {
  fn first_terminal(&self) -> String;
  fn terminal_ids(&self) -> Vec<String>;
  fn remove_terminal(self, id: &str) -> Option<Self>
  where
    Self: Sized;
  fn split_terminal(&mut self, id: &str, new_id: &str, axis: SplitAxis);
}

impl LayoutExt for ViewLayout {
  fn first_terminal(&self) -> String {
    match self {
      Self::Terminal { terminal_id } => terminal_id.clone(),
      Self::Split { children, .. } => children[0].first_terminal(),
    }
  }

  fn terminal_ids(&self) -> Vec<String> {
    match self {
      Self::Terminal { terminal_id } => vec![terminal_id.clone()],
      Self::Split { children, .. } => children.iter().flat_map(Self::terminal_ids).collect(),
    }
  }

  fn remove_terminal(self, id: &str) -> Option<Self> {
    match self {
      Self::Terminal { ref terminal_id } => (terminal_id != id).then_some(self),
      Self::Split {
        axis,
        children,
        weights,
      } => {
        let mut remaining = Vec::new();
        let mut remaining_weights = Vec::new();
        for (index, child) in children.into_iter().enumerate() {
          if let Some(child) = child.remove_terminal(id) {
            remaining.push(child);
            if !weights.is_empty() {
              remaining_weights.push(weights[index]);
            }
          }
        }
        let mut children = remaining;
        match children.len() {
          0 => None,
          1 => children.pop(),
          _ => Some(Self::Split {
            axis,
            children,
            weights: remaining_weights,
          }),
        }
      }
    }
  }

  fn split_terminal(&mut self, id: &str, new_id: &str, axis: SplitAxis) {
    match self {
      Self::Terminal { terminal_id } if terminal_id == id => {
        *self = Self::Split {
          axis,
          weights: Vec::new(),
          children: vec![
            self.clone(),
            Self::Terminal {
              terminal_id: new_id.into(),
            },
          ],
        };
      }
      Self::Split { children, .. } => {
        for child in children {
          child.split_terminal(id, new_id, axis);
        }
      }
      Self::Terminal { .. } => {}
    }
  }
}

/// Validate the observed source and authority under the same lock as mutation.
fn attached_pane_target<'a>(
  registry: &'a SessionRegistry,
  attached: &Terminal,
  attachment_id: &str,
  target: &PaneTarget,
) -> Result<&'a Session, SessionControlError> {
  let owner = lock(&attached.owner).session_id.clone();
  let root = registry
    .sessions
    .get(&owner)
    .ok_or_else(|| SessionControlError::InvalidView("view has closed".into()))?;
  if !root
    .view
    .leases
    .status(attachment_id, LeaseKind::Layout)
    .owned_by_client
  {
    return Err(SessionControlError::LayoutLeaseRequired);
  }
  if root.closing {
    return Err(SessionControlError::InvalidView(
      "session is terminating".into(),
    ));
  }
  if target.session_id != root.id
    || target.view_id != root.view.id
    || target.expected_revision != root.view.revision
  {
    return Err(SessionControlError::InvalidView(
      "view changed; reload before moving panes".into(),
    ));
  }
  if !root
    .view
    .layout
    .terminal_ids()
    .contains(&target.terminal_id)
  {
    return Err(SessionControlError::InvalidView(
      "pane must belong to the attached view".into(),
    ));
  }
  running_movable_terminal(registry, &target.terminal_id)?;
  Ok(root)
}

fn running_movable_terminal(
  registry: &SessionRegistry,
  id: &str,
) -> Result<(), SessionControlError> {
  let terminal = registry
    .terminals
    .get(id)
    .ok_or_else(|| SessionControlError::InvalidView("pane no longer exists".into()))?;
  if terminal.managed {
    return Err(SessionControlError::InvalidView(
      "managed task terminals cannot be moved".into(),
    ));
  }
  if !matches!(*lock(&terminal.lifecycle), SessionLifecycle::Running) {
    return Err(SessionControlError::InvalidView("pane has ended".into()));
  }
  Ok(())
}

fn swap_terminal_ids(layout: &mut ViewLayout, left: &str, right: &str) {
  match layout {
    ViewLayout::Terminal { terminal_id } if terminal_id == left => {
      right.clone_into(terminal_id);
    }
    ViewLayout::Terminal { terminal_id } if terminal_id == right => {
      left.clone_into(terminal_id);
    }
    ViewLayout::Split { children, .. } => {
      for child in children {
        swap_terminal_ids(child, left, right);
      }
    }
    ViewLayout::Terminal { .. } => {}
  }
}

impl Terminal {
  pub(super) fn swap_pane_inner(
    &self,
    attachment_id: &str,
    target: &PaneTarget,
    previous: bool,
  ) -> Result<ViewInfo, SessionControlError> {
    self.resize_layout(attachment_id, |view, registry| {
      attached_pane_target(registry, self, attachment_id, target)?;
      let ids = view.layout.terminal_ids();
      if ids.len() < 2 {
        return Err(SessionControlError::InvalidView(
          "session has only one pane".into(),
        ));
      }
      let index = ids
        .iter()
        .position(|id| id == &target.terminal_id)
        .expect("validated pane");
      let adjacent = if previous {
        (index + ids.len() - 1) % ids.len()
      } else {
        (index + 1) % ids.len()
      };
      running_movable_terminal(registry, &ids[adjacent])?;
      let mut layout = view.layout.clone();
      swap_terminal_ids(&mut layout, &target.terminal_id, &ids[adjacent]);
      Ok((layout, true))
    })
  }

  pub(super) fn break_pane_inner(
    &self,
    attachment_id: &str,
    target: &PaneTarget,
    name: Option<String>,
  ) -> Result<(ViewInfo, ViewInfo), PaneMoveError> {
    let inner = self
      .manager
      .upgrade()
      .ok_or_else(|| SessionControlError::InvalidView("view has closed".into()))?;
    let manager = SessionManager { inner };
    let mut reservation = manager.reserve_name(name)?;
    let mut registry = lock(&manager.inner.registry);
    let root = attached_pane_target(&registry, self, attachment_id, target)?;
    let ids = root.view.layout.terminal_ids();
    if ids.len() < 2 {
      return Err(SessionControlError::InvalidView("session has only one pane".into()).into());
    }
    let source_id = root.id.clone();
    let old_layout = root.view.layout.clone();
    let old_zoom = root.view.zoomed_terminal_id.clone();
    let old_revision = root.view.revision;
    let remaining = old_layout
      .clone()
      .remove_terminal(&target.terminal_id)
      .expect("remaining pane");
    remaining
      .pane_geometry(&root.view.canvas_size)
      .map_err(SessionControlError::InvalidView)?;
    let geometry = registry.capture_view_geometry(&source_id);
    let view = &mut registry
      .sessions
      .get_mut(&source_id)
      .expect("validated source")
      .view;
    view.layout = remaining;
    view.zoomed_terminal_id = None;
    view.revision += 1;
    if let Err(error) = registry.reflow_view(&source_id) {
      let view = &mut registry
        .sessions
        .get_mut(&source_id)
        .expect("validated source")
        .view;
      view.layout = old_layout;
      view.zoomed_terminal_id = old_zoom;
      view.revision = old_revision;
      registry.restore_view_geometry(&source_id, &geometry);
      return Err(error.into());
    }
    let terminal = Arc::clone(&registry.terminals[&target.terminal_id]);
    for record in lock(&terminal.attachments).values() {
      registry
        .sessions
        .get_mut(&source_id)
        .expect("validated source")
        .view
        .leases
        .release_attachment(&record.attachment_id);
    }
    let owner = TerminalOwner {
      created_at_ms: unix_time_ms(),
      session_id: Uuid::new_v4().to_string(),
      view_id: Uuid::new_v4().to_string(),
      name: reservation.name.clone(),
    };
    *lock(&terminal.owner) = owner.clone();
    registry.sessions.insert(
      owner.session_id.clone(),
      Session {
        closing: false,
        id: owner.session_id.clone(),
        name: owner.name,
        view: View {
          id: owner.view_id,
          revision: 0,
          canvas_size: terminal.info().terminal_size,
          zoomed_terminal_id: None,
          leases: ctmux_core::AttachmentLeaseRegistry::default(),
          layout: ViewLayout::Terminal {
            terminal_id: target.terminal_id.clone(),
          },
        },
      },
    );
    registry.pending_names.remove(&reservation.name);
    reservation.active = false;
    registry.publish_view(&source_id);
    registry.publish_view(&owner.session_id);
    Ok((
      registry.view_info(&owner.session_id)?,
      registry.view_info(&source_id)?,
    ))
  }
}

impl SessionRegistry {
  pub(super) fn capture_view_geometry(
    &self,
    id: &str,
  ) -> Vec<(Arc<Terminal>, ctmux_proto::TerminalSize)> {
    self.sessions[id]
      .view
      .layout
      .terminal_ids()
      .iter()
      .map(|id| {
        let terminal = Arc::clone(&self.terminals[id]);
        let size = terminal.info().terminal_size;
        (terminal, size)
      })
      .collect()
  }

  pub(super) fn restore_view_geometry(
    &self,
    id: &str,
    geometry: &[(Arc<Terminal>, ctmux_proto::TerminalSize)],
  ) {
    // Continue after a failed PTY: later hidden panes may already have adopted
    // tentative unzoom geometry and also need their original dimensions back.
    for (terminal, size) in geometry {
      let _restore = terminal.resize_pty(size.clone());
    }
    self.publish_view(id);
  }

  pub(super) fn resize_view(
    &mut self,
    id: &str,
    size: ctmux_proto::TerminalSize,
  ) -> Result<(), super::SessionControlError> {
    let root = &self.sessions[id];
    let panes = if let Some(terminal_id) = &root.view.zoomed_terminal_id {
      vec![ctmux_proto::PaneGeometry {
        terminal_id: terminal_id.clone(),
        left: 0,
        top: 0,
        columns: size.columns,
        rows: size.rows,
      }]
    } else {
      root
        .view
        .layout
        .pane_geometry(&size)
        .map_err(super::SessionControlError::Pty)?
    };
    for pane in panes {
      if let Some(terminal) = self.terminals.get(&pane.terminal_id) {
        terminal.resize_pty(pane_size(&size, &pane))?;
      }
    }
    let root = self.sessions.get_mut(id).expect("view exists");
    if root.view.canvas_size != size {
      root.view.canvas_size = size;
      root.view.revision += 1;
    }
    self.publish_view(id);
    Ok(())
  }

  pub(super) fn publish_view(&self, id: &str) {
    let Ok(view) = self.view_info(id) else {
      return;
    };
    for terminal in &view.terminals {
      if let Some(terminal) = self.terminals.get(&terminal.terminal_id) {
        terminal.view_updates.send_if_modified(|current| {
          if current.as_ref().is_some_and(|previous| {
            previous.view_id == view.view_id && previous.revision == view.revision
          }) {
            return false;
          }
          *current = Some(view.clone());
          true
        });
      }
    }
    // A terminal can move between views without replacing its attachment.
    // Refresh ownership against its current view after every topology update.
    self.publish_layout_leases(id);
  }

  pub(super) fn publish_layout_leases(&self, id: &str) {
    let Some(root) = self.sessions.get(id) else {
      return;
    };
    for terminal_id in root.view.layout.terminal_ids() {
      if let Some(terminal) = self.terminals.get(&terminal_id) {
        // This watch is an invalidation signal, independent of canonical PTY
        // events. Drivers query current ownership and suppress unchanged state.
        terminal.layout_lease_updates.send_replace(());
      }
    }
  }

  pub(super) fn reflow_view(&mut self, id: &str) -> Result<(), super::SessionControlError> {
    let Some(root) = self.sessions.get(id) else {
      return Ok(());
    };
    self.resize_view(id, root.view.canvas_size.clone())
  }

  pub(super) fn plan_split(
    &self,
    target_id: &str,
    new_id: &str,
    axis: SplitAxis,
  ) -> Result<(TerminalOwner, ViewLayout), SessionManagerError> {
    let target = self
      .terminals
      .get(target_id)
      .ok_or_else(|| SessionManagerError::NotFound {
        selector: target_id.into(),
      })?;
    if target.managed {
      return Err(SessionManagerError::InvalidView(
        "managed task terminals cannot be split".into(),
      ));
    }
    let owner = lock(&target.owner).clone();
    let root = &self.sessions[&owner.session_id];
    if root.closing {
      return Err(SessionManagerError::InvalidView(
        "session is terminating".into(),
      ));
    }
    let mut layout = root.view.layout.clone();
    layout.split_terminal(target_id, new_id, axis);
    validate_layout(&layout, 0)?;
    layout
      .pane_geometry(&root.view.canvas_size)
      .map_err(SessionManagerError::InvalidView)?;
    Ok((owner, layout))
  }

  pub(super) fn root(&self, selector: &str) -> Result<&Session, SessionManagerError> {
    self
      .sessions
      .get(selector)
      .or_else(|| {
        self
          .sessions
          .values()
          .find(|session| session.name == selector)
      })
      .ok_or_else(|| SessionManagerError::NotFound {
        selector: selector.into(),
      })
  }

  pub(super) fn view_info(&self, selector: &str) -> Result<ViewInfo, SessionManagerError> {
    let session = self.root(selector)?;
    let terminals = session
      .view
      .layout
      .terminal_ids()
      .iter()
      .filter_map(|id| self.terminals.get(id))
      .map(|terminal| {
        let info = terminal.info();
        TerminalInfo {
          terminal_id: terminal.id.clone(),
          name: terminal.name.clone(),
          created_at_ms: terminal.created_at_ms,
          next_sequence: info.next_sequence,
          terminal_size: info.terminal_size,
        }
      })
      .collect();
    Ok(ViewInfo {
      session_name: session.name.clone(),
      session_id: session.id.clone(),
      view_id: session.view.id.clone(),
      revision: session.view.revision,
      canvas_size: session.view.canvas_size.clone(),
      zoomed_terminal_id: session.view.zoomed_terminal_id.clone(),
      panes: session
        .view
        .layout
        .pane_geometry(&session.view.canvas_size)
        .map_err(SessionManagerError::InvalidView)?,
      layout: session.view.layout.clone(),
      terminals,
    })
  }

  fn detach_terminal(&mut self, id: &str) {
    let Some(terminal) = self.terminals.get(id) else {
      return;
    };
    let owner_id = lock(&terminal.owner).session_id.clone();
    if let Some(session) = self.sessions.get_mut(&owner_id) {
      if let Some(layout) = session.view.layout.clone().remove_terminal(id) {
        for record in lock(&terminal.attachments).values() {
          session
            .view
            .leases
            .release_attachment(&record.attachment_id);
        }
        session.view.layout = layout;
        session.view.zoomed_terminal_id = None;
        session.view.revision += 1;
      } else {
        self.sessions.remove(&owner_id);
      }
    }
    // Lease changes are committed independently of fallible PTY reflow.
    self.publish_layout_leases(&owner_id);
    let _ = self.reflow_view(&owner_id);
  }

  pub(super) fn remove_terminal(&mut self, id: &str) {
    self.detach_terminal(id);
    self.terminals.remove(id);
  }
}

impl SessionManager {
  pub fn view(&self, selector: &str) -> Result<ViewInfo, SessionManagerError> {
    lock(&self.inner.registry).view_info(selector)
  }

  pub(super) fn update_view_inner(
    &self,
    selector: &str,
    expected_revision: u64,
    mut layout: ViewLayout,
  ) -> Result<ViewInfo, SessionManagerError> {
    validate_layout(&layout, 0)?;
    let mut registry = lock(&self.inner.registry);
    let root = registry.root(selector)?;
    if root.view.revision != expected_revision {
      return Err(SessionManagerError::InvalidView(
        "view changed; reload before editing".into(),
      ));
    }
    preserve_layout_weights(&mut layout, &root.view.layout)?;
    let expected: HashSet<_> = root.view.layout.terminal_ids().into_iter().collect();
    let ids = layout.terminal_ids();
    if ids.len() != expected.len()
      || ids.iter().collect::<HashSet<_>>().len() != ids.len()
      || ids.iter().any(|id| !expected.contains(id))
    {
      return Err(SessionManagerError::InvalidView(
        "layout must contain each owned terminal exactly once".into(),
      ));
    }
    layout
      .pane_geometry(&root.view.canvas_size)
      .map_err(SessionManagerError::InvalidView)?;
    let id = root.id.clone();
    let root = registry.sessions.get_mut(&id).expect("validated root");
    root.view.layout = layout;
    root.view.zoomed_terminal_id = None;
    root.view.revision += 1;
    registry
      .reflow_view(&id)
      .map_err(|error| SessionManagerError::Pty(error.to_string()))?;
    registry.view_info(&id)
  }

  pub(super) fn promote_terminal_inner(
    &self,
    terminal_id: &str,
    name: Option<String>,
  ) -> Result<ViewInfo, SessionManagerError> {
    let mut reservation = self.reserve_name(name)?;
    let mut registry = lock(&self.inner.registry);
    let terminal = registry
      .terminals
      .get(terminal_id)
      .cloned()
      .ok_or_else(|| SessionManagerError::NotFound {
        selector: terminal_id.into(),
      })?;
    if terminal.managed {
      return Err(SessionManagerError::InvalidView(
        "managed task terminals cannot be moved".into(),
      ));
    }
    let old_owner = lock(&terminal.owner).clone();
    if registry.sessions[&old_owner.session_id].closing {
      return Err(SessionManagerError::InvalidView(
        "session is terminating".into(),
      ));
    }
    if registry.sessions[&old_owner.session_id]
      .view
      .layout
      .terminal_ids()
      .len()
      == 1
    {
      return Err(SessionManagerError::InvalidView(
        "terminal is already the only terminal in its session".into(),
      ));
    }
    registry.detach_terminal(terminal_id);
    let owner = TerminalOwner {
      created_at_ms: unix_time_ms(),
      session_id: Uuid::new_v4().to_string(),
      view_id: Uuid::new_v4().to_string(),
      name: reservation.name.clone(),
    };
    *lock(&terminal.owner) = owner.clone();
    registry.sessions.insert(
      owner.session_id.clone(),
      Session {
        closing: false,
        id: owner.session_id.clone(),
        name: owner.name,
        view: View {
          id: owner.view_id,
          revision: 0,
          canvas_size: terminal.info().terminal_size,
          zoomed_terminal_id: None,
          leases: ctmux_core::AttachmentLeaseRegistry::default(),
          layout: ViewLayout::Terminal {
            terminal_id: terminal_id.into(),
          },
        },
      },
    );
    registry.pending_names.remove(&reservation.name);
    reservation.active = false;
    registry.publish_layout_leases(&owner.session_id);
    registry.publish_view(&owner.session_id);
    registry.view_info(&owner.session_id)
  }

  pub(super) fn merge_sessions_inner(
    &self,
    source: &str,
    destination: &str,
  ) -> Result<ViewInfo, SessionManagerError> {
    let mut registry = lock(&self.inner.registry);
    let source_id = registry.root(source)?.id.clone();
    let destination_id = registry.root(destination)?.id.clone();
    if source_id == destination_id {
      return Err(SessionManagerError::InvalidView(
        "cannot merge a session into itself".into(),
      ));
    }
    for id in [source_id.as_str(), destination_id.as_str()] {
      if registry.sessions[id].closing {
        return Err(SessionManagerError::InvalidView(
          "session is terminating".into(),
        ));
      }
      for terminal_id in registry.sessions[id].view.layout.terminal_ids() {
        if registry.terminals[&terminal_id].managed {
          return Err(SessionManagerError::InvalidView(
            "managed task sessions cannot be merged".into(),
          ));
        }
      }
    }
    let merged_layout = ViewLayout::Split {
      axis: SplitAxis::Horizontal,
      weights: Vec::new(),
      children: vec![
        registry.sessions[&destination_id].view.layout.clone(),
        registry.sessions[&source_id].view.layout.clone(),
      ],
    };
    validate_layout(&merged_layout, 0)?;
    merged_layout
      .pane_geometry(&registry.sessions[&destination_id].view.canvas_size)
      .map_err(SessionManagerError::InvalidView)?;
    let source = registry
      .sessions
      .remove(&source_id)
      .expect("validated source");
    let created_at_ms = lock(
      &registry.terminals[&registry.sessions[&destination_id]
        .view
        .layout
        .first_terminal()]
        .owner,
    )
    .created_at_ms;
    let destination = registry
      .sessions
      .get_mut(&destination_id)
      .expect("validated destination");
    let owner = TerminalOwner {
      created_at_ms,
      session_id: destination.id.clone(),
      view_id: destination.view.id.clone(),
      name: destination.name.clone(),
    };
    let moved_ids = source.view.layout.terminal_ids();
    destination.view.layout = merged_layout;
    destination.view.zoomed_terminal_id = None;
    destination.view.revision += 1;
    for id in moved_ids {
      *lock(&registry.terminals[&id].owner) = owner.clone();
    }
    // Source attachments now consult the destination registry even if its
    // geometry update fails before a view snapshot can be published.
    registry.publish_layout_leases(&destination_id);
    registry
      .reflow_view(&destination_id)
      .map_err(|error| SessionManagerError::Pty(error.to_string()))?;
    registry.view_info(&destination_id)
  }

  pub(super) fn begin_termination_inner(
    &self,
    selector: &str,
  ) -> Result<Vec<Arc<Terminal>>, SessionManagerError> {
    let mut registry = lock(&self.inner.registry);
    let root_id = registry.root(selector)?.id.clone();
    let root = registry.sessions.get_mut(&root_id).expect("validated root");
    root.closing = true;
    let ids = root.view.layout.terminal_ids();
    Ok(
      ids
        .iter()
        .filter_map(|id| registry.terminals.get(id).cloned())
        .collect(),
    )
  }
}

pub(super) fn validate_layout(
  layout: &ViewLayout,
  depth: usize,
) -> Result<(), SessionManagerError> {
  if depth == 0 && layout.terminal_ids().len() > 64 {
    return Err(SessionManagerError::InvalidView(
      "a view supports at most 64 terminals".into(),
    ));
  }
  if depth > 16 {
    return Err(SessionManagerError::InvalidView(
      "layout nesting exceeds 16".into(),
    ));
  }
  match layout {
    ViewLayout::Terminal { .. } => Ok(()),
    ViewLayout::Split {
      children, weights, ..
    } => {
      if children.len() < 2 || children.len() > 64 {
        return Err(SessionManagerError::InvalidView(
          "layout groups require 2 to 64 children".into(),
        ));
      }
      if !weights.is_empty() && (weights.len() != children.len() || weights.contains(&0)) {
        return Err(SessionManagerError::InvalidView(
          "split weights must have one positive value per child".into(),
        ));
      }
      for child in children {
        validate_layout(child, depth + 1)?;
      }
      Ok(())
    }
  }
}

/// Arrangement edits cannot bypass the attachment lease used for pane sizing.
/// Missing weights preserve slot proportions, including when older clients swap leaves.
fn preserve_layout_weights(
  next: &mut ViewLayout,
  previous: &ViewLayout,
) -> Result<(), SessionManagerError> {
  let ambiguous = || {
    SessionManagerError::InvalidView(
    "weighted layout cannot be restructured by update_view; resize panes through an owned attachment".into(),
  )
  };
  match (next, previous) {
    (
      ViewLayout::Split {
        axis,
        children,
        weights,
      },
      ViewLayout::Split {
        axis: old_axis,
        children: old_children,
        weights: old_weights,
      },
    ) if axis == old_axis && children.len() == old_children.len() => {
      if !weights.is_empty() && weights != old_weights {
        return Err(ambiguous());
      }
      weights.clone_from(old_weights);
      for (child, previous) in children.iter_mut().zip(old_children) {
        preserve_layout_weights(child, previous)?;
      }
      Ok(())
    }
    (next, previous) if next.has_weights() || previous.has_weights() => Err(ambiguous()),
    _ => Ok(()),
  }
}

pub(super) fn pane_size(
  canvas: &ctmux_proto::TerminalSize,
  pane: &ctmux_proto::PaneGeometry,
) -> ctmux_proto::TerminalSize {
  ctmux_proto::TerminalSize {
    columns: pane.columns,
    rows: pane.rows,
    pixel_width: u16::try_from(
      (u32::from(canvas.pixel_width) * u32::from(pane.columns)) / u32::from(canvas.columns),
    )
    .expect("pane is bounded by canvas"),
    pixel_height: u16::try_from(
      (u32::from(canvas.pixel_height) * u32::from(pane.rows)) / u32::from(canvas.rows),
    )
    .expect("pane is bounded by canvas"),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn leaf(id: &str) -> ViewLayout {
    ViewLayout::Terminal {
      terminal_id: id.into(),
    }
  }
  fn split(children: Vec<ViewLayout>, weights: &[u32]) -> ViewLayout {
    ViewLayout::Split {
      axis: SplitAxis::Horizontal,
      children,
      weights: weights.to_vec(),
    }
  }

  #[test]
  fn arrangement_edits_preserve_existing_slot_weights_and_reject_lease_bypasses() {
    let original = split(
      vec![leaf("a"), split(vec![leaf("b"), leaf("c")], &[1, 3])],
      &[2, 5],
    );
    let mut next = split(vec![leaf("c"), split(vec![leaf("a"), leaf("b")], &[])], &[]);
    preserve_layout_weights(&mut next, &original).unwrap();
    assert_eq!(
      next,
      split(
        vec![leaf("c"), split(vec![leaf("a"), leaf("b")], &[1, 3])],
        &[2, 5]
      )
    );
    let mut forged = split(
      vec![leaf("a"), split(vec![leaf("b"), leaf("c")], &[1, 3])],
      &[1, 1],
    );
    assert!(preserve_layout_weights(&mut forged, &original).is_err());
    let mut flattened = split(vec![leaf("a"), leaf("b"), leaf("c")], &[]);
    assert!(preserve_layout_weights(&mut flattened, &original).is_err());
    let mut new_weighted = split(vec![leaf("a"), leaf("b")], &[1, 3]);
    assert!(
      preserve_layout_weights(&mut new_weighted, &split(vec![leaf("a"), leaf("b")], &[])).is_err()
    );
  }

  #[test]
  fn topology_changes_keep_surviving_split_weights_aligned() {
    let original = split(
      vec![
        leaf("a"),
        split(vec![leaf("b"), leaf("c")], &[1, 3]),
        leaf("d"),
      ],
      &[2, 5, 1],
    );
    assert_eq!(
      original.clone().remove_terminal("b").unwrap(),
      split(vec![leaf("a"), leaf("c"), leaf("d")], &[2, 5, 1])
    );
    assert_eq!(
      original.clone().remove_terminal("a").unwrap(),
      split(
        vec![split(vec![leaf("b"), leaf("c")], &[1, 3]), leaf("d")],
        &[5, 1]
      )
    );
    let mut divided = original;
    divided.split_terminal("a", "new", SplitAxis::Vertical);
    let ViewLayout::Split {
      weights, children, ..
    } = divided
    else {
      panic!("split expected");
    };
    assert_eq!(weights, [2, 5, 1]);
    assert!(matches!(&children[0], ViewLayout::Split { weights, .. } if weights.is_empty()));
  }
}
