use crate::{
  Result,
  actions::{Action, Direction},
  copy::{Action as CopyAction, BottomBehavior, CopyMode},
  divider::{Divider, Drag},
  input::{self, Prefix},
  keys::{Dispatch, KeyState},
  maintenance::{Maintenance, Reconnect, Snapshot},
  pane::{Pane, ReconnectLeases, identity},
  prompt::{self, Command as PromptCommand, Event as PromptEvent, Prompt},
  render::{Frame, Renderer, pane_at, pane_position, viewport_offset},
  transport::{LocalTransport, Transport},
};
use crossterm::event::{
  Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ctmux_client::{
  session::{CreateSessionRequest, SessionClient, SessionId},
  view::{SplitTerminalRequest, TerminalId, ViewClient},
};
#[cfg(all(test, unix))]
use ctmux_proto::{ClientMessage, ServerMessage};
use ctmux_proto::{LeaseKind, SessionInfo, SessionStatus, SplitAxis, TerminalSize, ViewInfo};
use std::{
  collections::{BTreeMap, BTreeSet},
  io,
  path::PathBuf,
  time::Duration,
};
use tokio::{
  sync::mpsc,
  time::{Instant, timeout},
};

enum Overlay {
  None,
  Help,
  Sessions(Option<String>),
  Kill(String),
  Archives(usize),
  ArchiveTerminals(Box<ctmux_client::archive::SessionArchive>, usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NoticeKind {
  Action,
  Connection,
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
  keys: KeyState,
  sessions: Vec<SessionInfo>,
  archives: Vec<ctmux_client::archive::SessionArchive>,
  ended: Option<String>,
  selected_id: String,
  archived_panes: Vec<ctmux_client::archive::ArchivedPane>,
  view: Option<ViewInfo>,
  panes: BTreeMap<String, Pane>,
  focused: String,
  navigation: navigation::Navigation,
  pane_labels: Option<navigation::PaneLabels>,
  size: (u16, u16),
  overlay: Overlay,
  copies: BTreeMap<String, CopyMode>,
  archive_copy: Option<CopyMode>,
  mouse_capture: Option<MouseCapture>,
  divider_drag: Option<Drag>,
  pane_move: Option<moves::PendingMove>,
  migrated_panes: BTreeSet<String>,
  prompt: Prompt,
  copy_buffer: Option<String>,
  message: String,
  message_until: Instant,
  notice_kind: NoticeKind,
  renderer: Renderer,
  layout_owner: Option<String>,
  maintenance: Maintenance<'a>,
  runtime: bool,
  resize_sequence: u64,
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
      keys: KeyState::Root,
      sessions: Vec::new(),
      archives: Vec::new(),
      ended: None,
      selected_id: String::new(),
      archived_panes: Vec::new(),
      view: None,
      panes: BTreeMap::new(),
      focused: String::new(),
      navigation: navigation::Navigation::default(),
      pane_labels: None,
      size: crossterm::terminal::size().unwrap_or((80, 24)),
      overlay: Overlay::None,
      copies: BTreeMap::new(),
      archive_copy: None,
      mouse_capture: None,
      divider_drag: None,
      pane_move: None,
      migrated_panes: BTreeSet::new(),
      prompt: Prompt::default(),
      copy_buffer: None,
      message: String::new(),
      message_until: Instant::now(),
      notice_kind: NoticeKind::Action,
      renderer: Renderer::default(),
      layout_owner: None,
      maintenance: Maintenance::default(),
      runtime: false,
      resize_sequence: 0,
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

  #[cfg(all(test, unix))]
  async fn request(&self, message: ClientMessage) -> Result<ServerMessage> {
    timeout(Duration::from_secs(5), async {
      let local = LocalTransport(self.socket.clone());
      let stream = self.transport.unwrap_or(&local).connect().await?;
      Ok(ctmux_client::request(stream, &identity(), message).await?)
    })
    .await?
  }

  async fn session_client(&self) -> Result<SessionClient<crate::transport::Stream>> {
    let local = LocalTransport(self.socket.clone());
    let stream = self.transport.unwrap_or(&local).connect().await?;
    Ok(SessionClient::new(stream, identity()))
  }

  async fn view_client(&self) -> Result<ViewClient<crate::transport::Stream>> {
    let local = LocalTransport(self.socket.clone());
    let stream = self.transport.unwrap_or(&local).connect().await?;
    Ok(ViewClient::new(stream, identity()))
  }

  async fn get_view(&self, session: &str) -> Result<ViewInfo> {
    timeout(Duration::from_secs(5), async {
      Ok::<_, crate::Error>(
        self
          .view_client()
          .await?
          .get(SessionId(session.into()))
          .await?,
      )
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
    self.keys = KeyState::Root;
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
    let mut sessions = timeout(Duration::from_secs(5), async {
      Ok::<_, crate::Error>(self.session_client().await?.list().await?)
    })
    .await??;
    sessions.retain(|session| session.status == SessionStatus::Running);
    sessions.sort_by_key(|session| (session.created_at_ms, session.session_id.clone()));
    self.sessions = sessions;
    self.retain_navigation();
    Ok(())
  }

  async fn create(&mut self) -> Result<()> {
    self.create_named(None).await
  }

  async fn create_named(&mut self, name: Option<String>) -> Result<()> {
    let session = timeout(Duration::from_secs(5), async {
      Ok::<_, crate::Error>(
        self
          .session_client()
          .await?
          .create(CreateSessionRequest {
            name,
            command: Vec::new(),
            cwd: std::env::current_dir()
              .ok()
              .map(|path| path.to_string_lossy().into_owned()),
            terminal_size: self.canvas_size(),
          })
          .await?,
      )
    })
    .await??;
    self.list().await?;
    self.select(&session.session_id).await
  }

  async fn find_view(&self, selector: &str) -> Result<ViewInfo> {
    let request = self.get_view(selector).await;
    match request {
      Ok(view) => Ok(view),
      Err(error) if session_not_found(&error) => {
        // A terminal ID is also a valid attachment target. GetView itself takes
        // a root selector, so resolve membership without taking any leases.
        for root in &self.sessions {
          match self.get_view(&root.session_id).await {
            Ok(view)
              if view
                .terminals
                .iter()
                .any(|pane| pane.terminal_id == selector) =>
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
    // Resolve the target before dropping live attachments or frozen copies.
    // A remembered session can disappear while another session is active.
    let view = match self.find_view(session).await {
      Ok(view) => view,
      Err(error) if session_not_found(&error) && self.view.is_none() => {
        session.clone_into(&mut self.selected_id);
        self.ended = Some("Session no longer exists — press any key to exit".into());
        return Ok(());
      }
      Err(error) => return Err(error),
    };
    if self
      .view
      .as_ref()
      .is_some_and(|current| current.session_id == view.session_id)
    {
      // Explicitly selecting the current session is also a retry after the
      // user has resolved an authentication/configuration failure.
      self.maintenance.cancel();
      self.adopt_view(view).await?;
      if session != self.selected_id
        && self
          .view
          .as_ref()
          .is_some_and(|view| view.panes.iter().any(|pane| pane.terminal_id == session))
      {
        if session != self.focused {
          self.unzoom().await?;
        }
        self.focus_pane(session.into());
      }
      self.overlay = Overlay::None;
      return Ok(());
    }
    self.remember_session_switch(&view.session_id);
    self.cancel_pane_move();
    self.migrated_panes.clear();
    self.maintenance.cancel();
    self.release_mouse().await?;
    self.copies.clear();
    self.archive_copy = None;
    self.mouse_capture = None;
    self.pane_labels = None;
    self.ended = None;
    self.archived_panes.clear();
    self.selected_id.clone_from(&view.session_id);
    self.detach().await;
    let remembered = self.remembered_focus(&view.session_id);
    self.focused = view
      .terminals
      .iter()
      .find(|pane| session != view.session_id && pane.terminal_id == session)
      .or_else(|| {
        view
          .terminals
          .iter()
          .find(|pane| Some(&pane.terminal_id) == remembered)
      })
      .or_else(|| view.terminals.first())
      .map_or_else(String::new, |pane| pane.terminal_id.clone());
    self.view = Some(view);
    self.overlay = Overlay::None;
    self.keys = KeyState::Root;
    self.reconcile().await?;
    // The first attachment may have resized the shared canvas.
    self.refresh_view().await
  }

  async fn refresh_view(&mut self) -> Result<()> {
    let Some(view) = &self.view else {
      return Ok(());
    };
    let view = match self.get_view(&view.session_id).await {
      Ok(view) => view,
      Err(error) if session_not_found(&error) => {
        self.ended = Some("Session no longer exists — press any key to exit".into());
        return Ok(());
      }
      Err(error) => return Err(error),
    };
    self.adopt_view(view).await
  }

  async fn reconcile(&mut self) -> Result<()> {
    let Some(view) = &self.view else {
      return Ok(());
    };
    let ids: Vec<_> = view
      .terminals
      .iter()
      .map(|pane| pane.terminal_id.clone())
      .collect();
    self.maintenance.retain_panes(&ids);
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
    if let Some(zoomed) = &view.zoomed_terminal_id {
      self.focused.clone_from(zoomed);
    }
    self.remember_focus();
    if self.runtime {
      self.schedule_reconnects(&ids);
      return Ok(());
    }
    for id in &ids {
      if self
        .panes
        .get(id)
        .is_some_and(|pane| pane.connected || pane.ended.is_some())
      {
        continue;
      }
      let request = self.reconnect_request(id, &ids);
      if let Some(old) = self.panes.get_mut(id) {
        old.close().await;
      }
      let local = LocalTransport(self.socket.clone());
      let opened = timeout(
        Duration::from_secs(5),
        Pane::open(
          self.transport.unwrap_or(&local),
          id,
          self.canvas_size(),
          request.leases,
          request.token,
        ),
      )
      .await?;
      match opened {
        Ok(opened) => {
          self.cancel_replaced_move(id);
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
    self.pane_labels = None;
    self.cancel_pane_move();
    self.migrated_panes.clear();
    self.maintenance.cancel();
    let panes = std::mem::take(&mut self.panes);
    for (_, mut pane) in panes {
      pane.close().await;
    }
  }

  pub async fn run(&mut self, mut events: mpsc::Receiver<io::Result<Event>>) -> Result<()> {
    self.runtime = true;
    // Keep event-loop state out of the futures owned by CLI callers.
    let result = Box::pin(self.run_loop(&mut events)).await;
    self.runtime = false;
    self.maintenance.cancel();
    result
  }

  async fn run_loop(&mut self, events: &mut mpsc::Receiver<io::Result<Event>>) -> Result<()> {
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
          self.poll_maintenance().await;
          if !self.archive_only && refreshed.elapsed() >= Duration::from_secs(2) {
            self.schedule_refresh();
            refreshed = Instant::now();
          }
          self.draw()?;
        }
      }
    }
  }

  fn schedule_refresh(&mut self) {
    if self.ended.is_some() || self.archive_only {
      return;
    }
    if !self.panes.is_empty() && self.panes.values().all(|pane| pane.ended.is_some()) {
      self.mark_session_ended();
      return;
    }
    self.maintenance.refresh(
      self.transport,
      self.socket.clone(),
      self.view.as_ref().map(|view| view.session_id.clone()),
    );
  }

  fn schedule_reconnects(&mut self, ids: &[String]) {
    for id in ids {
      let old = self.panes.get(id);
      if old.is_some_and(|pane| pane.connected || pane.ended.is_some()) {
        continue;
      }
      self.maintenance.reconnect(
        self.transport,
        self.socket.clone(),
        self.reconnect_request(id, ids),
      );
    }
  }

  fn reconnect_request(&self, id: &str, ids: &[String]) -> Reconnect {
    let old = self.panes.get(id);
    Reconnect {
      terminal_id: id.into(),
      size: self.canvas_size(),
      leases: old.map_or_else(
        || {
          ReconnectLeases::new(
            !self.read_only,
            !self.read_only
              && ids.first().is_some_and(|first| first == id)
              && !self.panes.values().any(|pane| pane.reconnect_leases.layout),
          )
        },
        |pane| pane.reconnect_leases,
      ),
      token: old.map(|pane| pane.token.clone()),
    }
  }

  async fn poll_maintenance(&mut self) {
    let ready = self.maintenance.poll().await;
    if let Some(snapshot) = ready.snapshot {
      match snapshot {
        Ok(snapshot) => {
          if let Err(error) = self.adopt_snapshot(snapshot).await {
            self.notice(error.to_string());
          } else {
            self.clear_connection_notice();
          }
        }
        Err(error) => self.connection_notice(&error.to_string()),
      }
    }
    for (id, opened) in ready.panes {
      match opened {
        Ok(pane)
          if self.ended.is_none()
            && self
              .panes
              .get(&id)
              .is_none_or(|current| current.ended.is_none())
            && self.view.as_ref().is_some_and(|view| {
              view
                .terminals
                .iter()
                .any(|terminal| terminal.terminal_id == id)
            }) =>
        {
          if self
            .divider_drag
            .as_ref()
            .is_some_and(|drag| drag.owner == id)
          {
            self.divider_drag = None;
          }
          self.cancel_replaced_move(&id);
          self.panes.insert(id, pane);
          self.clear_connection_notice();
        }
        Ok(_) => {}
        Err(error) if session_not_found(&error) => {
          if let Some(pane) = self.panes.get_mut(&id) {
            pane.ended = Some("Terminal no longer exists".into());
          }
        }
        Err(error) => self.connection_notice(&error.to_string()),
      }
    }
    if let Some(view) = &self.view {
      let ids = view
        .terminals
        .iter()
        .map(|pane| pane.terminal_id.clone())
        .collect::<Vec<_>>();
      self.schedule_reconnects(&ids);
    }
  }

  async fn adopt_snapshot(&mut self, snapshot: Snapshot) -> Result<()> {
    self.sessions = snapshot.sessions;
    self.retain_navigation();
    if let Some(view) = snapshot.view {
      // A sibling can publish a newer topology/zoom while GetView is pending.
      self.adopt_view(view).await
    } else {
      if self.panes.values().any(|pane| pane.ended.is_some()) {
        return Ok(());
      }
      if self.view.is_some() {
        self.mark_session_ended();
      }
      Ok(())
    }
  }

  fn mark_session_ended(&mut self) {
    let outcome = self
      .panes
      .get(&self.focused)
      .and_then(|pane| pane.ended.as_deref())
      .unwrap_or("Session no longer exists");
    self.ended = Some(format!("{outcome} — press any key to exit"));
  }

  async fn drain(&mut self) {
    let mut notices = Vec::new();
    let mut view_update: Option<ViewInfo> = None;
    let mut resize_results = Vec::new();
    let mut view_updates = Vec::new();
    let mut migrations = Vec::new();
    let mut drag_disconnected = false;
    for (id, pane) in &mut self.panes {
      // A closed transport may still have a final SessionEnded event queued.
      match pane.drain().await {
        Ok(Some(message)) => notices.push((NoticeKind::Action, message)),
        Ok(None) => {}
        Err(error) => {
          pane.connected = false;
          notices.push((NoticeKind::Connection, error.to_string()));
        }
      }
      if let Some(view) = pane.view_update.take() {
        if let Some(pending) = &mut self.pane_move {
          pending.observe_view(&view);
        }
        view_updates.push((id.clone(), view));
      }
      resize_results.extend(pane.resize_results.drain(..));
      if !pane.connected
        && self
          .divider_drag
          .as_ref()
          .is_some_and(|drag| &drag.owner == id)
      {
        drag_disconnected = true;
      }
    }
    if drag_disconnected {
      self.divider_drag = None;
    }
    // Only correlated replies advance the drag's revision. A view broadcast
    // can repaint the confirmed geometry before that reply reaches this loop.
    for (request_id, outcome) in resize_results {
      if let Some(drag) = &mut self.divider_drag
        && let Err(error) = drag.acknowledge(&request_id, &outcome)
      {
        self.divider_drag = None;
        notices.push((NoticeKind::Action, error));
      }
    }
    // Settle an explicit identity handoff before classifying broadcasts. A
    // later source/promoted snapshot must not be downgraded by the move reply.
    if let Err(error) = self.poll_pane_move().await {
      notices.push((NoticeKind::Action, error.to_string()));
    }
    for (id, view) in view_updates {
      if self.view.as_ref().is_some_and(|current| {
        current.session_id == view.session_id
          && current.view_id == view.view_id
          && view.revision >= current.revision
      }) && view_update
        .as_ref()
        .is_none_or(|pending| view.revision >= pending.revision)
      {
        view_update = Some(view);
      } else {
        migrations.push((id, view));
      }
    }
    self.observe_migrations(migrations);
    if let Some(view) = view_update
      && let Err(error) = self.adopt_view(view).await
    {
      notices.push((NoticeKind::Action, error.to_string()));
    }
    let owner = self
      .panes
      .iter()
      .find(|(_, pane)| pane.connected && pane.control.state().leases().layout.owned_by_client)
      .map(|(id, _)| id.clone());
    if owner != self.layout_owner {
      self.layout_owner = owner;
      if let Err(error) = self.resize().await {
        notices.push((NoticeKind::Action, error.to_string()));
      }
    }
    if let Err(error) = self.flush_divider_drag().await {
      notices.push((NoticeKind::Action, error.to_string()));
    }
    for (kind, message) in notices {
      match kind {
        NoticeKind::Connection => self.connection_notice(&message),
        NoticeKind::Action => self.notice(message),
      }
    }
  }

  async fn adopt_view(&mut self, view: ViewInfo) -> Result<()> {
    if let Some(pending) = &mut self.pane_move {
      pending.observe_view(&view);
    }
    let Some(current) = &self.view else {
      return Ok(());
    };
    if current.session_id != view.session_id
      || current.view_id != view.view_id
      || view.revision < current.revision
    {
      return Ok(());
    }
    if current.zoomed_terminal_id != view.zoomed_terminal_id {
      self.release_mouse().await?;
    }
    if self
      .divider_drag
      .as_ref()
      .is_some_and(|drag| !drag.accepts_view(&view))
    {
      self.divider_drag = None;
    }
    self.reconcile_migrations(&view).await?;
    let missing = self.panes.keys().any(|id| {
      !view
        .terminals
        .iter()
        .any(|terminal| terminal.terminal_id == *id)
    });
    if missing {
      // Topology metadata can arrive before a sibling's final output/Ended
      // event. Both broadcasts and command acknowledgements must preserve its
      // controller, copy state, and final screen until dismissal archives it.
      if let Some(current) = &mut self.view {
        current.revision = view.revision;
        current.zoomed_terminal_id = view.zoomed_terminal_id;
      }
    } else {
      self.view = Some(view);
    }
    self.reconcile().await
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
    self.notice_kind = NoticeKind::Action;
  }

  fn connection_notice(&mut self, error: &str) {
    self.notice(format!("Disconnected: {error}; retrying"));
    self.notice_kind = NoticeKind::Connection;
  }

  fn clear_connection_notice(&mut self) {
    if self.notice_kind == NoticeKind::Connection
      && !self.panes.is_empty()
      && self
        .panes
        .values()
        .all(|pane| pane.connected || pane.ended.is_some())
    {
      self.message_until = Instant::now();
      self.notice_kind = NoticeKind::Action;
    }
  }

  async fn event(&mut self, event: Event) -> Result<bool> {
    self.expire_pane_labels();
    match event {
      Event::Mouse(mouse) => {
        if self.pane_labels.is_some() {
          if matches!(
            mouse.kind,
            MouseEventKind::Down(_)
              | MouseEventKind::ScrollUp
              | MouseEventKind::ScrollDown
              | MouseEventKind::ScrollLeft
              | MouseEventKind::ScrollRight
          ) {
            self.pane_labels = None;
          }
          return Ok(false);
        }
        if matches!(
          mouse.kind,
          MouseEventKind::Down(_)
            | MouseEventKind::ScrollUp
            | MouseEventKind::ScrollDown
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight
        ) {
          self.keys.cancel_repeat();
        }
        self.mouse(mouse).await?;
      }
      Event::Key(key) => return self.key(key).await,
      Event::Resize(columns, rows) => {
        self.divider_drag = None;
        self.size = (columns, rows);
        self.resize().await?;
      }
      Event::Paste(text) => {
        if self.pane_labels.take().is_some() {
          return Ok(false);
        }
        self.divider_drag = None;
        self.keys.cancel_repeat();
        if self.prompt.is_active() {
          self.prompt.paste(&text);
        } else if self.active_copy().is_none()
          && matches!(self.overlay, Overlay::None)
          && !self.keys.is_prefix()
        {
          self.paste(text).await?;
        }
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
      && let Some(rect) = self.view.as_ref().and_then(|view| {
        view
          .visible_panes()
          .into_iter()
          .find(|rect| &rect.terminal_id == id)
      })
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
    self.divider_drag = None;
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
    if self.prompt.is_active() {
      return Ok(());
    }
    if self.drag_mouse(mouse).await? {
      return Ok(());
    }
    if matches!(mouse.kind, MouseEventKind::Drag(_) | MouseEventKind::Up(_)) {
      if let Some(capture) = self.mouse_capture.take() {
        return self.captured_mouse(mouse, capture).await;
      }
      // A modal control may consume the press. Its drag/release must never
      // reach an application that did not receive the corresponding press.
      return Ok(());
    }
    if self.keys.is_prefix() {
      return Ok(());
    }
    if mouse.column >= self.size.0 || mouse.row >= self.size.1.saturating_sub(1) {
      return Ok(());
    }
    if self.begin_divider_drag(mouse) {
      return Ok(());
    }
    let Some(target) = self.mouse_target(mouse) else {
      return Ok(());
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
        self.focus_pane(id.clone());
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

  fn mouse_target(&self, mouse: MouseEvent) -> Option<CopyTarget> {
    if self.archive_copy.is_some() {
      return Some(CopyTarget::Archive);
    }
    if !matches!(self.overlay, Overlay::None) {
      return None;
    }
    let id = pane_at(
      self.view.as_ref()?,
      &self.panes,
      &self.copies,
      &self.focused,
      self.size,
      (mouse.column, mouse.row),
    )?;
    Some(CopyTarget::Pane(id.to_owned()))
  }

  async fn drag_mouse(&mut self, mouse: MouseEvent) -> Result<bool> {
    let Some(drag) = &mut self.divider_drag else {
      return Ok(false);
    };
    if !drag.released()
      && matches!(
        mouse.kind,
        MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
      )
    {
      drag.move_to(
        (mouse.column, mouse.row),
        mouse.kind == MouseEventKind::Up(MouseButton::Left),
      );
      self.flush_divider_drag().await?;
    }
    // Keep the final mouseup local while its target waits for confirmation.
    Ok(true)
  }

  fn begin_divider_drag(&mut self, mouse: MouseEvent) -> bool {
    if mouse.kind == MouseEventKind::Down(MouseButton::Left)
      && self.mouse_capture.is_none()
      && matches!(self.overlay, Overlay::None)
      && self.archive_copy.is_none()
      && let Some(view) = &self.view
    {
      let offset = viewport_offset(
        view,
        self.panes.get(&self.focused),
        self.copies.get(&self.focused),
        &self.focused,
        self.size.0,
        self.size.1.saturating_sub(1),
      );
      let position = (
        mouse.column.saturating_add(offset.0),
        mouse.row.saturating_add(offset.1),
      );
      if let Some(divider) = Divider::hit(view, position) {
        if self.read_only {
          self.notice("This attachment is read only".into());
          return true;
        }
        let Some((id, owner)) = self.panes.iter().find(|(_, pane)| {
          pane.connected
            && pane.ended.is_none()
            && pane.control.state().leases().layout.owned_by_client
        }) else {
          self.notice(format!("{} R to take resize control", self.prefix.label));
          return true;
        };
        if !owner.control.supports_divider_resize() {
          self.notice("Divider dragging requires ctmux 1.1.17".into());
          return true;
        }
        self.mouse_capture = None;
        self.divider_drag = Some(Drag::new(view, divider, offset, id.clone()));
        return true;
      }
    }
    false
  }

  async fn flush_divider_drag(&mut self) -> Result<()> {
    let Some(drag) = &self.divider_drag else {
      return Ok(());
    };
    if !self.panes.get(&drag.owner).is_some_and(|pane| {
      pane.connected && pane.ended.is_none() && pane.control.state().leases().layout.owned_by_client
    }) || self
      .view
      .as_ref()
      .is_none_or(|view| !drag.accepts_view(view))
    {
      self.divider_drag = None;
      return Ok(());
    }
    self.resize_sequence = self.resize_sequence.wrapping_add(1);
    let request_id = format!("tui-divider-resize-{}", self.resize_sequence);
    let drag = self.divider_drag.as_mut().expect("checked drag");
    let owner = drag.owner.clone();
    if let Some(divider) = drag.next(request_id.clone())
      && let Err(error) = self
        .panes
        .get_mut(&owner)
        .expect("connected resize owner")
        .resize_divider(divider, request_id)
        .await
    {
      self.divider_drag = None;
      return Err(error);
    }
    if self.divider_drag.as_ref().is_some_and(Drag::finished) {
      self.divider_drag = None;
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
    if key.kind == KeyEventKind::Release {
      return Ok(false);
    }
    if self.prompt.is_active() {
      return self.prompt_key(key).await;
    }
    if self.pane_label_key(key).await? {
      return Ok(false);
    }
    if self.divider_drag.take().is_some() && key.code == KeyCode::Esc {
      return Ok(false);
    }
    if self.archive_copy.is_some() {
      self.keys.cancel_repeat();
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
      match self.keys.resolve(key, &self.prefix, Instant::now()) {
        Dispatch::Ignore | Dispatch::Pending => return Ok(false),
        Dispatch::Action(binding) => return self.execute(binding.action).await,
        Dispatch::SendPrefix => {
          self.send_key(key).await?;
          return Ok(false);
        }
        Dispatch::Unknown => {
          self.notice(format!("{} ? for commands", self.prefix.label));
          return Ok(false);
        }
        Dispatch::Forward => {}
      }
      if key.code == KeyCode::PageUp
        && key.modifiers == KeyModifiers::SHIFT
        && self.active_copy().is_none()
      {
        return self.execute(Action::History { page_back: true }).await;
      }
      let target = CopyTarget::Pane(self.focused.clone());
      let (_, height) = self.copy_size(&target);
      if let Some(mode) = self.copies.get_mut(&self.focused) {
        let action = mode.key(key, height);
        self.copy_action(&target, action)?;
        return Ok(false);
      }
    } else {
      self.keys.cancel_repeat();
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
      self.remember_focus();
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

  async fn prompt_key(&mut self, key: KeyEvent) -> Result<bool> {
    let PromptEvent::Submit(line) = self.prompt.key(key) else {
      return Ok(false);
    };
    if self.notice_kind == NoticeKind::Action {
      self.message_until = Instant::now();
    }
    let command = match prompt::parse(&line) {
      Ok(command) => command,
      Err(error) => {
        self.notice(error);
        return Ok(false);
      }
    };
    match self.execute_command(command).await {
      Ok(detach) => Ok(detach),
      Err(error) => {
        self.notice(error.to_string());
        Ok(false)
      }
    }
  }

  async fn execute_command(&mut self, command: PromptCommand) -> Result<bool> {
    match command {
      PromptCommand::Action(action) => return self.execute(action).await,
      PromptCommand::SelectPane(number) => self.select_pane_number(number).await?,
      PromptCommand::NewSession(name) if !self.read_only => self.create_named(name).await?,
      PromptCommand::BreakPane { name, detached } if !self.read_only => {
        self.break_pane(name, detached).await?;
      }
      PromptCommand::SwitchSession(target) => {
        // Validate before selection clears the current pane's frozen copy state.
        self.list().await?;
        let session = self
          .sessions
          .iter()
          .find(|session| session.session_id == target || session.name == target)
          .ok_or_else(|| format!("Session not found: {target}"))?
          .session_id
          .clone();
        self.select(&session).await?;
      }
      PromptCommand::Lease { kind, requested } if !self.read_only => {
        self.change_lease(kind, Some(requested)).await?;
      }
      PromptCommand::NewSession(_)
      | PromptCommand::BreakPane { .. }
      | PromptCommand::Lease { .. } => {
        self.notice("This attachment is read only".into());
      }
    }
    Ok(false)
  }

  async fn send_key(&self, key: KeyEvent) -> Result<()> {
    if let Some(pane) = self.panes.get(&self.focused) {
      let data = input::encode(
        key,
        pane.model.vt.cursor_key_app_mode(),
        pane.model.modify_other_keys(),
      );
      if !data.is_empty() {
        pane.control.input(data).await?;
      }
    }
    Ok(())
  }

  async fn execute(&mut self, action: Action) -> Result<bool> {
    match action {
      Action::CommandPrompt => {
        self.release_mouse().await?;
        self.keys = KeyState::Root;
        self.prompt.open();
      }
      Action::Archives => {
        self.archives = self.local_archives()?;
        self.overlay = Overlay::Archives(0);
      }
      Action::History { page_back } => {
        self.release_mouse().await?;
        let id = self.focused.clone();
        if !self.copies.contains_key(&id)
          && let Some(mode) = self.new_copy(&id)
        {
          self.copies.insert(id.clone(), mode);
        }
        if page_back {
          let (width, height) = self.copy_size(&CopyTarget::Pane(id.clone()));
          if let Some(mode) = self.copies.get_mut(&id) {
            mode.fit(width, height);
            mode.scroll(true, height, height);
          }
        }
      }
      Action::Paste if !self.read_only => {
        if let Some(text) = self.copy_buffer.clone() {
          // Reuse the same lease checks and bracketed-paste encoding as a host paste.
          self.paste(text).await?;
        } else {
          self.notice("Copy buffer is empty".into());
        }
      }
      Action::Detach => return Ok(true),
      Action::Refresh => self.renderer.invalidate(),
      Action::NextPane => {
        self.unzoom().await?;
        self.next_pane();
      }
      Action::LastPane => self.last_pane().await?,
      Action::LastSession => self.last_session().await?,
      Action::DisplayPanes => self.display_panes().await?,
      Action::ToggleZoom => {
        let target = self.view.as_ref().and_then(|view| {
          view
            .zoomed_terminal_id
            .is_none()
            .then(|| self.focused.clone())
        });
        self.set_zoom(target).await?;
      }
      Action::ResizePane { direction, amount } if !self.read_only => {
        self.release_mouse().await?;
        self.resize_sequence = self.resize_sequence.wrapping_add(1);
        let request_id = format!("tui-pane-resize-{}", self.resize_sequence);
        let owner = self
          .panes
          .values_mut()
          .find(|pane| pane.connected && pane.control.state().leases().layout.owned_by_client)
          .ok_or("Resize lease required to resize panes")?;
        // The daemon moves the divider and clears zoom in one mutation. The
        // regular event drain adopts its view; input and rendering keep running.
        owner
          .resize_pane(self.focused.clone(), direction, amount, request_id)
          .await?;
      }
      Action::Help => self.overlay = Overlay::Help,
      Action::Sessions => self.open_sessions(),
      Action::NextSession => self.next_session(1).await?,
      Action::PreviousSession => self.next_session(-1).await?,
      Action::CreateSession if !self.read_only => self.create().await?,
      Action::Split(axis) if !self.read_only => self.split(axis).await?,
      Action::SwapPane { previous, stay } if !self.read_only => {
        self.swap_pane(previous, stay).await?;
      }
      Action::BreakPane if !self.read_only => self.break_pane(None, false).await?,
      Action::KillPane if !self.read_only => self.overlay = Overlay::Kill(self.focused.clone()),
      Action::ToggleLease(lease) if !self.read_only => self.toggle_lease(lease).await?,
      Action::Focus(direction) => {
        self.unzoom().await?;
        self.focus(direction);
      }
      Action::Cancel => {}
      Action::Paste
      | Action::CreateSession
      | Action::Split(_)
      | Action::SwapPane { .. }
      | Action::BreakPane
      | Action::ResizePane { .. }
      | Action::KillPane
      | Action::ToggleLease(_) => {
        self.notice("This attachment is read only".into());
      }
    }
    Ok(false)
  }

  async fn set_zoom(&mut self, target: Option<String>) -> Result<()> {
    self.release_mouse().await?;
    let owner = self
      .panes
      .iter()
      .find(|(_, pane)| pane.connected && pane.control.state().leases().layout.owned_by_client)
      .map(|(id, _)| id.clone())
      .ok_or("Resize lease required to change pane zoom")?;
    let current = self.view.as_ref().ok_or("No session to zoom")?;
    let session_id = current.session_id.clone();
    let view_id = current.view_id.clone();
    let revision = current.revision;
    let changes_zoom = current.zoomed_terminal_id != target;
    self.panes[&owner]
      .control
      .set_view_zoom(target.clone())
      .await?;
    // Commands enter an ordered local queue. Only the daemon's snapshot confirms
    // the mutation, so do not change focus or geometry optimistically.
    let view = timeout(Duration::from_secs(5), async {
      loop {
        let pane = self.panes.get_mut(&owner).ok_or("Zoom attachment closed")?;
        if let Some(error) = pane.drain().await? {
          return Err::<ViewInfo, crate::Error>(error.into());
        }
        if let Some(view) = pane.view_update.take()
          && view.session_id == session_id
          && view.view_id == view_id
          && view.revision >= revision
          && (!changes_zoom || view.revision > revision)
          && view.zoomed_terminal_id == target
        {
          return Ok(view);
        }
        if !pane.connected {
          return Err("Disconnected while changing pane zoom".into());
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
      }
    })
    .await??;
    self.adopt_view(view).await
  }

  async fn unzoom(&mut self) -> Result<()> {
    if self
      .view
      .as_ref()
      .is_some_and(|view| view.zoomed_terminal_id.is_some())
    {
      self.set_zoom(None).await?;
    }
    Ok(())
  }

  async fn toggle_lease(&mut self, lease: LeaseKind) -> Result<()> {
    self.change_lease(lease, None).await
  }

  async fn change_lease(&mut self, lease: LeaseKind, requested: Option<bool>) -> Result<()> {
    // Layout belongs to the view, so release its owner regardless of focus.
    let owner = self
      .panes
      .iter()
      .find(|(_, pane)| {
        let leases = pane.control.state().leases();
        match lease {
          LeaseKind::Layout => {
            if pane.connected {
              leases.layout.owned_by_client
            } else {
              pane
                .reconnect_leases
                .intended_ownership(lease, leases.layout.owned_by_client)
            }
          }
          LeaseKind::Input => false,
        }
      })
      .map(|(id, _)| id.clone());
    let id = if lease == LeaseKind::Layout {
      owner.as_ref().unwrap_or(&self.focused)
    } else {
      &self.focused
    }
    .clone();
    if let Some(pane) = self.panes.get(&id) {
      let leases = pane.control.state().leases();
      let observed = match lease {
        LeaseKind::Input => leases.input.owned_by_client,
        LeaseKind::Layout => leases.layout.owned_by_client,
      };
      let held_by_client = if pane.connected {
        observed
      } else {
        pane.reconnect_leases.intended_ownership(lease, observed)
      };
      let requested = requested.unwrap_or(!held_by_client);
      let control = pane.control.clone();
      let connected = pane.connected;
      // A release is also the user's reconnect preference. Keep that intent
      // even when a disconnected command queue can no longer accept it.
      if lease == LeaseKind::Layout {
        for pane in self.panes.values_mut() {
          pane.request_lease(lease, false);
        }
      }
      if let Some(pane) = self.panes.get_mut(&id) {
        pane.request_lease(lease, requested);
      }
      self.maintenance.cancel_reconnects();
      if !connected || held_by_client == requested {
        // Preserve reconnect intent even when no wire change is needed now.
        return Ok(());
      }
      if requested {
        control.request_lease(lease).await?;
      } else {
        control.release_lease(lease).await?;
      }
    }
    Ok(())
  }

  async fn split(&mut self, axis: SplitAxis) -> Result<()> {
    if self.focused.is_empty() {
      return Ok(());
    }
    let previous: Vec<_> = self.panes.keys().cloned().collect();
    let view = timeout(Duration::from_secs(5), async {
      Ok::<_, crate::Error>(
        self
          .view_client()
          .await?
          .split(SplitTerminalRequest {
            terminal_id: TerminalId(self.focused.clone()),
            axis,
            command: Vec::new(),
            cwd: None,
            terminal_size: self.canvas_size(),
          })
          .await?,
      )
    })
    .await??;
    let target = view
      .panes
      .iter()
      .find(|pane| !previous.contains(&pane.terminal_id))
      .map(|pane| pane.terminal_id.clone());
    self.view = Some(view);
    if let Some(target) = target {
      self.focus_pane(target);
    }
    self.reconcile().await?;
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
    self.focus_pane(
      view.panes[(index + 1) % view.panes.len()]
        .terminal_id
        .clone(),
    );
  }

  fn focus(&mut self, direction: Direction) {
    let Some(view) = &self.view else {
      return;
    };
    if let Some(id) = adjacent(&view.panes, &self.focused, direction) {
      self.focus_pane(id);
    }
  }

  async fn overlay_key(&mut self, key: KeyEvent) -> Result<()> {
    if let Overlay::Sessions(selected) = &self.overlay {
      let selected = selected.clone();
      match key.code {
        KeyCode::Up | KeyCode::Down => {
          let index = self.picker_index(selected.as_deref());
          let index = if key.code == KeyCode::Up {
            index.map_or_else(
              || self.sessions.len().saturating_sub(1),
              |index| index.saturating_sub(1),
            )
          } else {
            index.map_or(0, |index| {
              (index + 1).min(self.sessions.len().saturating_sub(1))
            })
          };
          self.overlay = Overlay::Sessions(
            self
              .sessions
              .get(index)
              .map(|session| session.session_id.clone()),
          );
        }
        KeyCode::Enter => {
          if let Some(selected) = selected.filter(|id| {
            self
              .sessions
              .iter()
              .any(|session| &session.session_id == id)
          }) {
            self.select(&selected).await?;
          } else {
            self.notice("Selected session no longer exists; choose another session".into());
          }
        }
        KeyCode::Esc => self.overlay = Overlay::None,
        _ => {}
      }
      return Ok(());
    }
    match &mut self.overlay {
      Overlay::Sessions(_) => unreachable!("session picker handled above"),
      Overlay::Kill(id) => {
        let id = id.clone();
        self.overlay = Overlay::None;
        if key.code == KeyCode::Char('y') && !id.is_empty() {
          timeout(Duration::from_secs(5), async {
            Ok::<_, crate::Error>(
              self
                .view_client()
                .await?
                .terminate_terminal(TerminalId(id))
                .await?,
            )
          })
          .await??;
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
    self.expire_pane_labels();
    let mut frame = Frame::new(self.size.0, self.size.1);
    if let Some(mode) = &mut self.archive_copy {
      mode.fit(
        usize::from(self.size.0),
        usize::from(self.size.1.saturating_sub(1)),
      );
      frame.copy_mode(mode);
    } else if let Some(view) = &self.view {
      for rect in view.visible_panes() {
        if let Some(mode) = self.copies.get_mut(&rect.terminal_id) {
          mode.fit(usize::from(rect.columns), usize::from(rect.rows));
        }
      }
      if let Some(drag) = &self.divider_drag {
        frame.canvas_at(
          view,
          &self.panes,
          &self.copies,
          &self.focused,
          drag.viewport_offset(),
        );
      } else {
        frame.canvas(view, &self.panes, &self.copies, &self.focused);
      }
      frame.overlay(&self.overlay_lines());
      if let Some(labels) = &self.pane_labels {
        let offset = viewport_offset(
          view,
          self.panes.get(&self.focused),
          self.copies.get(&self.focused),
          &self.focused,
          self.size.0,
          self.size.1.saturating_sub(1),
        );
        frame.pane_numbers(view, &labels.labels, &self.focused, offset);
      }
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
    if self.prompt.is_active() {
      frame.command_prompt(&self.prompt);
    }
    frame
  }

  fn draw(&mut self) -> Result<()> {
    let frame = self.frame();
    self.renderer.draw(frame)?;
    Ok(())
  }

  fn status(&self) -> String {
    if self.pane_labels.is_some() {
      return format!(
        " {} | Pane numbers: 1–9 selects; any other key cancels",
        self.connection_history_status()
      );
    }
    if let Some(mode) = &self.archive_copy {
      return format!(" archive | {}", mode.status());
    }
    if self.notice_kind == NoticeKind::Action
      && Instant::now() < self.message_until
      && matches!(self.overlay, Overlay::None)
      && !self.keys.is_prefix()
    {
      let copy = if self.copies.contains_key(&self.focused) {
        " | COPY"
      } else {
        ""
      };
      return format!(
        " {}{copy} | {}",
        self.connection_history_status(),
        self.message
      );
    }
    if let Some(mode) = self.copies.get(&self.focused)
      && matches!(self.overlay, Overlay::None)
      && !self.keys.is_prefix()
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
    if self.keys.is_prefix() {
      return format!(
        " {} | PREFIX  % split right  \" split below  arrows focus  c new  s sessions  : commands  d detach  ? help",
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
    let status = self.panes.get(&self.focused).map_or_else(
      || connection.to_owned(),
      |pane| format!("{connection} | {}", pane.history_status()),
    );
    if self
      .view
      .as_ref()
      .is_some_and(|view| view.zoomed_terminal_id.is_some())
    {
      format!("{status} | ZOOM")
    } else {
      status
    }
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
      Overlay::Sessions(selected) => {
        let mut lines = vec!["Sessions — ↑/↓ choose, Enter opens, Esc cancels".into()];
        let index = self.picker_index(selected.as_deref());
        if index.is_none() {
          lines.push("Selected session unavailable — arrows choose another".into());
        }
        let capacity = usize::from(self.size.1.saturating_sub(2)).max(1);
        let start = index.unwrap_or(0).saturating_sub(capacity - 1);
        lines.extend(
          self
            .sessions
            .iter()
            .enumerate()
            .skip(start)
            .take(capacity)
            .map(|(i, session)| {
              format!(
                "{} {}",
                if Some(i) == index { ">" } else { " " },
                session.name
              )
            }),
        );
        lines
      }
      Overlay::Help => vec![
        format!("Commands after {} — any key closes help", self.prefix.label),
        "%: split right    \": split below".into(),
        "Arrows: focus pane    o: next pane    z: zoom    x: terminate (confirm)".into(),
        ";: last pane    q: pane numbers (1–9 selects)".into(),
        "{/}: swap with previous/next pane    !: move pane to a new session".into(),
        "Ctrl/Alt arrows resize panes by 1/5 cells after prefix.".into(),
        "Mouse: drag dividers to resize; Esc cancels remaining movement.".into(),
        "Focus/resize arrows repeat for 500 ms; other commands need a fresh prefix.".into(),
        "c: new session    n/p: next/previous    l/L: last    s/w: session list".into(),
        ": command prompt (pane, session, and ownership commands)".into(),
        "Prompt panes: split-window, select-pane, resize-pane, kill-pane".into(),
        "Prompt navigation: last-pane, select-pane -t NUMBER, display-panes".into(),
        "Prompt moves: swap-pane -U/-D [-d], break-pane [-d] [-n NAME]".into(),
        "Prompt sessions: new-session, switch-client, list-sessions".into(),
        "Prompt ownership: take-input/release-input, take-resize/release-resize".into(),
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
  direction: Direction,
) -> Option<String> {
  let origin = panes.iter().find(|pane| pane.terminal_id == focused)?;
  let horizontal = matches!(direction, Direction::Left | Direction::Right);
  let sign = if matches!(direction, Direction::Right | Direction::Down) {
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

#[path = "moves.rs"]
mod moves;

#[path = "navigation.rs"]
mod navigation;

#[path = "migration.rs"]
mod migration;
