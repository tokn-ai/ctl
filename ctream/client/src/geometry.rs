use std::marker::PhantomData;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FrameSpace {}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NormalizedSpace {}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ViewportSpace {}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point<S> {
  pub x: f64,
  pub y: f64,
  space: PhantomData<S>,
}

impl<S> Point<S> {
  pub fn new(x: f64, y: f64) -> Self {
    Self {
      x,
      y,
      space: PhantomData,
    }
  }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect<S> {
  min: Point<S>,
  max: Point<S>,
}

impl<S> Rect<S> {
  pub fn new(min: Point<S>, max: Point<S>) -> Option<Self> {
    ([min.x, min.y, max.x, max.y, max.x - min.x, max.y - min.y]
      .into_iter()
      .all(f64::is_finite)
      && max.x > min.x
      && max.y > min.y)
      .then_some(Self { min, max })
  }

  pub fn min(&self) -> Point<S> {
    Point::new(self.min.x, self.min.y)
  }

  pub fn max(&self) -> Point<S> {
    Point::new(self.max.x, self.max.y)
  }

  pub fn contains(&self, point: Point<S>) -> bool {
    point.x >= self.min.x && point.x <= self.max.x && point.y >= self.min.y && point.y <= self.max.y
  }
}

#[derive(Clone, Copy, Debug)]
pub struct Transform<F, T> {
  from: Rect<F>,
  to: Rect<T>,
}

impl<F, T> Transform<F, T> {
  pub fn new(from: Rect<F>, to: Rect<T>) -> Self {
    Self { from, to }
  }

  pub fn map(&self, point: Point<F>) -> Point<T> {
    Point::new(
      self.to.min.x
        + (point.x - self.from.min.x) / (self.from.max.x - self.from.min.x)
          * (self.to.max.x - self.to.min.x),
      self.to.min.y
        + (point.y - self.from.min.y) / (self.from.max.y - self.from.min.y)
          * (self.to.max.y - self.to.min.y),
    )
  }

  pub fn inverse(self) -> Transform<T, F> {
    Transform::new(self.to, self.from)
  }
}

/// The image transform shared by rendering, overlays, and pointer input.
#[derive(Clone, Copy, Debug)]
pub struct ImageGeometry {
  size: [u32; 2],
  transform: Transform<FrameSpace, ViewportSpace>,
}

impl ImageGeometry {
  pub fn fit(area: Rect<ViewportSpace>, size: [u32; 2]) -> Option<Self> {
    let [width, height] = size.map(f64::from);
    if width == 0.0 || height == 0.0 {
      return None;
    }
    let scale = ((area.max.x - area.min.x) / width).min((area.max.y - area.min.y) / height);
    let left = area.min.x + ((area.max.x - area.min.x) - width * scale) / 2.0;
    let top = area.min.y + ((area.max.y - area.min.y) - height * scale) / 2.0;
    let viewport = Rect::new(
      Point::new(left, top),
      Point::new(left + width * scale, top + height * scale),
    )?;
    Self::fill(viewport, size)
  }

  /// Maps the complete frame onto the given viewport, even during native resize.
  pub fn fill(viewport: Rect<ViewportSpace>, size: [u32; 2]) -> Option<Self> {
    let [width, height] = size.map(f64::from);
    let frame = Rect::new(Point::new(0.0, 0.0), Point::new(width, height))?;
    Some(Self {
      size,
      transform: Transform::new(frame, viewport),
    })
  }

  pub fn viewport(&self) -> Rect<ViewportSpace> {
    self.transform.to
  }

  pub fn size(&self) -> [u32; 2] {
    self.size
  }

  pub fn pixel(&self, point: Point<ViewportSpace>) -> Option<[u32; 2]> {
    if !self.viewport().contains(point) {
      return None;
    }
    let point = self.transform.inverse().map(point);
    Some([
      (point.x.round() as u32).min(self.size[0] - 1),
      (point.y.round() as u32).min(self.size[1] - 1),
    ])
  }

  pub fn normalized_rect(&self, [x, y, width, height]: [f32; 4]) -> Option<Rect<ViewportSpace>> {
    let [x, y, width, height] = [x, y, width, height].map(f64::from);
    let bounds = Rect::<NormalizedSpace>::new(Point::new(x, y), Point::new(x + width, y + height))?;
    let [width, height] = self.size.map(f64::from);
    let clipped = Rect::new(
      Point::new(
        bounds.min.x.clamp(0.0, 1.0) * width,
        bounds.min.y.clamp(0.0, 1.0) * height,
      ),
      Point::new(
        bounds.max.x.clamp(0.0, 1.0) * width,
        bounds.max.y.clamp(0.0, 1.0) * height,
      ),
    )?;
    Rect::new(
      self.transform.map(clipped.min),
      self.transform.map(clipped.max),
    )
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn image_overlay_and_pointer_share_the_transform() {
    for (size, expected) in [
      ([1000.0, 400.0], [320.0, 230.0, 720.0, 330.0]),
      ([400.0, 800.0], [120.0, 430.0, 320.0, 480.0]),
    ] {
      let area = Rect::new(
        Point::new(20.0, 30.0),
        Point::new(20.0 + size[0], 30.0 + size[1]),
      )
      .unwrap();
      let image = ImageGeometry::fit(area, [800, 400]).unwrap();
      let overlay = image.normalized_rect([0.25, 0.5, 0.5, 0.25]).unwrap();
      assert_eq!(
        [overlay.min.x, overlay.min.y, overlay.max.x, overlay.max.y],
        expected
      );
      assert_eq!(image.pixel(overlay.min), Some([200, 200]));
      assert_eq!(image.pixel(image.viewport().max), Some([799, 399]));
      assert_eq!(
        image.pixel(Point::new(image.viewport().min.x - 1.0, 30.0)),
        None
      );
    }
  }

  #[test]
  fn clips_overlays_and_rejects_invalid_geometry() {
    let area = Rect::new(Point::new(20.0, 30.0), Point::new(420.0, 230.0)).unwrap();
    let image = ImageGeometry::fit(area, [800, 400]).unwrap();
    let overlay = image.normalized_rect([-0.25, 0.5, 0.5, 1.0]).unwrap();
    assert_eq!(
      [overlay.min.x, overlay.min.y, overlay.max.x, overlay.max.y],
      [20.0, 130.0, 120.0, 230.0]
    );
    for bounds in [
      [2.0, 0.0, 0.5, 0.5],
      [f32::NAN, 0.0, 0.5, 0.5],
      [0.0, 0.0, 0.0, 1.0],
    ] {
      assert!(image.normalized_rect(bounds).is_none());
    }
    assert!(ImageGeometry::fit(area, [0, 400]).is_none());
    assert!(Rect::<ViewportSpace>::new(Point::new(0.0, 0.0), Point::new(f64::NAN, 1.0)).is_none());
    assert_eq!(image.pixel(Point::new(f64::NAN, 0.0)), None);
  }
}
