use super::{
  SessionManager, SessionManagerError, SessionRegistry, Terminal, TerminalOwner, lock, unix_time_ms,
};
use rmux_proto::{SplitAxis, TerminalInfo, ViewInfo, ViewLayout};
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
      Self::Split { children, .. } | Self::Tabs { children } => children[0].first_terminal(),
    }
  }

  fn terminal_ids(&self) -> Vec<String> {
    match self {
      Self::Terminal { terminal_id } => vec![terminal_id.clone()],
      Self::Split { children, .. } | Self::Tabs { children } => {
        children.iter().flat_map(Self::terminal_ids).collect()
      }
    }
  }

  fn remove_terminal(self, id: &str) -> Option<Self> {
    match self {
      Self::Terminal { ref terminal_id } => (terminal_id != id).then_some(self),
      Self::Split { axis, children } => {
        let mut children: Vec<_> = children
          .into_iter()
          .filter_map(|child| child.remove_terminal(id))
          .collect();
        match children.len() {
          0 => None,
          1 => children.pop(),
          _ => Some(Self::Split { axis, children }),
        }
      }
      Self::Tabs { children } => {
        let mut children: Vec<_> = children
          .into_iter()
          .filter_map(|child| child.remove_terminal(id))
          .collect();
        match children.len() {
          0 => None,
          1 => children.pop(),
          _ => Some(Self::Tabs { children }),
        }
      }
    }
  }

  fn split_terminal(&mut self, id: &str, new_id: &str, axis: SplitAxis) {
    match self {
      Self::Terminal { terminal_id } if terminal_id == id => {
        *self = Self::Split {
          axis,
          children: vec![
            self.clone(),
            Self::Terminal {
              terminal_id: new_id.into(),
            },
          ],
        };
      }
      Self::Split { children, .. } | Self::Tabs { children } => {
        for child in children {
          child.split_terminal(id, new_id, axis);
        }
      }
      Self::Terminal { .. } => {}
    }
  }
}

impl SessionRegistry {
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

  fn view_info(&self, selector: &str) -> Result<ViewInfo, SessionManagerError> {
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
        session.view.layout = layout;
        session.view.revision += 1;
      } else {
        self.sessions.remove(&owner_id);
      }
    }
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

  pub fn update_view(
    &self,
    selector: &str,
    expected_revision: u64,
    layout: ViewLayout,
  ) -> Result<ViewInfo, SessionManagerError> {
    validate_layout(&layout, 0)?;
    let mut registry = lock(&self.inner.registry);
    let root = registry.root(selector)?;
    if root.view.revision != expected_revision {
      return Err(SessionManagerError::InvalidView(
        "view changed; reload before editing".into(),
      ));
    }
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
    let id = root.id.clone();
    let root = registry.sessions.get_mut(&id).expect("validated root");
    root.view.layout = layout;
    root.view.revision += 1;
    registry.view_info(&id)
  }

  pub fn promote_terminal(
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
          layout: ViewLayout::Terminal {
            terminal_id: terminal_id.into(),
          },
        },
      },
    );
    registry.pending_names.remove(&reservation.name);
    reservation.active = false;
    registry.view_info(&owner.session_id)
  }

  pub fn merge_sessions(
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
    let merged_layout = ViewLayout::Tabs {
      children: vec![
        registry.sessions[&destination_id].view.layout.clone(),
        registry.sessions[&source_id].view.layout.clone(),
      ],
    };
    validate_layout(&merged_layout, 0)?;
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
    destination.view.revision += 1;
    for id in moved_ids {
      *lock(&registry.terminals[&id].owner) = owner.clone();
    }
    registry.view_info(&destination_id)
  }

  pub fn begin_termination(
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
    ViewLayout::Split { children, .. } | ViewLayout::Tabs { children } => {
      if children.len() < 2 || children.len() > 64 {
        return Err(SessionManagerError::InvalidView(
          "layout groups require 2 to 64 children".into(),
        ));
      }
      for child in children {
        validate_layout(child, depth + 1)?;
      }
      Ok(())
    }
  }
}
