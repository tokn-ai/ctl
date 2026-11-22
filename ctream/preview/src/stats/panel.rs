use super::OVERLAY_MARGIN;
use eframe::egui;

const MUTED: egui::Color32 = egui::Color32::from_rgb(174, 188, 205);
const COLUMNS_WIDTH: f32 = 560.0;

fn panel_bounds(video: egui::Rect) -> egui::Rect {
  let inset = (video.size() * 0.25).min(egui::Vec2::splat(OVERLAY_MARGIN));
  let available = video.shrink2(inset);
  let size = egui::vec2(
    (video.width() * 0.8).clamp(176.0, 680.0),
    (video.height() * 0.78).min(420.0),
  )
  .min(available.size());
  egui::Rect::from_min_size(
    egui::pos2(available.left(), available.bottom() - size.y),
    size,
  )
}

pub(super) struct Graph<'a> {
  pub title: &'a str,
  pub value: &'a str,
  pub samples: &'a [Option<f64>],
  pub color: egui::Color32,
}

pub(super) fn show(
  ui: &mut egui::Ui,
  video: egui::Rect,
  rows: &[(&str, &str)],
  graphs: &[Graph<'_>],
) -> bool {
  let escape = ui.input_mut(|input| {
    input.focused && input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)
  });
  let video = video.intersect(ui.clip_rect());
  if !video.is_positive() {
    return escape;
  }
  let bounds = panel_bounds(video);
  let height_id = ui.id().with("expanded_diagnostics_height");
  let height = ui
    .data(|data| data.get_temp::<f32>(height_id))
    .unwrap_or(bounds.height())
    .min(bounds.height());
  let rect = egui::Rect::from_min_size(
    egui::pos2(bounds.left(), bounds.bottom() - height),
    egui::vec2(bounds.width(), height),
  );
  ui.painter().rect(
    rect,
    8,
    egui::Color32::from_rgba_unmultiplied(12, 17, 25, 234),
    egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(30)),
    egui::StrokeKind::Inside,
  );

  // Lay out against the height limit even when the visible panel is shorter,
  // so a narrower view or wrapped row can grow it on the next paint.
  let padding = (bounds.size() * 0.25).min(egui::vec2(12.0, 10.0));
  let clip = rect.shrink2(padding);
  let content = egui::Rect::from_min_size(clip.min, bounds.size() - 2.0 * padding);
  let mut ui = ui.new_child(
    egui::UiBuilder::new()
      .id_salt("expanded_diagnostics")
      .max_rect(content)
      .layout(egui::Layout::top_down(egui::Align::Min)),
  );
  ui.set_clip_rect(clip);
  let style = ui.style_mut();
  style.visuals = egui::Visuals::dark();
  style.interaction.selectable_labels = false;

  let collapse = ui
    .horizontal(|ui| {
      ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let collapse = ui.button("Collapse");
        let copy = ui.button("Copy");
        if copy.clicked() {
          ui.ctx().copy_text(snapshot(rows, graphs));
        }
        if collapse.clicked() || escape {
          collapse.surrender_focus();
          copy.surrender_focus();
        }
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
          ui.add(egui::Label::new(egui::RichText::new("Diagnostics").strong()).truncate());
        });
        collapse.clicked()
      })
      .inner
    })
    .inner;

  ui.separator();
  egui::ScrollArea::vertical()
    .id_salt("diagnostics_content")
    .auto_shrink([false, true])
    .min_scrolled_height(0.0)
    .max_height(ui.available_height())
    .show(&mut ui, |ui| {
      if rect.width() >= COLUMNS_WIDTH && !rows.is_empty() && !graphs.is_empty() {
        ui.columns_const(|[details, charts]| {
          details.set_clip_rect(details.max_rect().intersect(details.clip_rect()));
          charts.set_clip_rect(charts.max_rect().intersect(charts.clip_rect()));
          detail_rows(details, rows);
          chart_rows(charts, graphs);
        });
      } else {
        detail_rows(ui, rows);
        if !rows.is_empty() && !graphs.is_empty() {
          ui.separator();
        }
        chart_rows(ui, graphs);
      }
    });
  let measured = (ui.min_rect().height() + 2.0 * padding.y).min(bounds.height());
  if (measured - height).abs() > 0.5 {
    ui.data_mut(|data| data.insert_temp(height_id, measured));
    ui.ctx().request_repaint();
  }
  collapse || escape
}

fn detail_rows(ui: &mut egui::Ui, rows: &[(&str, &str)]) {
  let label_width = (ui.available_width() * 0.38).min(158.0);
  for &(label, value) in rows {
    ui.horizontal_top(|ui| {
      ui.allocate_ui_with_layout(
        egui::vec2(label_width, 0.0),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
          ui.set_width(label_width);
          ui.add(egui::Label::new(egui::RichText::new(label).color(MUTED)).wrap());
        },
      );
      ui.add(egui::Label::new(egui::RichText::new(value).monospace()).wrap());
    });
  }
}

fn chart_rows(ui: &mut egui::Ui, graphs: &[Graph<'_>]) {
  for graph in graphs {
    ui.horizontal(|ui| {
      ui.label(egui::RichText::new(graph.title).color(MUTED));
      ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.add(egui::Label::new(egui::RichText::new(graph.value).monospace()).truncate());
      });
    });
    sparkline(ui, graph);
    ui.add_space(5.0);
  }
}

fn snapshot(rows: &[(&str, &str)], graphs: &[Graph<'_>]) -> String {
  let mut text = String::from("ctream diagnostics\n");
  let values = rows
    .iter()
    .copied()
    .chain(graphs.iter().map(|graph| (graph.title, graph.value)));
  for (label, value) in values {
    text.push_str(label);
    text.push_str(": ");
    text.push_str(value);
    text.push('\n');
  }
  text
}

fn sparkline(ui: &mut egui::Ui, graph: &Graph<'_>) {
  let (rect, _) =
    ui.allocate_exact_size(egui::vec2(ui.available_width(), 34.0), egui::Sense::hover());
  let painter = ui.painter_at(rect);
  painter.rect_filled(rect, 3, egui::Color32::from_black_alpha(52));
  let plot = rect.shrink2(egui::vec2(3.0, 4.0));
  painter.hline(
    plot.x_range(),
    plot.bottom(),
    egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(33)),
  );

  let max = graph
    .samples
    .iter()
    .flatten()
    .copied()
    .filter(|value| value.is_finite())
    .fold(1.0_f64, f64::max);
  let intervals = graph.samples.len().saturating_sub(1).max(1) as f32;
  let stroke = egui::Stroke::new(1.5_f32, graph.color);
  let mut points = Vec::with_capacity(graph.samples.len());
  for (index, value) in graph.samples.iter().enumerate() {
    if let Some(value) = value.filter(|value| value.is_finite() && *value >= 0.0) {
      let x = plot.left() + plot.width() * (index as f32 / intervals);
      let y = plot.bottom() - plot.height() * (value / max) as f32;
      points.push(egui::pos2(x, y));
    } else {
      paint_segment(&painter, std::mem::take(&mut points), stroke);
    }
  }
  paint_segment(&painter, points, stroke);
}

fn paint_segment(painter: &egui::Painter, points: Vec<egui::Pos2>, stroke: egui::Stroke) {
  match points.as_slice() {
    [] => {}
    [only] => {
      painter.circle_filled(*only, 1.5, stroke.color);
    }
    _ => {
      painter.add(egui::Shape::line(points, stroke));
    }
  }
}
