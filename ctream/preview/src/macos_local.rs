use super::{chrome, drivers, input, setup, view};
use ctream_client::media::Frame;
use ctream_driver_capture::macos::screen_capture_kit::{Capture, Event, Window, list_windows};
use ctream_driver_input::macos::{WindowIdentity, WindowInput};
use eframe::egui;
use std::{
  io,
  sync::{
    Arc, Mutex,
    mpsc::{self, Receiver, TryRecvError},
  },
};

pub(super) const DRIVER: &str = "macos.screencapturekit";

#[derive(Default)]
struct Update {
  frame: Option<Frame>,
  end: Option<End>,
}

enum End {
  Cancelled,
  Finished,
  Failed(String),
}

impl input::Sink for WindowInput {
  fn move_pointer(&mut self, x: u32, y: u32, size: [u32; 2]) -> io::Result<()> {
    Self::move_pointer(self, x, y, size)
  }

  fn button(&mut self, code: u8, pressed: bool) -> io::Result<()> {
    Self::button(self, code, pressed)
  }

  fn key(&mut self, name: &str, pressed: bool) -> io::Result<()> {
    Self::key(self, name, pressed)
  }

  fn reset(&mut self) -> io::Result<()> {
    Self::reset(self)
  }
}

#[derive(Default)]
pub(super) struct Viewer {
  windows: Vec<Window>,
  window_query: Option<Receiver<io::Result<Vec<Window>>>>,
  window_error: Option<io::Error>,
  selected_window: Option<u32>,
  capture: Option<Capture>,
  update: Arc<Mutex<Update>>,
  texture: Option<egui::TextureHandle>,
  input: Option<input::Input<WindowInput>>,
  input_error: Option<String>,
  status: Option<(String, bool)>,
  viewing: bool,
  opening: bool,
  size: view::AspectResize,
  chrome: chrome::Chrome,
}

impl Viewer {
  pub(super) fn new(ctx: &egui::Context) -> Self {
    let mut viewer = Self::default();
    viewer.refresh_windows(ctx);
    viewer
  }

  fn refresh_windows(&mut self, ctx: &egui::Context) {
    self.window_error = None;
    let (send, receive) = mpsc::channel();
    let ctx = ctx.clone();
    self.window_query = match list_windows(move |result| {
      let _ = send.send(result);
      ctx.request_repaint_of(egui::ViewportId::ROOT);
    }) {
      Ok(()) => Some(receive),
      Err(error) => {
        self.windows.clear();
        self.window_error = Some(error);
        None
      }
    };
  }

  fn clear_capture(&mut self) {
    self.input = None;
    self.input_error = None;
    self.selected_window = None;
    self.capture = None;
    self.texture = None;
    self.status = None;
    self.update = Arc::default();
  }

  fn choose(&mut self, ctx: &egui::Context, window_index: Option<usize>) {
    self.clear_capture();
    let update = Arc::clone(&self.update);
    let repaint = view::capture_repaint(ctx);
    let callback = move |event| {
      let mut update = update.lock().unwrap();
      let frame_pending = update.frame.is_some();
      match event {
        Event::Frame { size, pixels } => {
          update.frame = Some(Frame { size, pixels });
        }
        Event::Cancelled => update.end = Some(End::Cancelled),
        Event::Finished => update.end = Some(End::Finished),
        Event::Failed(error) => update.end = Some(End::Failed(error)),
      }
      let wake = !frame_pending || update.end.is_some();
      drop(update);
      if wake {
        let _ = repaint.try_send(());
      }
    };
    let window = window_index.map(|index| &self.windows[index]);
    let result = if let Some(window) = window {
      Capture::start_window(window, callback)
    } else {
      Capture::start(callback)
    };
    match result {
      Ok(capture) => {
        self.selected_window = window.map(|window| window.id);
        self.capture = Some(capture);
      }
      Err(error) => self.status = Some((error.to_string(), true)),
    }
  }

