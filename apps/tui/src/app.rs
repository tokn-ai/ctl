use crate::{
  Result,
  copy::{Action as CopyAction, CopyMode},
  input::{self, Prefix},
  pane::{Pane, identity},
  render::{Frame, Renderer},
};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind};
use rmux_proto::{
  ClientMessage, LeaseKind, ServerMessage, SessionInfo, SessionStatus, SplitAxis, TerminalSize,
  ViewInfo,
};
use std::{collections::BTreeMap, io, path::PathBuf, time::Duration};
use tokio::{
  sync::mpsc,
  time::{Instant, timeout},
};

enum Overlay {
  None,
  Help,
  Sessions(usize),
  Kill(String),
}

pub struct App {
  socket: PathBuf,
  read_only: bool,
  prefix: Prefix,
  prefix_pending: bool,
  sessions: Vec<SessionInfo>,
  view: Option<ViewInfo>,
  panes: BTreeMap<String, Pane>,
  focused: String,
  size: (u16, u16),
  overlay: Overlay,
  copy_mode: Option<CopyMode>,
  copy_buffer: Option<String>,
  message: String,
  message_until: Instant,
  renderer: Renderer,
  layout_owner: Option<String>,
}

impl App {
  pub fn new(socket: PathBuf, read_only: bool, prefix: Prefix) -> Self {
    Self {
      socket,
      read_only,
      prefix,
      prefix_pending: false,
      sessions: Vec::new(),
      view: None,
      panes: BTreeMap::new(),
      focused: String::new(),
      size: crossterm::terminal::size().unwrap_or((80, 24)),
      overlay: Overlay::None,
      copy_mode: None,
      copy_buffer: None,
      message: String::new(),
      message_until: Instant::now(),
      renderer: Renderer::default(),
      layout_owner: None,
    }
  }

  fn canvas_size(&self) -> TerminalSize {
    TerminalSize {
      columns: self.size.0.max(2),
      rows: self.size.1.saturating_sub(1).max(1),
      pixel_width: 0,
      pixel_height: 0,
    }
  }

  async fn request(&self, message: ClientMessage) -> Result<ServerMessage> {
    timeout(Duration::from_secs(5), async {
      let stream = rmux_ipc::connect_or_start_daemon(&self.socket).await?;
      Ok(rmux_client::request(stream, &identity(), message).await?)
    })
    .await?
  }

  pub async fn start(&mut self, selected: Option<String>) -> Result<()> {
    self.list().await?;
    if let Some(selected) = selected {
      return self.select(&selected).await;
    }
    if let Some(session) = self.sessions.first() {
      self.select(&session.session_id.clone()).await?;
    } else if !self.read_only {
      self.create().await?;
    }
    Ok(())
  }

  async fn list(&mut self) -> Result<()> {
    let ServerMessage::SessionList { mut sessions } =
      self.request(ClientMessage::ListSessions).await?
    else {
      return Err("expected session list".into());
    };
    sessions.retain(|session| session.status == SessionStatus::Running);
    sessions.sort_by_key(|session| (session.created_at_ms, session.session_id.clone()));
    self.sessions = sessions;
    Ok(())
  }

  async fn create(&mut self) -> Result<()> {
    let response = self
      .request(ClientMessage::CreateSession {
        name: None,
        command: None,
        working_directory: std::env::current_dir()
          .ok()
          .map(|path| path.to_string_lossy().into_owned()),
        terminal_size: self.canvas_size(),
      })
      .await?;
    let ServerMessage::SessionCreated { session } = response else {
      return Err("expected created session".into());
    };
    self.list().await?;
    self.select(&session.session_id).await
  }

