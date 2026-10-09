use super::{App, Overlay};
use crate::{Result, keys::KeyState};
use crossterm::event::{KeyCode, KeyEvent};
use ctmux_proto::ViewInfo;
use std::{collections::BTreeMap, time::Duration};
use tokio::time::Instant;

#[derive(Default)]
pub(super) struct Navigation {
  pub last_session: Option<String>,
  focused: BTreeMap<String, String>,
  last_pane: BTreeMap<String, String>,
}

pub(super) struct PaneLabels {
  session_id: String,
  view_id: String,
  visible: Vec<String>,
  pub labels: Vec<(String, usize)>,
  deadline: Instant,
}

impl PaneLabels {
  fn new(view: &ViewInfo) -> Self {
    Self {
      session_id: view.session_id.clone(),
      view_id: view.view_id.clone(),
      visible: visible_ids(view),
      labels: pane_labels(view),
      deadline: Instant::now() + Duration::from_secs(1),
    }
  }

  fn valid(&self, view: &ViewInfo) -> bool {
    self.deadline > Instant::now()
      && self.session_id == view.session_id
      && self.view_id == view.view_id
      && self.visible == visible_ids(view)
      && self.labels == pane_labels(view)
  }
}

fn visible_ids(view: &ViewInfo) -> Vec<String> {
  view
    .visible_panes()
    .into_iter()
    .map(|pane| pane.terminal_id)
    .collect()
}

fn pane_labels(view: &ViewInfo) -> Vec<(String, usize)> {
  view
    .panes
    .iter()
    .enumerate()
    .map(|(index, pane)| (pane.terminal_id.clone(), index + 1))
    .collect()
}

