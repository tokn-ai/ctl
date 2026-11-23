use super::{
  SourceOptions, agent_sync, background, view,
  welcome::Viewer,
  workspace_hosts::{Host, Workspace},
};
#[cfg(not(target_os = "macos"))]
use ctream_client::transport::Local;
use ctream_client::transport::{Channel, ProcessReader, SshCommand, Transport};
use eframe::egui;
use std::{
  io,
  process::ChildStdin,
  sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, TryRecvError},
  },
  time::Duration,
};

enum Connection {
  #[cfg(not(target_os = "macos"))]
  Local(Local),
  Remote(SshCommand),
}

impl Transport for Connection {
  type Reader = ProcessReader;
  type Writer = ChildStdin;

  fn open(&self, arguments: &[String]) -> io::Result<Channel<Self::Reader, Self::Writer>> {
    match self {
      #[cfg(not(target_os = "macos"))]
      Self::Local(local) => local.open(arguments),
      Self::Remote(remote) => remote.open(arguments),
    }
  }
}

enum Connected {
  Agent(view::App<Viewer<Connection>>),
  #[cfg(target_os = "macos")]
  Macos(view::App<super::macos_local::Viewer>),
}

impl Connected {
  fn is_viewing(&self) -> bool {
    match self {
      Self::Agent(viewer) => viewer.is_viewing(),
      #[cfg(target_os = "macos")]
      Self::Macos(viewer) => viewer.is_viewing(),
    }
  }

  fn logic(&mut self, ctx: &egui::Context) {
    match self {
      Self::Agent(viewer) => viewer.logic(ctx),
      #[cfg(target_os = "macos")]
      Self::Macos(viewer) => viewer.logic(ctx),
    }
  }

  fn update_ui(&mut self, ui: &mut egui::Ui) {
    match self {
      Self::Agent(viewer) => viewer.update_ui(ui),
      #[cfg(target_os = "macos")]
      Self::Macos(viewer) => viewer.update_ui(ui),
    }
  }
}

struct Connecting {
  host_name: String,
  stage: &'static str,
  progress: Receiver<&'static str>,
  finished: Receiver<io::Result<SshCommand>>,
  cancelled: Arc<AtomicBool>,
}

impl Drop for Connecting {
  fn drop(&mut self) {
    self.cancelled.store(true, Ordering::Release);
  }
}

pub(super) struct Preview {
  workspace: Workspace,
  source: SourceOptions,
  hosts: Vec<Host>,
  selected: Option<String>,
  loading: Option<Receiver<io::Result<Vec<Host>>>>,
  host_error: Option<String>,
  connecting: Option<Connecting>,
  connect_error: Option<io::Error>,
  connected: Option<(String, Connected)>,
}

impl Preview {
  pub(super) fn new(source: SourceOptions, ctx: &egui::Context) -> Self {
    let mut app = Self {
      workspace: Workspace::default(),
      source,
      hosts: Vec::new(),
      selected: None,
      loading: None,
      host_error: None,
      connecting: None,
      connect_error: None,
      connected: None,
    };
    app.refresh(ctx);
    app
  }

  fn refresh(&mut self, ctx: &egui::Context) {
    let workspace = self.workspace.clone();
    self.hosts.clear();
    self.host_error = None;
    self.loading = Some(background(ctx, move || workspace.hosts()));
  }

  fn selected_host(&self) -> Option<&Host> {
    let selected = self.selected.as_deref()?;
    self.hosts.iter().find(|host| host.id == selected)
  }

  fn connect(&mut self, host: Host, ctx: &egui::Context) {
    let host_name = host.name.clone();
    let workspace = self.workspace.clone();
    let cancelled = Arc::new(AtomicBool::new(false));
    let attempt = Arc::clone(&cancelled);
    let (send_progress, progress) = mpsc::channel();
    let repaint = ctx.clone();
    let finished = background(ctx, move || {
      agent_sync::connect(&workspace, &host, &attempt, |stage| {
        let _ = send_progress.send(stage);
        repaint.request_repaint_of(egui::ViewportId::ROOT);
      })
    });
    self.connect_error = None;
    self.connecting = Some(Connecting {
      host_name,
      stage: "Connecting…",
      progress,
      finished,
      cancelled,
    });
  }