  async fn find_view(&self, selector: &str) -> Result<ViewInfo> {
    let request = self
      .request(ClientMessage::GetView {
        session: selector.into(),
      })
      .await;
    match request {
      Ok(ServerMessage::ViewSnapshot { view }) => Ok(view),
      Ok(_) => Err("expected view snapshot".into()),
      Err(error) if session_not_found(&error) => {
        // A terminal ID is also a valid attachment target. GetView itself takes
        // a root selector, so resolve membership without taking any leases.
        for root in &self.sessions {
          match self
            .request(ClientMessage::GetView {
              session: root.session_id.clone(),
            })
            .await
          {
            Ok(ServerMessage::ViewSnapshot { view })
              if view.panes.iter().any(|pane| pane.terminal_id == selector) =>
            {
              return Ok(view);
            }
            Err(failure) if !session_not_found(&failure) => return Err(failure),
            _ => {}
          }
        }
        Err(error)
      }
      Err(error) => Err(error),
    }
  }

  async fn select(&mut self, session: &str) -> Result<()> {
    let view = self.find_view(session).await?;
    self.detach().await;
    self.focused = view
      .panes
      .iter()
      .find(|pane| pane.terminal_id == session)
      .or_else(|| view.panes.first())
      .map_or_else(String::new, |pane| pane.terminal_id.clone());
    self.view = Some(view);
    self.overlay = Overlay::None;
    self.prefix_pending = false;
    self.reconcile().await?;
    // The first attachment may have resized the shared canvas.
    self.refresh_view().await
  }

  async fn refresh_view(&mut self) -> Result<()> {
    let Some(view) = &self.view else {
      return Ok(());
    };
    let ServerMessage::ViewSnapshot { view } = self
      .request(ClientMessage::GetView {
        session: view.session_id.clone(),
      })
      .await?
    else {
      return Err("expected view snapshot".into());
    };
    self.view = Some(view);
    self.reconcile().await
  }

  async fn reconcile(&mut self) -> Result<()> {
    let Some(view) = &self.view else {
      return Ok(());
    };
    let ids: Vec<_> = view
      .panes
      .iter()
      .map(|pane| pane.terminal_id.clone())
      .collect();
    let removed: Vec<_> = self
      .panes
      .keys()
      .filter(|id| !ids.contains(id))
      .cloned()
      .collect();
    for id in removed {
      if let Some(mut pane) = self.panes.remove(&id) {
        pane.close().await;
      }
    }
    if !ids.contains(&self.focused) {
      self.focused = ids.first().cloned().unwrap_or_default();
    }
    for id in &ids {
      if self.panes.get(id).is_some_and(|pane| pane.connected) {
        continue;
      }
      let token = if let Some(mut old) = self.panes.remove(id) {
        let token = old.token.clone();
        old.close().await;
        Some(token)
      } else {
        None
      };
      let layout = !self
        .panes
        .values()
        .any(|pane| pane.connected && pane.control.state().leases().layout.owned_by_client)
        && ids.first() == Some(id);
      let opened = timeout(
        Duration::from_secs(5),
        Pane::open(
          &self.socket,
          id,
          self.canvas_size(),
          self.read_only,
          layout,
          token,
        ),
      )
      .await??;
      self.panes.insert(id.clone(), opened);
    }
    Ok(())
  }

  pub async fn detach(&mut self) {
    let panes = std::mem::take(&mut self.panes);
    for (_, mut pane) in panes {
      pane.close().await;
    }
  }

  pub async fn run(&mut self, mut events: mpsc::Receiver<io::Result<Event>>) -> Result<()> {
    let mut tick = tokio::time::interval(Duration::from_millis(33));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut refreshed = Instant::now();
    loop {
      tokio::select! {
        event = events.recv() => {
          let Some(event) = event else { return Ok(()); };
          match self.event(event?).await {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(error) => self.notice(error.to_string()),
          }
        }
        _ = tick.tick() => {
          self.drain().await;
          if refreshed.elapsed() >= Duration::from_secs(2) {
            if let Err(error) = self.refresh().await { self.notice(error.to_string()); }
            refreshed = Instant::now();
          }
          self.draw()?;
        }
      }
    }
  }

