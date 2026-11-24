use objc2_foundation::{NSPoint, NSPointInRect, NSRect, NSSize};

pub(super) fn panel_frame(
  video: NSRect,
  areas: &[NSRect],
  height: f64,
  min_width: f64,
) -> Option<NSRect> {
  if video.is_empty() || height <= 0.0 || min_width <= 0.0 {
    return None;
  }
  let (left, right) = (video.min().x, video.max().x);
  let top = video.max().y;
  let edges = horizontal_edges(areas);
  let mut heights = vec![top];
  for area in areas.iter().filter(|area| !area.is_empty()) {
    // A feasible bar can move up until its upper edge reaches a work-area
    // top or its lower edge touches the top of the video.
    let y = area.max().y - height;
    if y >= video.min().y && y < top {
      heights.push(y);
    }
  }
  heights.sort_unstable_by(|a, b| b.total_cmp(a));
  heights.dedup();

  let mut offset = None;
  for y in heights {
    let spans = covered_spans(&edges, areas, y, y + height);
    let frame = |start, width| NSRect::new(NSPoint::new(start, y), NSSize::new(width, height));
    let width = |&[start, end]: &[f64; 2]| end - start;
    if let Some([start, end]) = spans
      .iter()
      .map(|&[start, end]| [start.max(left), end.min(right)])
      .filter(|span| width(span) >= min_width)
      .max_by(|one, other| width(one).total_cmp(&width(other)))
    {
      return Some(frame(start, end - start));
    }
    if offset.is_none() {
      // If the side of the viewer is nearly offscreen, keep the buttons
      // reachable by extending the bar only as far as its minimum width.
      let overlap = |&[start, end]: &[f64; 2]| end.min(right) - start.max(left);
      offset = spans
        .into_iter()
        .filter(|span| width(span) >= min_width && overlap(span) > 0.0)
        .max_by(|one, other| overlap(one).total_cmp(&overlap(other)))
        .map(|[start, end]| frame(left.clamp(start, end - min_width), min_width));
    }
  }
  offset
}

fn horizontal_edges(areas: &[NSRect]) -> Vec<f64> {
  let mut edges: Vec<_> = areas
    .iter()
    .filter(|area| !area.is_empty())
    .flat_map(|area| [area.min().x, area.max().x])
    .collect();
  edges.sort_unstable_by(f64::total_cmp);
  edges.dedup();
  edges
}

pub(super) fn keep_panel_visible(
  previous: NSRect,
  areas: &[NSRect],
  min_width: f64,
) -> Option<NSRect> {
  let spans = covered_spans(
    &horizontal_edges(areas),
    areas,
    previous.min().y,
    previous.max().y,
  );
  if spans
    .iter()
    .any(|&[left, right]| left <= previous.min().x && right >= previous.max().x)
  {
    return Some(previous);
  }

  // A display can disappear during an owned drag. If the old placement is
  // gone and no attachment point fits, keep the bar on the nearest work area.
  let height = previous.size.height;
  let distance =
    |frame: &NSRect| (frame.origin.x - previous.origin.x).hypot(frame.origin.y - previous.origin.y);
  areas
    .iter()
    .filter(|area| area.size.width >= min_width && area.size.height >= height)
    .map(|area| {
      let width = previous.size.width.min(area.size.width).max(min_width);
      let x = previous.min().x.clamp(area.min().x, area.max().x - width);
      let y = previous.min().y.clamp(area.min().y, area.max().y - height);
      NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
    })
    .min_by(|one, other| distance(one).total_cmp(&distance(other)))
}

fn covered_spans(edges: &[f64], areas: &[NSRect], bottom: f64, top: f64) -> Vec<[f64; 2]> {
  let mut spans: Vec<[f64; 2]> = Vec::new();
  for edge in edges.windows(2) {
    let [left, right] = [edge[0], edge[1]];
    if covered_strip(areas, left, right, bottom, top) {
      if let Some(span) = spans.last_mut()
        && span[1] == left
      {
        span[1] = right;
      } else {
        spans.push([left, right]);
      }
    }
  }
  spans
}