  fn poll(&mut self, ctx: &egui::Context) {
    let result = self
      .window_query
      .as_ref()
      .and_then(|query| match query.try_recv() {
        Ok(result) => Some(result),
        Err(TryRecvError::Disconnected) => Some(Err(io::Error::other("Window discovery ended."))),
        Err(TryRecvError::Empty) => None,
      });
    if let Some(result) = result {
      self.window_query = None;
      match result {
        Ok(windows) => self.windows = windows,
        Err(error) => {
          self.windows.clear();
          self.window_error = Some(error);
        }
      }
    }
    let Update { frame, end } = std::mem::take(&mut *self.update.lock().unwrap());
    if let Some(end) = end {
      if self.viewing {
        self.close_viewer(ctx);
      }
      let refresh = self.selected_window.is_some() && matches!(end, End::Finished);
      self.clear_capture();
      if refresh {
        self.refresh_windows(ctx);
      }
      self.status = match end {
        End::Cancelled => None,
        End::Finished => Some(("Window capture ended.".into(), false)),
        End::Failed(error) => Some((error, true)),
      };
    } else if let Some(frame) = frame {
      view::upload(
        ctx,
        &mut self.texture,
        egui::ColorImage::from_rgb(frame.size, &frame.pixels),
      );
    }
  }

  pub(super) fn is_viewing(&self) -> bool {
    self.viewing
  }

  fn close_viewer(&mut self, ctx: &egui::Context) {
    self.input = None;
    self.input_error = None;
    self.chrome.dismiss();
    self.viewing = false;
    self.opening = false;
    view::restore_setup(ctx);
  }