impl App<'_> {
  pub(super) fn retain_navigation(&mut self) {
    let retained = |id: &String| {
      self
        .sessions
        .iter()
        .any(|session| &session.session_id == id)
    };
    self.navigation.focused.retain(|id, _| retained(id));
    self.navigation.last_pane.retain(|id, _| retained(id));
  }

  /// Only deliberate navigation updates the last pane. Reconciliation and
  /// remote zoom may change focus without turning it into a new back target.
  pub(super) fn focus_pane(&mut self, id: String) {
    let Some(view) = &self.view else {
      return;
    };
    if !view.panes.iter().any(|pane| pane.terminal_id == id) || self.focused == id {
      return;
    }
    if view
      .panes
      .iter()
      .any(|pane| pane.terminal_id == self.focused)
    {
      self
        .navigation
        .last_pane
        .insert(view.session_id.clone(), self.focused.clone());
    }
    self
      .navigation
      .focused
      .insert(view.session_id.clone(), id.clone());
    self.focused = id;
  }

  pub(super) fn remember_focus(&mut self) {
    if let Some(view) = &self.view {
      self
        .navigation
        .focused
        .insert(view.session_id.clone(), self.focused.clone());
      if self
        .navigation
        .last_pane
        .get(&view.session_id)
        .is_some_and(|id| !view.panes.iter().any(|pane| &pane.terminal_id == id))
      {
        self.navigation.last_pane.remove(&view.session_id);
      }
    }
    self.expire_pane_labels();
  }

  pub(super) fn remember_session_switch(&mut self, next: &str) {
    self.remember_focus();
    if let Some(current) = &self.view
      && current.session_id != next
    {
      self.navigation.last_session = Some(current.session_id.clone());
    }
  }

  pub(super) fn remembered_focus(&self, session: &str) -> Option<&String> {
    self.navigation.focused.get(session)
  }

  pub(super) async fn last_pane(&mut self) -> Result<()> {
    let target = self
      .view
      .as_ref()
      .and_then(|view| {
        self
          .navigation
          .last_pane
          .get(&view.session_id)
          .filter(|id| view.panes.iter().any(|pane| &pane.terminal_id == *id))
          .cloned()
      })
      .ok_or("No last pane available")?;
    if target != self.focused {
      self.unzoom().await?;
    }
    self.focus_pane(target);
    Ok(())
  }

  pub(super) async fn last_session(&mut self) -> Result<()> {
    let target = self
      .navigation
      .last_session
      .clone()
      .ok_or("No last session available")?;
    self.list().await?;
    if !self
      .sessions
      .iter()
      .any(|session| session.session_id == target)
    {
      return Err(format!("Session not found: {target}").into());
    }
    self.select(&target).await
  }

  pub(super) async fn select_pane_number(&mut self, number: usize) -> Result<()> {
    let target = self
      .view
      .as_ref()
      .and_then(|view| {
        number
          .checked_sub(1)
          .and_then(|index| view.panes.get(index))
      })
      .map(|pane| pane.terminal_id.clone())
      .ok_or_else(|| format!("Pane not found: {number}"))?;
    if target != self.focused {
      self.unzoom().await?;
    }
    self.focus_pane(target);
    Ok(())
  }

  pub(super) async fn display_panes(&mut self) -> Result<()> {
    self.release_mouse().await?;
    self.keys = KeyState::Root;
    self.pane_labels = self
      .view
      .as_ref()
      .filter(|view| !view.panes.is_empty())
      .map(PaneLabels::new);
    Ok(())
  }

  pub(super) fn expire_pane_labels(&mut self) {
    if self
      .pane_labels
      .as_ref()
      .is_some_and(|labels| self.view.as_ref().is_none_or(|view| !labels.valid(view)))
    {
      self.pane_labels = None;
    }
  }

  pub(super) async fn pane_label_key(&mut self, key: KeyEvent) -> Result<bool> {
    self.expire_pane_labels();
    let Some(labels) = self.pane_labels.take() else {
      return Ok(false);
    };
    // Every key dismisses the labels locally. Modified digits must not select
    // a pane or leak into its process, and labels never renumber while active.
    if key.modifiers.is_empty()
      && let KeyCode::Char(ch @ '1'..='9') = key.code
      && let Some((id, _)) = labels
        .labels
        .iter()
        .find(|(_, number)| *number == (ch as usize - '0' as usize))
    {
      let id = id.clone();
      if id != self.focused {
        self.unzoom().await?;
      }
      self.focus_pane(id);
    }
    Ok(true)
  }

  pub(super) fn open_sessions(&mut self) {
    self.overlay = Overlay::Sessions(
      self
        .sessions
        .get(self.session_index())
        .map(|session| session.session_id.clone()),
    );
  }

  pub(super) fn picker_index(&self, selected: Option<&str>) -> Option<usize> {
    selected.and_then(|id| {
      self
        .sessions
        .iter()
        .position(|session| session.session_id == id)
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::input;
  use crossterm::event::{Event, KeyEventKind, KeyModifiers};
  use ctmux_proto::{SplitAxis, TerminalSize, ViewLayout};
  use std::path::PathBuf;

  fn app() -> App<'static> {
    let mut app = App::new(
      PathBuf::from("unused.sock"),
      true,
      input::parse_prefix("Ctrl+b").unwrap(),
    );
    let canvas_size = TerminalSize {
      columns: 21,
      rows: 8,
      ..TerminalSize::default()
    };
    let layout = ViewLayout::Split {
      axis: SplitAxis::Horizontal,
      children: ["a", "b"]
        .map(|terminal| ViewLayout::Terminal {
          terminal_id: terminal.into(),
        })
        .into(),
      weights: Vec::new(),
    };
    app.view = Some(ViewInfo {
      session_id: "root".into(),
      session_name: "root".into(),
      view_id: "view".into(),
      revision: 0,
      panes: layout.pane_geometry(&canvas_size).unwrap(),
      canvas_size,
      zoomed_terminal_id: None,
      layout,
      terminals: Vec::new(),
    });
    app.focused = "a".into();
    app
  }

  #[tokio::test]
  async fn last_pane_tracks_identity_and_ignores_no_ops_and_failed_targets() -> Result<()> {
    let mut app = app();
    app.focus_pane("b".into());
    app.focus_pane("b".into());
    app.focus_pane("missing".into());
    assert!(app.select_pane_number(0).await.is_err());
    assert!(app.select_pane_number(3).await.is_err());
    app.last_pane().await?;
    assert_eq!(app.focused, "a");
    app.last_pane().await?;
    assert_eq!(app.focused, "b");
    // A remote focus change is remembered for returning to a session without
    // replacing the user's deliberate back target.
    app.focused = "a".into();
    app.remember_focus();
    assert_eq!(app.navigation.last_pane["root"], "a");
    app
      .view
      .as_mut()
      .unwrap()
      .panes
      .retain(|pane| pane.terminal_id != "a");
    app.remember_focus();
    assert!(app.last_pane().await.is_err());
    Ok(())
  }

  #[tokio::test]
  async fn pane_labels_are_local_consume_modifiers_releases_and_paste() -> Result<()> {
    let mut app = app();
    let view = app.view.clone();
    app.display_panes().await?;
    app
      .key(KeyEvent::new_with_kind(
        KeyCode::Char('2'),
        KeyModifiers::NONE,
        KeyEventKind::Release,
      ))
      .await?;
    assert!(app.pane_labels.is_some());
    app
      .key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::CONTROL))
      .await?;
    assert_eq!(app.focused, "a");
    assert!(app.pane_labels.is_none());
    app.display_panes().await?;
    app
      .event(Event::Paste("unexpected child input".into()))
      .await?;
    assert!(app.pane_labels.is_none());
    app.display_panes().await?;
    app
      .key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE))
      .await?;
    assert_eq!(app.focused, "b");
    assert_eq!(app.view, view);
    assert!(app.panes.is_empty());
    app.last_pane().await?;
    assert_eq!(app.focused, "a");
    Ok(())
  }

  #[test]
  fn labels_survive_geometry_and_reconnect_metadata_but_cancel_on_identity_changes() {
    let mut app = app();
    let mut view = app.view.clone().unwrap();
    let mut labels = PaneLabels::new(&view);
    view.revision += 1;
    view.canvas_size.columns += 5;
    view.panes = view.layout.pane_geometry(&view.canvas_size).unwrap();
    assert!(labels.valid(&view));
    view.panes.reverse();
    assert!(!labels.valid(&view));
    view.panes.reverse();
    view.zoomed_terminal_id = Some("b".into());
    assert!(!labels.valid(&view));
    view.zoomed_terminal_id = None;
    view.view_id = "replacement".into();
    assert!(!labels.valid(&view));
    view.view_id = labels.view_id.clone();
    labels.deadline = Instant::now();
    assert!(!labels.valid(&view));
    app.pane_labels = Some(labels);
    app.frame();
    assert!(app.pane_labels.is_none());
  }
}