fn covered_strip(areas: &[NSRect], left: f64, right: f64, bottom: f64, top: f64) -> bool {
  // Screen edges bound this strip, so each work area covers either its
  // entire width or none. Join vertically touching coverage before moving on.
  coverage_top(areas, bottom, |area| {
    area.min().x <= left && area.max().x >= right
  }) >= top
}

fn coverage_top(areas: &[NSRect], mut top: f64, covers: impl Fn(NSRect) -> bool) -> f64 {
  loop {
    let next = areas
      .iter()
      .copied()
      .filter(|area| area.min().y <= top && covers(*area))
      .map(|area| area.max().y)
      .fold(top, f64::max);
    if next <= top {
      return top;
    }
    top = next;
  }
}

pub(super) fn reveal_at(
  video: NSRect,
  areas: &[NSRect],
  panel: NSRect,
  pointer: NSPoint,
  height: f64,
  hold: bool,
) -> bool {
  if pointer.x < panel.min().x || pointer.x >= panel.max().x || !NSPointInRect(pointer, video) {
    return false;
  }
  let at_x = |area: NSRect| pointer.x >= area.min().x && pointer.x < area.max().x;
  let top = coverage_top(areas, panel.min().y, at_x).min(video.max().y);
  // Holding an open bar also keeps the visible path from its reveal strip to
  // the bar when work-area geometry forced it further down over the video.
  let reveal = if hold {
    (top - height).min(panel.max().y)
  } else {
    top - height
  };
  let connected = coverage_top(areas, pointer.y.min(panel.min().y), at_x);
  pointer.y >= reveal && pointer.y <= top && connected >= pointer.y.max(panel.max().y)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
  }

  #[test]
  fn prefers_full_headroom_and_then_the_smallest_overlap() {
    let areas = [rect(0.0, 40.0, 800.0, 560.0)];
    for (top, toolbar_y) in [
      (540.0, 540.0),
      (580.0, 566.0),
      (600.0, 566.0),
      (630.0, 566.0),
    ] {
      let video = rect(50.0, top - 360.0, 640.0, 360.0);
      assert_eq!(
        panel_frame(video, &areas, 34.0, 200.0),
        Some(rect(50.0, toolbar_y, 640.0, 34.0))
      );
    }
  }

  #[test]
  fn crops_the_toolbar_horizontally_without_a_different_vertical_placement() {
    let areas = [rect(0.0, 40.0, 1440.0, 820.0)];
    for (x, start, width) in [(-180.0, 0.0, 440.0), (1040.0, 1040.0, 400.0)] {
      let video = rect(x, 100.0, 620.0, 360.0);
      assert_eq!(
        panel_frame(video, &areas, 34.0, 200.0),
        Some(rect(start, 460.0, width, 34.0))
      );
    }
    let video = rect(-540.0, 100.0, 620.0, 360.0);
    assert_eq!(
      panel_frame(video, &areas, 34.0, 200.0),
      Some(rect(0.0, 460.0, 200.0, 34.0))
    );
    let video = rect(1360.0, 100.0, 620.0, 360.0);
    assert_eq!(
      panel_frame(video, &areas, 34.0, 200.0),
      Some(rect(1240.0, 460.0, 200.0, 34.0))
    );
    assert_eq!(
      panel_frame(rect(1500.0, 100.0, 620.0, 360.0), &areas, 34.0, 200.0),
      None
    );
  }

  #[test]
  fn joins_adjacent_displays_only_when_the_entire_bar_height_is_covered() {
    let left = rect(-800.0, 40.0, 800.0, 560.0);
    let right = rect(0.0, 40.0, 1440.0, 820.0);
    let video = rect(-540.0, 100.0, 1740.0, 360.0);
    assert_eq!(
      panel_frame(video, &[left, right], 34.0, 200.0),
      Some(rect(-540.0, 460.0, 1740.0, 34.0))
    );
    let video = rect(-540.0, 230.0, 1740.0, 360.0);
    // Full headroom takes priority over a wider bar that covers video.
    assert_eq!(
      panel_frame(video, &[left, right], 34.0, 200.0),
      Some(rect(0.0, 590.0, 1200.0, 34.0))
    );
    // Recompute from the current work areas when the right display disconnects.
    assert_eq!(
      panel_frame(video, &[left], 34.0, 200.0),
      Some(rect(-540.0, 566.0, 540.0, 34.0))
    );
    let gapped = rect(30.0, 40.0, 1440.0, 820.0);
    let video = rect(-540.0, 100.0, 1740.0, 360.0);
    assert_eq!(
      panel_frame(video, &[left, gapped], 34.0, 200.0),
      Some(rect(30.0, 460.0, 1170.0, 34.0))
    );
  }

  #[test]
  fn accepts_a_stacked_union_but_not_the_gap_between_work_areas() {
    let lower = rect(0.0, 0.0, 800.0, 300.0);
    let upper = rect(0.0, 300.0, 800.0, 500.0);
    let video = rect(50.0, 100.0, 640.0, 180.0);
    assert_eq!(
      panel_frame(video, &[lower, upper], 34.0, 200.0),
      Some(rect(50.0, 280.0, 640.0, 34.0))
    );
    let gapped = rect(0.0, 304.0, 800.0, 500.0);
    assert_eq!(
      panel_frame(video, &[lower, gapped], 34.0, 200.0),
      Some(rect(50.0, 266.0, 640.0, 34.0))
    );
  }

  #[test]
  fn reveals_from_the_visible_top_and_excludes_seams_and_hidden_edges() {
    let lower = rect(0.0, 0.0, 800.0, 300.0);
    let upper = rect(0.0, 300.0, 600.0, 400.0);
    let video = rect(50.0, 100.0, 700.0, 630.0);
    let panel = panel_frame(video, &[lower, upper], 34.0, 200.0).unwrap();
    let hover = |x, y| {
      reveal_at(
        video,
        &[lower, upper],
        panel,
        NSPoint::new(x, y),
        8.0,
        false,
      )
    };
    assert!(!hover(500.0, 298.0));
    assert!(hover(500.0, 698.0));
    assert!(!hover(650.0, 298.0));
    assert!(!hover(500.0, 728.0));
  }

  #[test]
  fn reveals_only_under_the_actual_panel_and_holds_a_reachable_path_to_it() {
    let short = rect(0.0, 0.0, 200.0, 460.0);
    let tall = rect(200.0, 0.0, 200.0, 660.0);
    let video = rect(0.0, 100.0, 400.0, 400.0);
    let above = panel_frame(video, &[short, tall], 34.0, 200.0).unwrap();
    assert!(!reveal_at(
      video,
      &[short, tall],
      above,
      NSPoint::new(100.0, 455.0),
      8.0,
      false
    ));
    assert!(reveal_at(
      video,
      &[short, tall],
      above,
      NSPoint::new(300.0, 495.0),
      8.0,
      false
    ));

    let overlap = panel_frame(video, &[short, tall], 34.0, 250.0).unwrap();
    let path = NSPoint::new(300.0, 480.0);
    assert!(!reveal_at(video, &[short, tall], overlap, path, 8.0, false));
    assert!(reveal_at(video, &[short, tall], overlap, path, 8.0, true));
  }

  #[test]
  fn an_owned_drag_keeps_a_visible_panel_when_its_display_disappears() {
    let left = rect(0.0, 0.0, 400.0, 600.0);
    let right = rect(400.0, 0.0, 420.0, 800.0);
    let previous = rect(500.0, 580.0, 300.0, 34.0);
    assert_eq!(
      keep_panel_visible(previous, &[left, right], 200.0),
      Some(previous)
    );
    assert_eq!(
      keep_panel_visible(previous, &[left], 200.0),
      Some(rect(100.0, 566.0, 300.0, 34.0))
    );
  }
}