  fn show_setup(&mut self, ctx: &egui::Context) {
    let supported = Capture::is_supported();
    let mut choose_system = false;
    let mut choose_window = None;
    let mut refresh = false;
    let mut stop = false;
    let mut start = false;
    egui::TopBottomPanel::top("local_window_heading").show(ctx, |ui| {
      ui.horizontal(|ui| {
        ui.heading("Local window");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
          if self.capture.is_some() {
            stop = ui.button("Stop capture").clicked();
          }
        });
      });
      ui.label("Choose a window, then start.");
    });
    egui::TopBottomPanel::bottom("local_window_actions").show(ctx, |ui| {
      ui.horizontal(|ui| {
        ui.label("View only");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
          start = ui
            .add_enabled(
              self.capture.is_some() && self.texture.is_some(),
              egui::Button::new("Start").min_size(egui::vec2(110.0, 36.0)),
            )
            .clicked();
        });
      });
    });
    egui::CentralPanel::default().show(ctx, |ui| {
      let (selectors, pane) = setup::panes(ui);
      ui.scope_builder(egui::UiBuilder::new().max_rect(selectors), |ui| {
        ui.set_clip_rect(selectors.intersect(ui.clip_rect()));
        setup::heading(ui, "CAPTURE DRIVER");
        drivers::screen_capture_kit(ui);
        ui.add_space(18.0);
        setup::heading(ui, "SOURCE");
        let selected = self
          .windows
          .iter()
          .find(|window| Some(window.id) == self.selected_window);
        let label = selected.map(|window| setup::window_name(&window.title, &window.application));
        let choices = self.windows.iter().enumerate().map(|(index, window)| {
          (
            index,
            Some(window.id) == self.selected_window,
            window.title.as_str(),
            window.application.as_str(),
          )
        });
        ui.add_enabled_ui(
          !self.windows.is_empty() && self.window_query.is_none(),
          |ui| {
            choose_window = setup::window_picker(ui, label, choices);
          },
        );
        if let Some(error) = &self.window_error {
          if error.kind() == io::ErrorKind::PermissionDenied {
            ui.small(
              "Allow Screen Recording in System Settings to list windows, or use the macOS picker.",
            );
          } else if supported {
            ui.colored_label(ui.visuals().error_fg_color, error.to_string());
          }
        } else if self.window_query.is_some() {
          ui.small("Finding windows…");
        } else if self.windows.is_empty() {
          ui.small("No capturable windows found.");
        }
        ui.add_space(16.0);
        refresh = ui
          .add_enabled(
            supported && self.window_query.is_none(),
            egui::Button::new("Refresh windows"),
          )
          .clicked();
        choose_system = ui
          .add_enabled(supported, egui::Button::new("Use macOS picker…"))
          .clicked();
      });
      ui.scope_builder(egui::UiBuilder::new().max_rect(pane), |ui| {
        ui.set_clip_rect(pane.intersect(ui.clip_rect()));
        setup::heading(ui, "LIVE PREVIEW");
        if let Some(texture) = &self.texture {
          let area = ui.available_rect_before_wrap();
          if let Some(image) = view::fit(area, texture.size().map(|v| v as u32)) {
            view::paint(ui, texture, area, &image);
          }
        } else {
          ui.centered_and_justified(|ui| {
            if let Some((status, error)) = &self.status {
              if *error {
                ui.colored_label(ui.visuals().error_fg_color, status);
              } else {
                ui.label(status);
              }
            } else if !supported {
              ui.label("Local window capture requires macOS 14 or later.");
            } else if self.capture.is_some() {
              ui.label("Waiting for the selected window…");
            } else {
              ui.label("Choose a window to preview it.");
            }
          });
        }
      });
    });
    if choose_system || choose_window.is_some() {
      self.choose(ctx, choose_window);
    } else if stop {
      self.clear_capture();
    } else if refresh {
      self.refresh_windows(ctx);
    } else if start {
      self.input_error = match self.capture.as_ref().and_then(Capture::window_identity) {
        Some((id, pid)) => match WindowInput::open(WindowIdentity { id, pid }) {
          Ok(input) => {
            self.input = Some(input::Input::new(input));
            None
          }
          Err(error) => Some(format!("View only: {error}")),
        },
        None => Some("View only: the picker did not provide a window identity".into()),
      };
      self.viewing = true;
      self.opening = true;
      self.size = view::AspectResize::default();
      self.chrome = chrome::Chrome::default();
      ctx.request_repaint();
    }
  }

  fn show_viewer(&mut self, ctx: &egui::Context) {
    let Some(image_size) = self
      .texture
      .as_ref()
      .map(|texture| texture.size().map(|v| v as u32))
    else {
      return;
    };
    let builder = view::viewer_builder(ctx, self.opening.then_some((image_size, 0.0)));
    ctx.show_viewport_immediate(view::viewer_id(), builder, |child, class| {
      if class == egui::ViewportClass::Embedded {
        self.close_viewer(child);
        self.clear_capture();
        self.status = Some((
          "This backend cannot open a native viewer window.".into(),
          true,
        ));
        return;
      }
      if child.input(|input| input.viewport().close_requested()) {
        self.close_viewer(child);
        return;
      }
      if self.opening {
        view::hide_setup(child);
        self.opening = false;
      }
      let controls = self.chrome.show(
        child,
        chrome::Status {
          text: self.input_error.as_deref().unwrap_or("Control enabled"),
          error: self.input_error.is_some(),
          driver: Some(DRIVER),
          debug: None,
        },
      );
      if controls.close {
        self.close_viewer(child);
        return;
      }
      let video = child.available_rect().size();
      egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show(child, |ui| {
          let area = ui.available_rect_before_wrap();
          let image = view::fill(area, image_size);
          if let (Some(texture), Some(image)) = (&self.texture, &image) {
            view::paint(ui, texture, area, image);
          }
          if let Some(input) = &mut self.input
            && let Err(error) = input.update(child, image.as_ref(), &controls.pointer_regions)
          {
            self.input = None;
            self.input_error = Some(format!("View only: {error}"));
          }
          if !controls.resizing {
            self.size.update(child, image_size, video);
          }
        });
    });
  }

  pub(super) fn update_ui(&mut self, ctx: &egui::Context) {
    if ctx.input(|input| input.viewport().close_requested()) {
      self.chrome.dismiss();
      self.clear_capture();
      return;
    }
    self.poll(ctx);
    if self.viewing {
      self.show_viewer(ctx);
    }
    if !self.viewing {
      self.show_setup(ctx);
    }
  }
}

impl eframe::App for Viewer {
  fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
    self.update_ui(ctx);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn ended_capture_returns_to_setup_and_discards_late_frames() {
    let ctx = egui::Context::default();
    let mut viewer = Viewer {
      viewing: true,
      ..Default::default()
    };
    let retired = Arc::clone(&viewer.update);
    retired.lock().unwrap().end = Some(End::Finished);
    let output = ctx.run(egui::RawInput::default(), |ctx| viewer.poll(ctx));
    assert!(!viewer.is_viewing());
    assert!(
      output.viewport_output[&egui::ViewportId::ROOT]
        .commands
        .contains(&egui::ViewportCommand::Visible(true))
    );
    retired.lock().unwrap().frame = Some(Frame {
      size: [1, 1],
      pixels: vec![255, 0, 0],
    });
    let output = ctx.run(egui::RawInput::default(), |ctx| viewer.poll(ctx));
    assert!(output.textures_delta.set.is_empty());
    assert!(viewer.texture.is_none());
  }
}