  async fn drain(&mut self) {
    let mut notices = Vec::new();
    for pane in self.panes.values_mut() {
      if !pane.connected {
        continue;
      }
      match pane.drain().await {
        Ok(Some(message)) => notices.push(message),
        Ok(None) => {}
        Err(error) => {
          pane.connected = false;
          notices.push(format!("Disconnected: {error}; retrying"));
        }
      }
    }
    let owner = self
      .panes
      .iter()
      .find(|(_, pane)| pane.connected && pane.control.state().leases().layout.owned_by_client)
      .map(|(id, _)| id.clone());
    if owner != self.layout_owner {
      self.layout_owner = owner;
      if let Err(error) = self.resize().await {
        notices.push(error.to_string());
      }
    }
    for message in notices {
      self.notice(message);
    }
  }

  async fn refresh(&mut self) -> Result<()> {
    self.list().await?;
    let exists = self.view.as_ref().is_some_and(|view| {
      self
        .sessions
        .iter()
        .any(|session| session.session_id == view.session_id)
    });
    if exists {
      self.refresh_view().await
    } else {
      self.detach().await;
      self.view = None;
      if let Some(session) = self.sessions.first() {
        self.select(&session.session_id.clone()).await?;
      }
      Ok(())
    }
  }

  fn notice(&mut self, message: String) {
    self.message = message;
    self.message_until = Instant::now() + Duration::from_secs(6);
  }

  async fn event(&mut self, event: Event) -> Result<bool> {
    match event {
      Event::Key(key) if key.kind != KeyEventKind::Release => return self.key(key).await,
      Event::Resize(columns, rows) => {
        self.size = (columns, rows);
        self.resize().await?;
      }
      Event::Paste(text)
        if self.copy_mode.is_none()
          && matches!(self.overlay, Overlay::None)
          && !self.prefix_pending =>
      {
        self.paste(text).await?;
      }
      _ => {}
    }
    Ok(false)
  }

  async fn paste(&self, text: String) -> Result<()> {
    if let Some(pane) = self.panes.get(&self.focused) {
      let data = if pane.model.bracketed_paste {
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
      } else {
        text.into_bytes()
      };
      pane.control.input(data).await?;
    }
    Ok(())
  }

  async fn resize(&self) -> Result<()> {
    if let Some(pane) = self
      .panes
      .values()
      .find(|pane| pane.connected && pane.control.state().leases().layout.owned_by_client)
    {
      pane.control.resize(self.canvas_size()).await?;
    }
    Ok(())
  }

  async fn key(&mut self, key: KeyEvent) -> Result<bool> {
    if let Some(mode) = &mut self.copy_mode {
      match mode.key(key, usize::from(self.size.1.saturating_sub(2))) {
        CopyAction::Stay => {}
        CopyAction::Close => self.copy_mode = None,
        CopyAction::Copy(text) => {
          self.copy_mode = None;
          self.copy_buffer = Some(text.clone());
          let sent = crate::terminal::copy_to_clipboard(&text)?;
          self.notice(if sent {
            "Copied to rmux buffer; clipboard requested (OSC 52)".into()
          } else {
            "Copied to rmux buffer; selection exceeds clipboard limit".into()
          });
        }
      }
      return Ok(false);
    }
    if !matches!(self.overlay, Overlay::None) {
      self.overlay_key(key).await?;
      return Ok(false);
    }
    if self.prefix_pending {
      self.prefix_pending = false;
      if input::matches_prefix(key, &self.prefix) {
        self.send_key(key).await?;
        return Ok(false);
      }
      return self.command(key.code).await;
    }
    if input::matches_prefix(key, &self.prefix) {
      self.prefix_pending = true;
    } else {
      self.send_key(key).await?;
    }
    Ok(false)
  }

