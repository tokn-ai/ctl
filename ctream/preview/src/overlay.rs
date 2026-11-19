use ctream::protocol::{ControlBounds, Element};
use eframe::egui::{self, Color32, Painter, Rect, Stroke, StrokeKind, pos2};

pub fn control_rect(video: Rect, bounds: &ControlBounds) -> Option<Rect> {
  let ControlBounds {
    x,
    y,
    width,
    height,
  } = *bounds;
  let right = x + width;
  let bottom = y + height;
  if !video.is_positive()
    || ![x, y, right, bottom].into_iter().all(f32::is_finite)
    || width <= 0.0
    || height <= 0.0
  {
    return None;
  }

  let at = |x: f32, y: f32| {
    pos2(
      egui::lerp(video.x_range(), x.clamp(0.0, 1.0)),
      egui::lerp(video.y_range(), y.clamp(0.0, 1.0)),
    )
  };
  let rect = Rect::from_min_max(at(x, y), at(right, bottom));
  rect.is_positive().then_some(rect)
}

pub fn paint(painter: &Painter, video: Rect, elements: &[Element]) {
  if elements.is_empty() {
    return;
  }
  let painter = painter.with_clip_rect(video);
  for rect in elements
    .iter()
    .filter_map(|element| control_rect(video, element.bounds.as_ref()?))
  {
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn control_follows_video_in_wide_and_tall_viewports() {
    let bounds = ControlBounds {
      x: 0.25,
      y: 0.5,
      width: 0.5,
      height: 0.25,
    };
    let image_size = [800, 400];
    let wide = Rect::from_min_max(pos2(20.0, 30.0), pos2(1020.0, 430.0));
    let tall = Rect::from_min_max(pos2(20.0, 30.0), pos2(420.0, 830.0));
    assert_eq!(
      control_rect(crate::input::image_rect(wide, image_size), &bounds),
      Some(Rect::from_min_max(pos2(320.0, 230.0), pos2(720.0, 330.0)))
    );
    assert_eq!(
      control_rect(crate::input::image_rect(tall, image_size), &bounds),
      Some(Rect::from_min_max(pos2(120.0, 430.0), pos2(320.0, 480.0)))
    );
  }

  #[test]
  fn control_is_clipped_to_video_and_empty_bounds_are_ignored() {
    let video = Rect::from_min_max(pos2(20.0, 30.0), pos2(420.0, 230.0));
    let mut bounds = ControlBounds {
      x: -0.25,
      y: 0.5,
      width: 0.5,
      height: 1.0,
    };
    assert_eq!(
      control_rect(video, &bounds),
      Some(Rect::from_min_max(pos2(20.0, 130.0), pos2(120.0, 230.0)))
    );
    bounds.x = 2.0;
    assert_eq!(control_rect(video, &bounds), None);
    bounds.x = f32::NAN;
    assert_eq!(control_rect(video, &bounds), None);
  }
}
