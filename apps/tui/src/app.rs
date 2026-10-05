use crate::{
  Result,
  copy::{Action as CopyAction, BottomBehavior, CopyMode},
  input::{self, Prefix},
  pane::{Pane, identity},
  render::{Frame, Renderer, pane_at, pane_position},
  transport::{LocalTransport, Transport},
};
use crossterm::event::{
  Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ctmux_proto::{
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
  Archives(usize),
  ArchiveTerminals(Box<ctmux_client::archive::SessionArchive>, usize),
}

#[derive(Clone)]
enum CopyTarget {
  Pane(String),
  Archive,
}

enum MouseCapture {
  Selection {
    target: CopyTarget,
    pending: Option<CopyMode>,
  },
  Application {
    terminal_id: String,
    button: MouseButton,
    position: (usize, usize),
  },
}

pub struct App<'a> {
  pub transport: Option<&'a dyn Transport>,
  socket: PathBuf,
  archive_directory: Option<PathBuf>,
  read_only: bool,
  archive_only: bool,
  prefix: Prefix,
  prefix_pending: bool,
  sessions: Vec<SessionInfo>,
  archives: Vec<ctmux_client::archive::SessionArchive>,
  ended: Option<String>,
  selected_id: String,
  archived_panes: Vec<ctmux_client::archive::ArchivedPane>,
  view: Option<ViewInfo>,
  panes: BTreeMap<String, Pane>,
  focused: String,
  size: (u16, u16),
  overlay: Overlay,
  copies: BTreeMap<String, CopyMode>,
  archive_copy: Option<CopyMode>,
  mouse_capture: Option<MouseCapture>,
  copy_buffer: Option<String>,
  message: String,
  message_until: Instant,
  renderer: Renderer,
  layout_owner: Option<String>,
}