  async fn send_key(&self, key: KeyEvent) -> Result<()> {
    if let Some(pane) = self.panes.get(&self.focused) {
      let data = input::encode(key, pane.model.vt.cursor_key_app_mode());
      if !data.is_empty() {
        pane.control.input(data).await?;
      }
    }
    Ok(())
  }

  async fn command(&mut self, code: KeyCode) -> Result<bool> {
    match code {
      KeyCode::Char('[') => {
        if let Some(pane) = self.panes.get(&self.focused) {
          self.copy_mode = Some(CopyMode::new(pane.model.copy_lines()));
        }
      }
      KeyCode::Char(']') if !self.read_only => {
        if let Some(text) = self.copy_buffer.clone() {
          // Reuse the same lease checks and bracketed-paste encoding as a host paste.
          self.paste(text).await?;
        } else {
          self.notice("Copy buffer is empty".into());
        }
      }
      KeyCode::Char('d') => return Ok(true),
      KeyCode::Char('r') => self.renderer.invalidate(),
      KeyCode::Char('o') => self.next_pane(),
      KeyCode::Char('?') => self.overlay = Overlay::Help,
      KeyCode::Char('s' | 'w') => self.overlay = Overlay::Sessions(self.session_index()),
      KeyCode::Char('n') => self.next_session(1).await?,
      KeyCode::Char('p') => self.next_session(-1).await?,
      KeyCode::Char('c') if !self.read_only => self.create().await?,
      KeyCode::Char('%') if !self.read_only => self.split(SplitAxis::Horizontal).await?,
      KeyCode::Char('"') if !self.read_only => self.split(SplitAxis::Vertical).await?,
      KeyCode::Char('x') if !self.read_only => self.overlay = Overlay::Kill(self.focused.clone()),
      KeyCode::Char('I') if !self.read_only => self.toggle_lease(LeaseKind::Input).await?,
      KeyCode::Char('R') if !self.read_only => self.toggle_lease(LeaseKind::Layout).await?,
      KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right => self.focus(code),
      KeyCode::Esc => {}
      _ => self.notice(format!("{} ? for commands", self.prefix.label)),
    }
    Ok(false)
  }

  async fn toggle_lease(&mut self, lease: LeaseKind) -> Result<()> {
    // Layout belongs to the view, so release its owner regardless of focus.
    let owner = self
      .panes
      .iter()
      .find(|(_, pane)| {
        let leases = pane.control.state().leases();
        match lease {
          LeaseKind::Layout => leases.layout.owned_by_client,
          LeaseKind::Input => false,
        }
      })
      .map(|(id, _)| id.clone());
    let id = if lease == LeaseKind::Layout {
      owner.as_ref().unwrap_or(&self.focused)
    } else {
      &self.focused
    };
    if let Some(pane) = self.panes.get(id) {
      let leases = pane.control.state().leases();
      let held_by_client = match lease {
        LeaseKind::Input => leases.input.owned_by_client,
        LeaseKind::Layout => leases.layout.owned_by_client,
      };
      if held_by_client {
        pane.control.release_lease(lease).await?;
      } else {
        pane.control.acquire_lease(lease).await?;
      }
    }
    Ok(())
  }

  async fn split(&mut self, axis: SplitAxis) -> Result<()> {
    if self.focused.is_empty() {
      return Ok(());
    }
    let previous: Vec<_> = self.panes.keys().cloned().collect();
    let response = self
      .request(ClientMessage::SplitTerminal {
        terminal_id: self.focused.clone(),
        axis,
        command: None,
        working_directory: None,
        terminal_size: self.canvas_size(),
      })
      .await?;
    if let ServerMessage::ViewSnapshot { view } = response {
      if let Some(pane) = view
        .panes
        .iter()
        .find(|pane| !previous.contains(&pane.terminal_id))
      {
        self.focused.clone_from(&pane.terminal_id);
      }
      self.view = Some(view);
      self.reconcile().await?;
    }
    Ok(())
  }