  fn open(&mut self, name: String, connection: Connection, ctx: &egui::Context) {
    let diagnostics = match &connection {
      #[cfg(not(target_os = "macos"))]
      Connection::Local(_) => super::stats::Connection::Local,
      Connection::Remote(_) => super::stats::Connection::Ssh(name.clone()),
    };
    self.connected = Some((
      name,
      Connected::Agent(self.source.clone().viewer(connection, diagnostics, ctx)),
    ));
  }

  fn poll(&mut self, ctx: &egui::Context) {
    if let Some(connecting) = &mut self.connecting {
      if let Some(stage) = connecting.progress.try_iter().last() {
        connecting.stage = stage;
      }
      if let Some(result) = completed(&connecting.finished) {
        let done = self.connecting.take().unwrap();
        if done.cancelled.load(Ordering::Acquire) {
          return;
        }
        match result {
          Ok(transport) => self.open(done.host_name.clone(), Connection::Remote(transport), ctx),
          Err(error) => self.connect_error = Some(error),
        }
      }
    }
    if let Some(result) = self.loading.as_ref().and_then(completed) {
      self.loading = None;
      match result {
        Ok(hosts) => {
          self.hosts = hosts;
          let retry = self
            .connect_error
            .take()
            .as_ref()
            .is_some_and(agent_sync::is_missing_agent);
          match self.selected_host() {
            Some(host) if retry && self.connected.is_none() => self.connect(host.clone(), ctx),
            Some(_) => {}
            None => self.selected = None,
          }
        }
        Err(error) => self.host_error = Some(error.to_string()),
      }
    }
  }

  fn pick_host(&mut self, ui: &mut egui::Ui) {
    egui::Panel::top("workspace_heading").show(ui, |ui| {
      ui.horizontal(|ui| {
        ui.heading("Workspace hosts");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
          if ui
            .add_enabled(
              self.loading.is_none() && self.connecting.is_none(),
              egui::Button::new("Refresh"),
            )
            .clicked()
          {
            self.refresh(ui.ctx());
          }
        });
      });
    });
    if self.connecting.is_none() {
      egui::Panel::bottom("workspace_actions").show(ui, |ui| {
        ui.horizontal(|ui| {
          #[cfg(not(target_os = "macos"))]
          if ui.button("Use local agent").clicked() {
            self.open(
              "Local".into(),
              Connection::Local(Local::default()),
              ui.ctx(),
            );
          }
          #[cfg(target_os = "macos")]
          if ui.button("Capture a local window").clicked() {
            let viewer = view::App::new(super::macos_local::Viewer::new(ui.ctx()));
            self.connected = Some(("Local".into(), Connected::Macos(viewer)));
          }
          let selected = self.selected_host().cloned();
          ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
              .add_enabled(
                selected.is_some(),
                egui::Button::new("Connect").min_size(egui::vec2(110.0, 36.0)),
              )
              .on_hover_text("Sync ctream-agent to the selected host and load its sources")
              .clicked()
              && let Some(host) = selected
            {
              self.connect(host, ui.ctx());
            }
          });
        });
      });
    }
    egui::CentralPanel::default_margins().show(ui, |ui| {
      if let Some(connecting) = &self.connecting {
        let stopping = connecting.cancelled.load(Ordering::Acquire);
        ui.horizontal(|ui| {
          ui.spinner();
          ui.label(if stopping {
            "Cancelling…"
          } else {
            connecting.stage
          });
          if ui
            .add_enabled(!stopping, egui::Button::new("Cancel"))
            .clicked()
          {
            connecting.cancelled.store(true, Ordering::Release);
          }
        });
        ui.label(&connecting.host_name);
        return;
      }
      if let Some(error) = &self.host_error {
        ui.colored_label(ui.visuals().error_fg_color, error);
      } else if self.loading.is_some() {
        ui.horizontal(|ui| {
          ui.spinner();
          ui.label("Loading workspace hosts…");
        });
      } else if self.hosts.is_empty() {
        ui.label("No saved hosts. Add one in ctl, then refresh.");
      }
      if self.loading.is_none()
        && let Some(error) = &self.connect_error
      {
        ui.colored_label(ui.visuals().error_fg_color, error.to_string());
      }
      egui::ScrollArea::vertical().show(ui, |ui| {
        for host in &self.hosts {
          let active = self.selected.as_deref() == Some(host.id.as_str());
          if ui
            .selectable_label(active, &host.name)
            .on_hover_text(&host.id)
            .clicked()
            && !active
          {
            self.selected = Some(host.id.clone());
            self.connect_error = None;
          }
        }
      });
    });
  }
}

