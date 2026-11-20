use ctream_client::geometry::ImageGeometry;
use ctream_proto::Element;
use eframe::egui::{Color32, Painter, Stroke, StrokeKind};

pub fn paint(painter: &Painter, image: &ImageGeometry, elements: &[Element]) {
  let painter = painter.with_clip_rect(super::view::rect(image.viewport()));
  for bounds in elements
    .iter()
    .filter_map(|element| element.bounds.as_ref())
  {
    if let Some(rect) = image.normalized_rect([bounds.x, bounds.y, bounds.width, bounds.height]) {
      let rect = super::view::rect(rect);
      painter.rect_stroke(
        rect,
        0.0,
        Stroke::new(3.0_f32, Color32::BLACK),
        StrokeKind::Inside,
      );
      painter.rect_stroke(
        rect,
        0.0,
        Stroke::new(1.5_f32, Color32::from_rgb(85, 234, 255)),
        StrokeKind::Inside,
      );
    }
  }
}