  fn session_index(&self) -> usize {
    self
      .sessions
      .iter()
      .position(|session| {
        self
          .view
          .as_ref()
          .is_some_and(|view| view.session_id == session.session_id)
      })
      .unwrap_or(0)
  }

  async fn next_session(&mut self, direction: i32) -> Result<()> {
    if self.sessions.is_empty() {
      return Ok(());
    }
    let current = self.session_index();
    let index = if direction > 0 {
      (current + 1) % self.sessions.len()
    } else {
      (current + self.sessions.len() - 1) % self.sessions.len()
    };
    self.select(&self.sessions[index].session_id.clone()).await
  }

  fn next_pane(&mut self) {
    let Some(view) = &self.view else {
      return;
    };
    if view.panes.is_empty() {
      return;
    }
    let index = view
      .panes
      .iter()
      .position(|pane| pane.terminal_id == self.focused)
      .unwrap_or(0);
    self
      .focused
      .clone_from(&view.panes[(index + 1) % view.panes.len()].terminal_id);
  }

  fn focus(&mut self, direction: KeyCode) {
    let Some(view) = &self.view else {
      return;
    };
    if let Some(id) = adjacent(&view.panes, &self.focused, direction) {
      self.focused = id;
    }
  }

  async fn overlay_key(&mut self, key: KeyEvent) -> Result<()> {
    match &mut self.overlay {
      Overlay::Sessions(index) => match key.code {
        KeyCode::Up => *index = index.saturating_sub(1),
        KeyCode::Down => *index = (*index + 1).min(self.sessions.len().saturating_sub(1)),
        KeyCode::Enter => {
          let selected = self
            .sessions
            .get(*index)
            .map(|session| session.session_id.clone());
          self.overlay = Overlay::None;
          if let Some(selected) = selected {
            self.select(&selected).await?;
          }
        }
        KeyCode::Esc => self.overlay = Overlay::None,
        _ => {}
      },
      Overlay::Kill(id) => {
        let id = id.clone();
        self.overlay = Overlay::None;
        if key.code == KeyCode::Char('y') && !id.is_empty() {
          self
            .request(ClientMessage::KillTerminal { terminal_id: id })
            .await?;
          self.refresh().await?;
        }
      }
      Overlay::Help => self.overlay = Overlay::None,
      Overlay::None => {}
    }
    Ok(())
  }

  fn draw(&mut self) -> Result<()> {
    let mut frame = Frame::new(self.size.0, self.size.1);
    if let Some(mode) = &mut self.copy_mode {
      mode.fit(
        usize::from(self.size.0),
        usize::from(self.size.1.saturating_sub(1)),
      );
      frame.copy_mode(mode);
      if self.size.1 > 0 {
        frame.text(0, self.size.1 - 1, &mode.status(), true);
      }
      self.renderer.draw(frame)?;
      return Ok(());
    }
    if let Some(view) = &self.view {
      frame.canvas(view, &self.panes, &self.focused);
    } else {
      let instructions = if self.read_only {
        format!("No running sessions. {} d detaches.", self.prefix.label)
      } else {
        format!(
          "No running sessions. {} c creates one; {} d detaches.",
          self.prefix.label, self.prefix.label
        )
      };
      frame.text(0, 0, &instructions, false);
    }
    frame.overlay(&self.overlay_lines());
    if self.size.1 > 0 {
      frame.text(0, self.size.1 - 1, &self.status(), true);
    }
    self.renderer.draw(frame)?;
    Ok(())
  }

