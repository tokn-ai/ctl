//! Native viewer controls over a synthetic frame for desktop testing.

#[path = "../src/chrome.rs"]
mod chrome;
#[cfg(target_os = "macos")]
#[path = "../src/macos_chrome.rs"]
#[allow(dead_code)] // The capture-driven repaint hook is unused in this fixture.
mod macos_chrome;

use eframe::egui;

struct Fixture {
  chrome: chrome::Chrome,
  frame: egui::TextureHandle,
  debug: bool,
}

impl Fixture {
  fn new(ctx: &egui::Context) -> Self {
    let colors = [
      egui::Color32::from_rgb(23, 38, 50),
      egui::Color32::from_rgb(53, 80, 101),
    ];
    let pixels = (0..16 * 9).map(|i| colors[(i % 16 + i / 16) % 2]).collect();
    let frame = ctx.load_texture(
      "controls fixture checkerboard",
      egui::ColorImage::new([16, 9], pixels),
      egui::TextureOptions::NEAREST,
    );
    Self {
      chrome: chrome::Chrome::default(),
      frame,
      debug: false,
    }
  }
}

impl eframe::App for Fixture {
  fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
    let escape = ui.input(|input| input.key_pressed(egui::Key::Escape));
    let controls = self.chrome.show(
      ui,
      chrome::Status {
        text: "Synthetic frame",
        error: false,
        driver: None,
        debug: Some(self.debug),
      },
    );
    if controls.close || escape {
      self.chrome.dismiss();
      ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
      return;
    }
    self.debug ^= controls.toggle_debug;
    egui::CentralPanel::no_frame().show(ui, |ui| {
      let content = ui.available_rect_before_wrap();
      let painter = ui.painter();
      painter.image(
        self.frame.id(),
        content,
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
      );
      let size = format!("{:.0} × {:.0} pt", content.width(), content.height());
      let label = if self.debug {
        let local = ui.input(|input| {
          input.pointer.hover_pos().is_some_and(|point| {
            controls
              .pointer_regions
              .iter()
              .any(|region| region.contains(point))
          })
        });
        let pointer = if local { "controls" } else { "video" };
        format!("{size} · Debug on\nPointer over {pointer}")
      } else {
        format!("{size} · Debug off")
      };
      painter.text(
        content.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::monospace(16.0),
        egui::Color32::WHITE,
      );
    });
  }
}

fn main() -> eframe::Result<()> {
  let options = eframe::NativeOptions {
    renderer: eframe::Renderer::Wgpu,
    viewport: egui::ViewportBuilder::default()
      .with_title(chrome::VIEWER_TITLE)
      .with_decorations(false)
      .with_inner_size([640.0, 360.0])
      .with_min_inner_size([200.0, 140.0])
      .with_position([180.0, 180.0]),
    ..Default::default()
  };
  eframe::run_native(
    chrome::VIEWER_TITLE,
    options,
    Box::new(|cc| Ok(Box::new(Fixture::new(&cc.egui_ctx)))),
  )
}
