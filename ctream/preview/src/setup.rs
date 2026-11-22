use eframe::egui;

pub(super) fn heading(ui: &mut egui::Ui, text: &str) {
  ui.label(
    egui::RichText::new(text)
      .small()
      .strong()
      .color(ui.visuals().weak_text_color()),
  );
  ui.add_space(8.0);
}

pub(super) fn panes(ui: &mut egui::Ui) -> (egui::Rect, egui::Rect) {
  let (body, _) = ui.allocate_exact_size(ui.available_size(), egui::Sense::hover());
  let selector_width = (body.width() * 0.36).clamp(180.0, 235.0);
  let selectors = egui::Rect::from_min_size(body.min, egui::vec2(selector_width, body.height()));
  let pane = egui::Rect::from_min_max(egui::pos2(selectors.right() + 22.0, body.top()), body.max);
  let separator_x = selectors.right() + 11.0;
  ui.painter().line_segment(
    [
      egui::pos2(separator_x, body.top()),
      egui::pos2(separator_x, body.bottom()),
    ],
    ui.visuals().widgets.noninteractive.bg_stroke,
  );
  (selectors, pane)
}

pub(super) fn window_name<'a>(title: &'a str, application: &'a str) -> &'a str {
  if !title.is_empty() {
    title
  } else if !application.is_empty() {
    application
  } else {
    "Untitled window"
  }
}

pub(super) fn window_picker<'a, K>(
  ui: &mut egui::Ui,
  selected: Option<&str>,
  windows: impl IntoIterator<Item = (K, bool, &'a str, &'a str)>,
) -> Option<K> {
  ui.label("Window");
  let mut choice = None;
  egui::ComboBox::from_id_salt("window")
    .selected_text(selected.unwrap_or("Choose a window…"))
    .truncate()
    .width(ui.available_width())
    .show_ui(ui, |ui| {
      for (key, active, title, application) in windows {
        if ui
          .selectable_label(active, window_name(title, application))
          .on_hover_text(application)
          .clicked()
          && !active
        {
          choice = Some(key);
        }
      }
    });
  choice
}
