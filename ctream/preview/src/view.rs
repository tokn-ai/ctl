use ctream_client::geometry::{ImageGeometry, Point, Rect, ViewportSpace};
use eframe::egui;

pub fn fit(area: egui::Rect, size: [u32; 2]) -> Option<ImageGeometry> {
  let area = Rect::new(
    Point::new(f64::from(area.min.x), f64::from(area.min.y)),
    Point::new(f64::from(area.max.x), f64::from(area.max.y)),
  )?;
  ImageGeometry::fit(area, size)
}

pub fn rect(rect: Rect<ViewportSpace>) -> egui::Rect {
  egui::Rect::from_min_max(
    egui::pos2(rect.min().x as f32, rect.min().y as f32),
    egui::pos2(rect.max().x as f32, rect.max().y as f32),
  )
}
