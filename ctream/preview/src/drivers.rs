use ctream_proto::{CaptureDriverDescriptor, CaptureScope, CaptureSystem, Display, Session};
use eframe::egui;

#[derive(Clone, Copy)]
enum TagStyle {
  System,
  Capability,
  Unavailable,
}

struct Tag {
  label: &'static str,
  style: TagStyle,
}

impl Tag {
  fn new(label: &'static str, style: TagStyle) -> Self {
    Self { label, style }
  }

  fn colors(&self, dark: bool) -> (egui::Color32, egui::Color32) {
    let (background, foreground) = match (self.style, dark) {
      (TagStyle::System, true) => ([42, 51, 65], [187, 205, 229]),
      (TagStyle::System, false) => ([222, 231, 244], [43, 65, 92]),
      (TagStyle::Capability, true) => ([28, 62, 54], [155, 222, 195]),
      (TagStyle::Capability, false) => ([215, 239, 229], [32, 94, 71]),
      (TagStyle::Unavailable, true) => ([66, 51, 29], [230, 196, 140]),
      (TagStyle::Unavailable, false) => ([247, 233, 207], [126, 84, 24]),
    };
    (
      egui::Color32::from_rgb(background[0], background[1], background[2]),
      egui::Color32::from_rgb(foreground[0], foreground[1], foreground[2]),
    )
  }
}

fn system(display: &Display) -> CaptureSystem {
  match display {
    Display::X11(_) => CaptureSystem::X11,
    Display::Wayland { .. } => CaptureSystem::Wayland,
    Display::Windows { .. } => CaptureSystem::Windows,
  }
}

fn tags(driver: &CaptureDriverDescriptor, displays: &[Session]) -> Vec<Tag> {
  let mut tags = Vec::new();
  if driver
    .systems
    .iter()
    .any(|system| matches!(system, CaptureSystem::X11 | CaptureSystem::Wayland))
  {
    tags.push(Tag::new("Linux", TagStyle::System));
  }
  for (system, label) in [
    (CaptureSystem::X11, "X11"),
    (CaptureSystem::Wayland, "Wayland"),
    (CaptureSystem::Windows, "Windows"),
  ] {
    if driver.systems.contains(&system) {
      tags.push(Tag::new(label, TagStyle::System));
    }
  }
  for (scope, label) in [
    (CaptureScope::Display, "Display"),
    (CaptureScope::Window, "Window"),
  ] {
    if driver.info.scopes.contains(&scope) {
      tags.push(Tag::new(label, TagStyle::Capability));
    }
  }
  let desktop_windows = driver.systems.contains(&CaptureSystem::Windows)
    && driver.info.scopes.contains(&CaptureScope::Window);
  if !desktop_windows
    && !displays
      .iter()
      .any(|display| display.selectable && driver.systems.contains(&system(&display.display)))
  {
    tags.push(Tag::new("No sources", TagStyle::Unavailable));
  }
  tags
}

fn card(ui: &mut egui::Ui, name: &str, tags: &[Tag], selected: bool) -> egui::Response {
  let width = ui.available_width();
  let padding = 8.0;
  let content_width = (width - padding * 2.0).max(1.0);
  let color = if selected {
    ui.visuals().selection.stroke.color
  } else {
    ui.visuals().widgets.inactive.fg_stroke.color
  };
  let mut title = egui::text::LayoutJob::simple(
    name.to_owned(),
    egui::TextStyle::Button.resolve(ui.style()),
    color,
    content_width,
  );
  title.wrap.max_rows = 2;
  let title = ui.painter().layout_job(title);
  let tag_font = egui::TextStyle::Small.resolve(ui.style());
  let mut x = 0.0;
  let mut y = title.size().y + 7.0;
  let mut row_height = 0.0_f32;
  let mut badges = Vec::with_capacity(tags.len());
  for tag in tags {
    let (background, foreground) = tag.colors(ui.visuals().dark_mode);
    let text = ui
      .painter()
      .layout_no_wrap(tag.label.into(), tag_font.clone(), foreground);
    let size = text.size() + egui::vec2(10.0, 4.0);
    if x > 0.0 && x + size.x > content_width {
      x = 0.0;
      y += row_height + 4.0;
      row_height = 0.0;
    }
    badges.push((egui::pos2(x, y), size, background, foreground, text));
    x += size.x + 4.0;
    row_height = row_height.max(size.y);
  }
  let height = y + row_height + padding * 2.0;
  // A standard button preserves keyboard activation and focus rendering while
  // the name and capability badges share one complete click target.
  let response = ui.add_sized(
    egui::vec2(width, height),
    egui::Button::new("").selected(selected).corner_radius(6.0),
  );
  response.widget_info(|| {
    egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), selected, name)
  });
  let origin = response.rect.min + egui::vec2(padding, padding);
  ui.painter().galley(origin, title, color);
  for (position, size, background, foreground, text) in badges {
    let rect = egui::Rect::from_min_size(origin + position.to_vec2(), size);
    ui.painter().rect_filled(rect, 4.0, background);
    ui.painter()
      .galley(rect.min + egui::vec2(5.0, 2.0), text, foreground);
  }
  response
}

#[cfg(target_os = "macos")]
pub(super) fn screen_capture_kit(ui: &mut egui::Ui) {
  card(
    ui,
    "ScreenCaptureKit",
    &[
      Tag::new("macOS", TagStyle::System),
      Tag::new("Window", TagStyle::Capability),
    ],
    true,
  );
}

/// All advertised drivers remain visible regardless of the current source.
/// Metadata supplies their tags, including for drivers compiled into custom agents.
pub(super) fn chooser(
  ui: &mut egui::Ui,
  drivers: &[CaptureDriverDescriptor],
  selected: &str,
  displays: &[Session],
) -> Option<String> {
  let mut choice = None;
  ui.push_id("auto", |ui| {
    let response = card(
      ui,
      "Automatic",
      &[
        Tag::new("Default", TagStyle::System),
        Tag::new("Display", TagStyle::Capability),
        Tag::new("Window", TagStyle::Capability),
      ],
      selected == "auto",
    )
    .on_hover_text("Let the agent choose a driver for the selected source.");
    if response.clicked() {
      choice = Some("auto".into());
    }
  });
  for driver in drivers {
    ui.add_space(4.0);
    ui.push_id(&driver.info.id, |ui| {
      let response = card(
        ui,
        &driver.info.name,
        &tags(driver, displays),
        selected == driver.info.id,
      )
      .on_hover_text(format!("{}\n{}", driver.info.name, driver.info.id));
      if response.clicked() {
        choice = Some(driver.info.id.clone());
      }
    });
  }
  choice
}