impl App<'_> {
  pub fn new(socket: PathBuf, read_only: bool, prefix: Prefix) -> Self {
    Self {
      transport: None,
      archive_directory: cfg!(test).then(|| socket.with_extension("client-archives")),
      socket,
      read_only,
      archive_only: false,
      prefix,
      prefix_pending: false,
      sessions: Vec::new(),
      archives: Vec::new(),
      ended: None,
      selected_id: String::new(),
      archived_panes: Vec::new(),
      view: None,
      panes: BTreeMap::new(),
      focused: String::new(),
      size: crossterm::terminal::size().unwrap_or((80, 24)),
      overlay: Overlay::None,
      copies: BTreeMap::new(),
      archive_copy: None,
      mouse_capture: None,
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
      let local = LocalTransport(self.socket.clone());
      let stream = self.transport.unwrap_or(&local).connect().await?;
      Ok(ctmux_client::request(stream, &identity(), message).await?)
    })
    .await?
  }

  fn archive_key(&self) -> String {
    self.transport.map_or_else(
      || self.socket.to_string_lossy().into_owned(),
      Transport::archive_key,
    )
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

  pub fn open_archive(&mut self, session_id: &str) -> Result<()> {
    self.archive_only = true;
    self.archives = self.local_archives()?;
    let archive = self
      .archives
      .iter()
      .find(|archive| archive.session_id == session_id)
      .cloned()
      .ok_or("archive not found on this client")?;
    self.overlay = Overlay::ArchiveTerminals(Box::new(archive), 0);
    Ok(())
  }

  fn archive_store(&self) -> std::io::Result<ctmux_client::archive::ArchiveStore> {
    match &self.archive_directory {
      Some(directory) => Ok(ctmux_client::archive::ArchiveStore::new(directory.clone())),
      None => ctmux_client::archive::ArchiveStore::for_client("tui"),
    }
  }

  fn local_archives(&self) -> Result<Vec<ctmux_client::archive::SessionArchive>> {
    Ok(
      self
        .archive_store()?
        .list()?
        .into_iter()
        .filter(|archive| archive.host_key == self.archive_key())
        .collect(),
    )
  }

  fn save_archive(&self) -> Result<()> {
    use ctmux_client::archive::{ArchivedPane, SessionArchive};
    let mut terminals = self.archived_panes.clone();
    terminals.extend(
      self
        .panes
        .iter()
        .filter(|(_, pane)| pane.ended.is_some() || self.ended.is_some())
        .map(|(id, pane)| ArchivedPane {
          terminal_id: id.clone(),
          reason: pane
            .ended
            .clone()
            .or(self.ended.clone())
            .unwrap_or_default(),
          lines: pane.model.copy_lines(),
          history_gap: pane.history_gap(),
        }),
    );
    if terminals.is_empty() {
      terminals.push(ArchivedPane {
        terminal_id: self.selected_id.clone(),
        reason: self.ended.clone().unwrap_or_else(|| "Missing".into()),
        lines: Vec::new(),
        history_gap: true,
      });
    }
    let session = self
      .sessions
      .iter()
      .find(|session| session.session_id == self.selected_id);
    self.archive_store()?.save(SessionArchive {
      session_id: self.selected_id.clone(),
      name: session.map_or_else(
        || {
          self.view.as_ref().map_or_else(
            || self.selected_id.clone(),
            |view| view.session_name.clone(),
          )
        },
        |session| session.name.clone(),
      ),
      host_key: self.archive_key(),
      archived_at_ms: 0,
      expires_at_ms: 0,
      terminals,
    })?;
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
    self.release_mouse().await?;
    self.copies.clear();
    self.archive_copy = None;
    self.mouse_capture = None;
    session.clone_into(&mut self.selected_id);
    let view = match self.find_view(session).await {
      Ok(view) => view,
      Err(error) if session_not_found(&error) => {
        self.ended = Some("Session no longer exists — press any key to exit".into());
        return Ok(());
      }
      Err(error) => return Err(error),
    };
    self.ended = None;
    self.archived_panes.clear();
    self.selected_id.clone_from(&view.session_id);
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
    if self.panes.values().any(|pane| pane.ended.is_some()) {
      return Ok(());
    }
    let response = self
      .request(ClientMessage::GetView {
        session: view.session_id.clone(),
      })
      .await;
    let response = match response {
      Ok(response) => response,
      Err(error) if session_not_found(&error) => {
        self.ended = Some("Session no longer exists — press any key to exit".into());
        return Ok(());
      }
      Err(error) => return Err(error),
    };
    let ServerMessage::ViewSnapshot { view } = response else {
      return Err("expected view snapshot".into());
    };
    let mut missing = false;
    for (id, pane) in &mut self.panes {
      if !view
        .terminals
        .iter()
        .any(|terminal| terminal.terminal_id == *id)
      {
        pane
          .ended
          .get_or_insert_with(|| "Terminal no longer exists".into());
        pane.connected = false;
        missing = true;
      }
    }
    if missing {
      return Ok(());
    }
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
      self.copies.remove(&id);
      if let Some(mut pane) = self.panes.remove(&id) {
        pane.close().await;
      }
    }
    if let Some(
      MouseCapture::Application {
        terminal_id: id, ..
      }
      | MouseCapture::Selection {
        target: CopyTarget::Pane(id),
        ..
      },
    ) = &self.mouse_capture
      && !ids.contains(id)
    {
      self.mouse_capture = None;
    }
    if !ids.contains(&self.focused) {
      self.focused = ids.first().cloned().unwrap_or_default();
    }
    for id in &ids {
      if self
        .panes
        .get(id)
        .is_some_and(|pane| pane.connected || pane.ended.is_some())
      {
        continue;
      }
      let token = if let Some(old) = self.panes.get_mut(id) {
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
      let local = LocalTransport(self.socket.clone());
      let opened = timeout(
        Duration::from_secs(5),
        Pane::open(
          self.transport.unwrap_or(&local),
          id,
          self.canvas_size(),
          self.read_only,
          layout,
          token,
        ),
      )
      .await?;
      match opened {
        Ok(opened) => {
          self.panes.insert(id.clone(), opened);
        }
        Err(error) if session_not_found(&error) => {
          if let Some(pane) = self.panes.get_mut(id) {
            pane.connected = false;
            pane.ended = Some("Terminal no longer exists".into());
          } else {
            self.ended = Some("Terminal no longer exists — press any key to exit".into());
          }
        }
        Err(error) => return Err(error),
      }
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
          if !self.archive_only && refreshed.elapsed() >= Duration::from_secs(2) {
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
      // A closed transport may still have a final SessionEnded event queued.
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
    if self.ended.is_some() {
      return Ok(());
    }
    if !self.panes.is_empty() && self.panes.values().all(|pane| pane.ended.is_some()) {
      let outcome = self
        .panes
        .get(&self.focused)
        .and_then(|pane| pane.ended.as_deref())
        .unwrap_or("Session ended");
      self.ended = Some(format!("{outcome} — press any key to exit"));
      return Ok(());
    }
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
      if self.view.is_some() {
        let outcome = self
          .panes
          .get(&self.focused)
          .and_then(|pane| pane.ended.clone())
          .unwrap_or_else(|| "Session no longer exists".into());
        self.ended = Some(format!("{outcome} — press any key to exit"));
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
      Event::Mouse(mouse) => self.mouse(mouse).await?,
      Event::Key(key)
        if key.kind != KeyEventKind::Release
          && key.code == KeyCode::PageUp
          && key.modifiers == KeyModifiers::SHIFT
          && self.active_copy().is_none()
          && matches!(self.overlay, Overlay::None) =>
      {
        self.command(KeyCode::Char('[')).await?;
        let (width, height) = self.copy_size(&CopyTarget::Pane(self.focused.clone()));
        if let Some(mode) = self.copies.get_mut(&self.focused) {
          mode.fit(width, height);
          mode.scroll(true, height, height);
        }
      }
      Event::Key(key) if key.kind != KeyEventKind::Release => return self.key(key).await,
      Event::Resize(columns, rows) => {
        self.size = (columns, rows);
        self.resize().await?;
      }
      Event::Paste(text)
        if self.active_copy().is_none()
          && matches!(self.overlay, Overlay::None)
          && !self.prefix_pending =>
      {
        self.paste(text).await?;
      }
      _ => {}
    }
    Ok(false)
  }

  fn active_copy(&self) -> Option<&CopyMode> {
    self
      .archive_copy
      .as_ref()
      .or_else(|| self.copies.get(&self.focused))
  }

  fn copy_mut(&mut self, target: &CopyTarget) -> Option<&mut CopyMode> {
    match target {
      CopyTarget::Pane(id) => self.copies.get_mut(id),
      CopyTarget::Archive => self.archive_copy.as_mut(),
    }
  }

  fn copy_size(&self, target: &CopyTarget) -> (usize, usize) {
    if let CopyTarget::Pane(id) = target
      && let Some(rect) = self
        .view
        .as_ref()
        .and_then(|view| view.panes.iter().find(|rect| &rect.terminal_id == id))
    {
      return (usize::from(rect.columns), usize::from(rect.rows));
    }
    (
      usize::from(self.size.0),
      usize::from(self.size.1.saturating_sub(1)),
    )
  }

  fn new_copy(&self, id: &str) -> Option<CopyMode> {
    let pane = self.panes.get(id)?;
    let (width, height) = self.copy_size(&CopyTarget::Pane(id.into()));
    let (prefix, rows) = pane.model.copy_snapshot();
    let mut mode = CopyMode::from_rows(prefix, rows, width);
    mode.history_gap = pane.history_gap();
    mode.fit(width, height);
    Some(mode)
  }

  fn copy_action(&mut self, target: &CopyTarget, action: CopyAction) -> Result<()> {
    if matches!(action, CopyAction::Stay) {
      return Ok(());
    }
    match target {
      CopyTarget::Pane(id) => {
        self.copies.remove(id);
      }
      CopyTarget::Archive => self.archive_copy = None,
    }
    self.mouse_capture = None;
    if let CopyAction::Copy(text) = action {
      self.copy_buffer = Some(text.clone());
      let sent = crate::terminal::copy_to_clipboard(&text)?;
      self.notice(if sent {
        "Copied to ctmux buffer; clipboard requested (OSC 52)".into()
      } else {
        "Copied to ctmux buffer; selection exceeds clipboard limit".into()
      });
    }
    Ok(())
  }

  fn mouse_position(&self, target: &CopyTarget, mouse: MouseEvent) -> Option<(usize, usize)> {
    let position = match target {
      CopyTarget::Pane(id) => pane_position(
        self.view.as_ref()?,
        &self.panes,
        &self.copies,
        &self.focused,
        self.size,
        id,
        (mouse.column, mouse.row),
      )?,
      CopyTarget::Archive => (
        mouse.column.min(self.size.0.saturating_sub(1)),
        mouse.row.min(self.size.1.saturating_sub(2)),
      ),
    };
    Some((usize::from(position.0), usize::from(position.1)))
  }

  async fn send_mouse(
    &self,
    id: &str,
    mut mouse: MouseEvent,
    position: (usize, usize),
  ) -> Result<()> {
    if let Some(pane) = self.panes.get(id)
      && !self.read_only
      && pane.connected
      && pane.ended.is_none()
      && pane.control.state().leases().input.owned_by_client
    {
      mouse.column = u16::try_from(position.0).expect("pane column");
      mouse.row = u16::try_from(position.1).expect("pane row");
      if let Some(data) = input::encode_mouse(mouse, pane.model.input_modes.mouse()) {
        pane.control.input(data).await?;
      }
    }
    Ok(())
  }

  async fn release_mouse(&mut self) -> Result<()> {
    if let Some(MouseCapture::Application {
      terminal_id,
      button,
      position,
    }) = self.mouse_capture.take()
    {
      self
        .send_mouse(
          &terminal_id,
          MouseEvent {
            kind: MouseEventKind::Up(button),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
          },
          position,
        )
        .await?;
    }
    Ok(())
  }

  // A drag belongs to the pane where it began, even across borders or the footer.
  async fn captured_mouse(&mut self, mouse: MouseEvent, capture: MouseCapture) -> Result<()> {
    match capture {
      MouseCapture::Application {
        terminal_id,
        button,
        position,
      } => {
        let position = self
          .mouse_position(&CopyTarget::Pane(terminal_id.clone()), mouse)
          .unwrap_or(position);
        self.send_mouse(&terminal_id, mouse, position).await?;
        if mouse.kind != MouseEventKind::Up(button) {
          self.mouse_capture = Some(MouseCapture::Application {
            terminal_id,
            button,
            position,
          });
        }
      }
      MouseCapture::Selection { target, pending } => {
        if !matches!(
          mouse.kind,
          MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
        ) {
          self.mouse_capture = Some(MouseCapture::Selection { target, pending });
          return Ok(());
        }
        let Some((x, y)) = self.mouse_position(&target, mouse) else {
          return Ok(());
        };
        if mouse.kind == MouseEventKind::Up(MouseButton::Left) {
          // A click only focuses; its pending snapshot never replaces the live view.
          if pending.is_none()
            && let Some(mode) = self.copy_mut(&target)
          {
            mode.drag(x, y);
            let action = mode.finish_drag();
            self.copy_action(&target, action)?;
          }
        } else {
          if let Some(mode) = pending
            && let CopyTarget::Pane(id) = &target
          {
            self.copies.insert(id.clone(), mode);
          }
          if let Some(mode) = self.copy_mut(&target) {
            mode.drag(x, y);
          }
          self.mouse_capture = Some(MouseCapture::Selection {
            target,
            pending: None,
          });
        }
      }
    }
    Ok(())
  }

  async fn mouse(&mut self, mouse: MouseEvent) -> Result<()> {
    if matches!(mouse.kind, MouseEventKind::Drag(_) | MouseEventKind::Up(_))
      && let Some(capture) = self.mouse_capture.take()
    {
      return self.captured_mouse(mouse, capture).await;
    }
    if self.prefix_pending {
      return Ok(());
    }
    if mouse.column >= self.size.0 || mouse.row >= self.size.1.saturating_sub(1) {
      return Ok(());
    }
    let target = if self.archive_copy.is_some() {
      CopyTarget::Archive
    } else {
      if !matches!(self.overlay, Overlay::None) {
        return Ok(());
      }
      let Some(id) = self.view.as_ref().and_then(|view| {
        pane_at(
          view,
          &self.panes,
          &self.copies,
          &self.focused,
          self.size,
          (mouse.column, mouse.row),
        )
      }) else {
        return Ok(());
      };
      CopyTarget::Pane(id.to_owned())
    };
    // Capture coordinates before focus can shift a clipped shared viewport.
    let Some(position) = self.mouse_position(&target, mouse) else {
      return Ok(());
    };
    if let CopyTarget::Pane(id) = &target {
      if matches!(
        mouse.kind,
        MouseEventKind::Down(_) | MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
      ) {
        self.focused.clone_from(id);
      }
      let application = !self.copies.contains_key(id)
        && !mouse.modifiers.contains(KeyModifiers::SHIFT)
        && !self.read_only
        && self.panes.get(id).is_some_and(|pane| {
          pane.connected
            && pane.ended.is_none()
            && pane.control.state().leases().input.owned_by_client
            && pane.model.input_modes.mouse().enabled()
        });
      if application {
        if let MouseEventKind::Down(button) = mouse.kind {
          self.mouse_capture = Some(MouseCapture::Application {
            terminal_id: id.clone(),
            button,
            position,
          });
        }
        return self.send_mouse(id, mouse, position).await;
      }
    }
    match mouse.kind {
      MouseEventKind::Down(MouseButton::Left) => {
        let pending = if let Some(mode) = self.copy_mut(&target) {
          mode.begin_drag(position.0, position.1);
          None
        } else if let CopyTarget::Pane(id) = &target {
          self.new_copy(id).map(|mut mode| {
            mode.begin_drag(position.0, position.1);
            mode
          })
        } else {
          None
        };
        self.mouse_capture = Some(MouseCapture::Selection { target, pending });
      }
      MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
        let up = mouse.kind == MouseEventKind::ScrollUp;
        let (width, height) = self.copy_size(&target);
        if let CopyTarget::Pane(id) = &target
          && up
          && !self.copies.contains_key(id)
          && let Some(mut mode) = self.new_copy(id)
        {
          mode.bottom_behavior = BottomBehavior::ReturnToLive;
          self.copies.insert(id.clone(), mode);
        }
        if let Some(mode) = self.copy_mut(&target) {
          mode.fit(width, height);
          let action = mode.scroll(up, 5, height);
          self.copy_action(&target, action)?;
        }
      }
      _ => {}
    }
    Ok(())
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
    if self.archive_copy.is_some() {
      let target = CopyTarget::Archive;
      let (_, height) = self.copy_size(&target);
      let action = self
        .archive_copy
        .as_mut()
        .expect("archive copy")
        .key(key, height);
      self.copy_action(&target, action)?;
      return Ok(false);
    }
    if matches!(self.overlay, Overlay::None) {
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
        return Ok(false);
      }
      let target = CopyTarget::Pane(self.focused.clone());
      let (_, height) = self.copy_size(&target);
      if let Some(mode) = self.copies.get_mut(&self.focused) {
        let action = mode.key(key, height);
        self.copy_action(&target, action)?;
        return Ok(false);
      }
    }
    if self.ended.is_some() {
      for pane in self.panes.values_mut() {
        pane.finish_ended_history().await;
      }
      self.save_archive()?;
      return Ok(true);
    }
    if self
      .panes
      .get(&self.focused)
      .is_some_and(|pane| pane.ended.is_some())
    {
      if let Some(pane) = self.panes.get_mut(&self.focused) {
        pane.finish_ended_history().await;
      }
      self.save_archive()?;
      if let Some(mut pane) = self.panes.remove(&self.focused) {
        self
          .archived_panes
          .push(ctmux_client::archive::ArchivedPane {
            terminal_id: self.focused.clone(),
            reason: pane.ended.clone().unwrap_or_default(),
            lines: pane.model.copy_lines(),
            history_gap: pane.history_gap(),
          });
        pane.close().await;
      }
      if self.panes.is_empty() {
        return Ok(true);
      }
      self.focused = self.panes.keys().next().cloned().unwrap_or_default();
      self.refresh().await?;
      return Ok(false);
    }
    if !matches!(self.overlay, Overlay::None) {
      self.overlay_key(key).await?;
      return Ok(false);
    }
    self.send_key(key).await?;
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
      KeyCode::Char('A') => {
        self.archives = self.local_archives()?;
        self.overlay = Overlay::Archives(0);
      }
      KeyCode::Char('[') | KeyCode::PageUp => {
        self.release_mouse().await?;
        let id = self.focused.clone();
        if !self.copies.contains_key(&id)
          && let Some(mode) = self.new_copy(&id)
        {
          self.copies.insert(id.clone(), mode);
        }
        if code == KeyCode::PageUp {
          let (_, height) = self.copy_size(&CopyTarget::Pane(id.clone()));
          if let Some(mode) = self.copies.get_mut(&id) {
            mode.scroll(true, height, height);
          }
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
      Overlay::Archives(index) => match key.code {
        KeyCode::Up => *index = index.saturating_sub(1),
        KeyCode::Down => *index = (*index + 1).min(self.archives.len().saturating_sub(1)),
        KeyCode::Enter => {
          if let Some(archive) = self.archives.get(*index).cloned() {
            self.overlay = Overlay::ArchiveTerminals(Box::new(archive), 0);
          }
        }
        KeyCode::Esc => self.overlay = Overlay::None,
        _ => {}
      },
      Overlay::ArchiveTerminals(archive, index) => match key.code {
        KeyCode::Up => *index = index.saturating_sub(1),
        KeyCode::Down => *index = (*index + 1).min(archive.terminals.len().saturating_sub(1)),
        KeyCode::Enter => {
          if let Some(terminal) = archive.terminals.get(*index) {
            let mut mode = CopyMode::new(terminal.lines.clone());
            mode.history_gap = terminal.history_gap;
            self.archive_copy = Some(mode);
          }
        }
        KeyCode::Esc => self.overlay = Overlay::Archives(0),
        _ => {}
      },
      Overlay::Help => self.overlay = Overlay::None,
      Overlay::None => {}
    }
    Ok(())
  }

  fn frame(&mut self) -> Frame {
    let mut frame = Frame::new(self.size.0, self.size.1);
    if let Some(mode) = &mut self.archive_copy {
      mode.fit(
        usize::from(self.size.0),
        usize::from(self.size.1.saturating_sub(1)),
      );
      frame.copy_mode(mode);
    } else if let Some(view) = &self.view {
      for rect in &view.panes {
        if let Some(mode) = self.copies.get_mut(&rect.terminal_id) {
          mode.fit(usize::from(rect.columns), usize::from(rect.rows));
        }
      }
      frame.canvas(view, &self.panes, &self.copies, &self.focused);
      frame.overlay(&self.overlay_lines());
    } else {
      let instructions = if let Some(ended) = &self.ended {
        ended.clone()
      } else if self.read_only {
        format!("No running sessions. {} d detaches.", self.prefix.label)
      } else {
        format!(
          "No running sessions. {} c creates one; {} d detaches.",
          self.prefix.label, self.prefix.label
        )
      };
      frame.text(0, 0, &instructions, false);
      frame.overlay(&self.overlay_lines());
    }
    if self.size.1 > 0 {
      frame.text(0, self.size.1 - 1, &self.status(), true);
    }
    frame
  }

  fn draw(&mut self) -> Result<()> {
    let frame = self.frame();
    self.renderer.draw(frame)?;
    Ok(())
  }

  fn status(&self) -> String {
    if let Some(mode) = &self.archive_copy {
      return format!(" archive | {}", mode.status());
    }
    if let Some(mode) = self.copies.get(&self.focused)
      && matches!(self.overlay, Overlay::None)
      && !self.prefix_pending
    {
      return format!(" {} | {}", self.connection_history_status(), mode.status());
    }

    if matches!(
      self.overlay,
      Overlay::Archives(_) | Overlay::ArchiveTerminals(..)
    ) {
      return "Archived sessions — read only; Esc returns".into();
    }
    if let Some(ended) = &self.ended {
      return ended.clone();
    }
    if let Some(ended) = self
      .panes
      .get(&self.focused)
      .and_then(|pane| pane.ended.as_ref())
    {
      return format!("{ended} — press any key to close pane");
    }
    if self.prefix_pending {
      return format!(
        " {} | PREFIX  % split right  \" split below  arrows focus  c new  s sessions  d detach  ? help",
        self.connection_history_status()
      );
    }
    if Instant::now() < self.message_until {
      return format!(" {} | {}", self.connection_history_status(), self.message);
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
    format!(
      " {} | ctmux [{name}] pane {index} | {} | {} | {} ? help | {}/{} sessions ",
      self.connection_history_status(),
      if input { "input" } else { "view only" },
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

  fn connection_status(&self) -> &'static str {
    match self.panes.get(&self.focused) {
      Some(pane) if pane.ended.is_some() => "ended",
      Some(pane) if pane.connected => "connected",
      Some(_) => "reconnecting",
      None => "no connection",
    }
  }

  fn connection_history_status(&self) -> String {
    let connection = self.connection_status();
    self.panes.get(&self.focused).map_or_else(
      || connection.to_owned(),
      |pane| format!("{connection} | {}", pane.history_status()),
    )
  }

  fn overlay_lines(&self) -> Vec<String> {
    match &self.overlay {
      Overlay::Archives(index) => {
        let mut lines =
          vec!["Archived sessions (read only) — arrows choose, Enter opens, Esc closes".into()];
        if self.archives.is_empty() {
          lines.push("No retained archives".into());
        }
        let start = index.saturating_sub(usize::from(self.size.1.saturating_sub(3)));
        lines.extend(
          self
            .archives
            .iter()
            .enumerate()
            .skip(start)
            .map(|(i, archive)| {
              format!(
                "{} {}  {}",
                if i == *index { ">" } else { " " },
                archive.name,
                archive.session_id
              )
            }),
        );
        lines
      }
      Overlay::ArchiveTerminals(archive, index) => {
        let mut lines = vec![format!(
          "{} — archived terminals; Enter browses/copies, Esc returns",
          archive.name
        )];
        let start = index.saturating_sub(usize::from(self.size.1.saturating_sub(3)));
        lines.extend(
          archive
            .terminals
            .iter()
            .enumerate()
            .skip(start)
            .map(|(i, terminal)| {
              format!(
                "{} {} {}",
                if i == *index { ">" } else { " " },
                terminal.terminal_id,
                terminal.reason
              )
            }),
        );
        lines
      }
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
        "[: history/copy mode    ]: paste copied text    A: archives".into(),
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
    error.downcast_ref::<ctmux_client::ClientError>(),
    Some(ctmux_client::ClientError::Server {
      code: ctmux_proto::ErrorCode::SessionNotFound,
      ..
    })
  )
}

fn adjacent(
  panes: &[ctmux_proto::PaneGeometry],
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
  let center = |pane: &ctmux_proto::PaneGeometry| {
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