  fn status(&self) -> String {
    if self.prefix_pending {
      return "PREFIX  % split right  \" split below  arrows focus  c new  s sessions  d detach  ? help".into();
    }
    if Instant::now() < self.message_until {
      return self.message.clone();
    }
    let name = self
      .view
      .as_ref()
      .map_or("no session", |view| view.session_name.as_str());
    let index = self
      .view
      .as_ref()
      .and_then(|view| {
        view
          .panes
          .iter()
          .position(|pane| pane.terminal_id == self.focused)
      })
      .map_or(0, |index| index + 1);
    let input = self
      .panes
      .get(&self.focused)
      .is_some_and(|pane| pane.control.state().leases().input.owned_by_client);
    let layout = self
      .panes
      .values()
      .any(|pane| pane.connected && pane.control.state().leases().layout.owned_by_client);
    let connected = self
      .panes
      .get(&self.focused)
      .is_some_and(|pane| pane.connected);
    format!(
      " rmux [{name}] pane {index} | {} | {} | {} ? help | {}/{} sessions ",
      if !connected {
        "reconnecting"
      } else if input {
        "input"
      } else {
        "view only"
      },
      if layout {
        "resize owner"
      } else {
        "shared size"
      },
      self.prefix.label,
      self.session_index() + usize::from(!self.sessions.is_empty()),
      self.sessions.len()
    )
  }

  fn overlay_lines(&self) -> Vec<String> {
    match &self.overlay {
      Overlay::None => Vec::new(),
      Overlay::Kill(_) => vec!["Terminate active pane? y confirms; any other key cancels".into()],
      Overlay::Sessions(index) => {
        let mut lines = vec!["Sessions — ↑/↓ choose, Enter opens, Esc cancels".into()];
        let capacity = usize::from(self.size.1.saturating_sub(2)).max(1);
        let start = index.saturating_sub(capacity - 1);
        lines.extend(
          self
            .sessions
            .iter()
            .enumerate()
            .skip(start)
            .take(capacity)
            .map(|(i, session)| {
              format!("{} {}", if i == *index { ">" } else { " " }, session.name)
            }),
        );
        lines
      }
      Overlay::Help => vec![
        format!("Commands after {} — any key closes help", self.prefix.label),
        "%: split right    \": split below".into(),
        "Arrows: focus pane    o: next pane    x: terminate (confirm)".into(),
        "c: new session    n/p: next/previous session    s/w: session list".into(),
        "[: history/copy mode    ]: paste copied text".into(),
        "r: redraw    I: take/release input    R: take/release resize".into(),
        "d: detach (sessions keep running)    Esc: cancel prefix".into(),
        format!(
          "{} twice sends the prefix to the active pane.",
          self.prefix.label
        ),
      ],
    }
  }
}

fn session_not_found(error: &crate::Error) -> bool {
  matches!(
    error.downcast_ref::<rmux_client::ClientError>(),
    Some(rmux_client::ClientError::Server {
      code: rmux_proto::ErrorCode::SessionNotFound,
      ..
    })
  )
}

fn adjacent(
  panes: &[rmux_proto::PaneGeometry],
  focused: &str,
  direction: KeyCode,
) -> Option<String> {
  let origin = panes.iter().find(|pane| pane.terminal_id == focused)?;
  let horizontal = matches!(direction, KeyCode::Left | KeyCode::Right);
  let sign = if matches!(direction, KeyCode::Right | KeyCode::Down) {
    1
  } else {
    -1
  };
  let center = |pane: &rmux_proto::PaneGeometry| {
    (
      i32::from(pane.left) * 2 + i32::from(pane.columns),
      i32::from(pane.top) * 2 + i32::from(pane.rows),
    )
  };
  let (x, y) = center(origin);
  panes
    .iter()
    .filter(|pane| pane.terminal_id != focused)
    .filter_map(|pane| {
      let (px, py) = center(pane);
      let distance = if horizontal { px - x } else { py - y } * sign;
      let cross = if horizontal { py - y } else { px - x }.abs();
      (distance > 0).then_some((cross, distance, pane.terminal_id.clone()))
    })
    .min()
    .map(|(_, _, id)| id)
}

#[cfg(all(test, unix))]
#[path = "app_tests.rs"]
mod tests;