impl eframe::App for Preview {
  fn logic(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
    if let Some((_, connected)) = &mut self.connected {
      connected.logic(ctx);
    }
  }

  fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
    if ui.input(|input| input.viewport().close_requested()) {
      if let Some(connecting) = self.connecting.take() {
        connecting.cancelled.store(true, Ordering::Release);
        let _ = connecting.finished.recv_timeout(Duration::from_millis(100));
      }
      self.connected = None;
      return;
    }
    self.poll(ui.ctx());
    let mut change_host = false;
    if let Some((host, viewer)) = &mut self.connected {
      if !viewer.is_viewing() {
        egui::Panel::top("workspace_connection").show(ui, |ui| {
          ui.horizontal(|ui| {
            ui.strong(host.as_str());
            change_host = ui.button("Change host").clicked();
          });
        });
      }
      if !change_host {
        viewer.update_ui(ui);
        return;
      }
    }
    self.connected = None;
    if change_host {
      self.connect_error = None;
      self.refresh(ui.ctx());
      ui.ctx().request_repaint();
      return;
    }
    self.pick_host(ui);
  }
}

fn completed<T>(receiver: &Receiver<io::Result<T>>) -> Option<io::Result<T>> {
  match receiver.try_recv() {
    Ok(result) => Some(result),
    Err(TryRecvError::Empty) => None,
    Err(TryRecvError::Disconnected) => Some(Err(io::Error::other("Request stopped"))),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use ctl_bundle::ctream::AgentTarget;

  #[test]
  fn refresh_retries_a_missing_agent_for_the_reloaded_host() {
    let ctx = egui::Context::default();
    let missing_agent = || Some(agent_sync::missing_agent_error(AgentTarget::LinuxX64));
    for (connect_error, host_id, retry) in [
      (missing_agent(), "selected-id", true),
      (missing_agent(), "other-id", false),
      (
        Some(io::Error::from(io::ErrorKind::NotFound)),
        "selected-id",
        false,
      ),
      (None, "selected-id", false),
    ] {
      let mut preview = Preview {
        workspace: Workspace::with_ctl(std::path::PathBuf::new()),
        source: SourceOptions {
          target: None,
          windows: false,
          window: None,
          driver: "auto".into(),
        },
        hosts: vec![Host {
          id: "selected-id".into(),
          name: "Old name".into(),
        }],
        selected: Some("selected-id".into()),
        loading: None,
        host_error: None,
        connecting: None,
        connect_error,
        connected: None,
      };
      preview.refresh(&ctx);
      let (send, result) = mpsc::channel();
      send
        .send(Ok(vec![Host {
          id: host_id.into(),
          name: "Refreshed host".into(),
        }]))
        .unwrap();
      preview.loading = Some(result);
      preview.poll(&ctx);

      assert_eq!(
        preview
          .connecting
          .as_ref()
          .map(|connection| connection.host_name.as_str()),
        retry.then_some("Refreshed host")
      );
      assert_eq!(
        preview.selected.as_deref(),
        (host_id == "selected-id").then_some("selected-id")
      );
    }
  }
}
