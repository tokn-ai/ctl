use crate::{PaneGeometry, ResizeDirection, SplitAxis, TerminalSize, ViewLayout};

impl ViewLayout {
  /// Minimum canvas that leaves every PTY at least two columns and one row.
  #[must_use]
  pub fn minimum_size(&self) -> (u32, u32) {
    match self {
      Self::Terminal { .. } => (2, 1),
      Self::Split {
        axis,
        children,
        weights,
      } => {
        let count = u32::try_from(children.len()).unwrap_or(u32::MAX);
        let largest = children
          .iter()
          .map(Self::minimum_size)
          .fold((2, 1), |a, b| (a.0.max(b.0), a.1.max(b.1)));
        if !weights.is_empty() {
          let total = children
            .iter()
            .map(Self::minimum_size)
            .fold((0u32, 0u32), |a, b| {
              (a.0.saturating_add(b.0), a.1.saturating_add(b.1))
            });
          return match axis {
            SplitAxis::Horizontal => (total.0.saturating_add(count.saturating_sub(1)), largest.1),
            SplitAxis::Vertical => (largest.0, total.1.saturating_add(count.saturating_sub(1))),
          };
        }
        match axis {
          SplitAxis::Horizontal => (
            largest
              .0
              .saturating_mul(count)
              .saturating_add(count.saturating_sub(1)),
            largest.1,
          ),
          SplitAxis::Vertical => (
            largest.0,
            largest
              .1
              .saturating_mul(count)
              .saturating_add(count.saturating_sub(1)),
          ),
        }
      }
    }
  }

  /// Allocates split weights deterministically, respecting subtree minimums.
  /// Equal splits and residual cells retain their earlier-child ordering.
  ///
  /// # Errors
  /// Rejects an empty split or a canvas too small for its terminal cells and dividers.
  pub fn pane_geometry(&self, size: &TerminalSize) -> Result<Vec<PaneGeometry>, String> {
    let minimum = self.minimum_size();
    if u32::from(size.columns) < minimum.0 || u32::from(size.rows) < minimum.1 {
      return Err(format!(
        "view needs at least {} columns and {} rows",
        minimum.0, minimum.1
      ));
    }
    let mut panes = Vec::new();
    self.place(0, 0, size.columns, size.rows, &mut panes)?;
    Ok(panes)
  }

  fn place(
    &self,
    left: u16,
    top: u16,
    columns: u16,
    rows: u16,
    panes: &mut Vec<PaneGeometry>,
  ) -> Result<(), String> {
    match self {
      Self::Terminal { terminal_id } => panes.push(PaneGeometry {
        terminal_id: terminal_id.clone(),
        left,
        top,
        columns,
        rows,
      }),
      Self::Split {
        axis,
        children,
        weights,
      } => {
        let horizontal = *axis == SplitAxis::Horizontal;
        let length = if horizontal { columns } else { rows };
        let extents = split_extents(*axis, children, weights, length)?;
        let mut offset = 0;
        for (index, (child, extent)) in children.iter().zip(extents).enumerate() {
          if horizontal {
            child.place(left + offset, top, extent, rows, panes)?;
          } else {
            child.place(left, top + offset, columns, extent, panes)?;
          }
          offset += extent;
          if index + 1 < children.len() {
            offset += 1;
          }
        }
      }
    }
    Ok(())
  }

  /// Moves the nearest matching-axis divider, clamped to adjacent subtree minima.
  /// Other siblings keep their current extents; resulting weights survive canvas changes.
  ///
  /// # Errors
  /// Rejects absent targets, zero movement, invalid weights, or an insufficient canvas.
  pub fn resize_pane(
    &mut self,
    terminal_id: &str,
    direction: ResizeDirection,
    amount: u16,
    size: &TerminalSize,
  ) -> Result<bool, String> {
    if amount == 0 {
      return Err("pane resize amount must be positive".into());
    }
    if !self.contains_terminal(terminal_id) {
      return Err("resize terminal must belong to the attached view".into());
    }
    self.pane_geometry(size)?;
    Ok(
      self
        .move_divider(terminal_id, direction, amount, size.columns, size.rows)?
        .unwrap_or(false),
    )
  }

  /// Moves an exact divider to an absolute canvas gap-cell coordinate.
  ///
  /// # Errors
  /// Rejects invalid paths/boundaries or an insufficient canvas before mutation.
  pub fn resize_divider(
    &mut self,
    split_path: &[u16],
    boundary: u16,
    position: u16,
    size: &TerminalSize,
  ) -> Result<bool, String> {
    if split_path.len() > 16 {
      return Err("divider split path exceeds 16 levels".into());
    }
    self.pane_geometry(size)?;
    self.place_divider(
      split_path,
      usize::from(boundary),
      position,
      LayoutBounds {
        left: 0,
        top: 0,
        columns: size.columns,
        rows: size.rows,
      },
    )
  }

  fn place_divider(
    &mut self,
    split_path: &[u16],
    boundary: usize,
    position: u16,
    bounds: LayoutBounds,
  ) -> Result<bool, String> {
    let Self::Split {
      axis,
      children,
      weights,
    } = self
    else {
      return Err("divider path must target a split".into());
    };
    let horizontal = *axis == SplitAxis::Horizontal;
    let extents = split_extents(
      *axis,
      children,
      weights,
      if horizontal {
        bounds.columns
      } else {
        bounds.rows
      },
    )?;
    if let Some((&index, remaining)) = split_path.split_first() {
      let index = usize::from(index);
      let extent = *extents
        .get(index)
        .ok_or("divider split path child is absent")?;
      let offset = extents[..index].iter().sum::<u16>()
        + u16::try_from(index).map_err(|_| "invalid divider path")?;
      let child_bounds = if horizontal {
        LayoutBounds {
          left: bounds.left + offset,
          columns: extent,
          ..bounds
        }
      } else {
        LayoutBounds {
          top: bounds.top + offset,
          rows: extent,
          ..bounds
        }
      };
      return children[index].place_divider(remaining, boundary, position, child_bounds);
    }
    if boundary + 1 >= children.len() {
      return Err("divider boundary is absent from split".into());
    }
    let current = (if horizontal { bounds.left } else { bounds.top })
      + extents[..=boundary].iter().sum::<u16>()
      + u16::try_from(boundary).map_err(|_| "invalid divider boundary")?;
    move_boundary(
      *axis,
      children,
      weights,
      extents,
      boundary,
      i64::from(position) - i64::from(current),
    )
  }

  /// Removes fields unavailable to contracts before proportional pane sizing.
  pub fn clear_weights(&mut self) {
    if let Self::Split {
      children, weights, ..
    } = self
    {
      weights.clear();
      for child in children {
        child.clear_weights();
      }
    }
  }

  #[must_use]
  pub fn has_weights(&self) -> bool {
    match self {
      Self::Terminal { .. } => false,
      Self::Split {
        children, weights, ..
      } => !weights.is_empty() || children.iter().any(Self::has_weights),
    }
  }

  fn contains_terminal(&self, id: &str) -> bool {
    match self {
      Self::Terminal { terminal_id } => terminal_id == id,
      Self::Split { children, .. } => children.iter().any(|child| child.contains_terminal(id)),
    }
  }

  fn move_divider(
    &mut self,
    terminal_id: &str,
    direction: ResizeDirection,
    amount: u16,
    columns: u16,
    rows: u16,
  ) -> Result<Option<bool>, String> {
    let Self::Split {
      axis,
      children,
      weights,
    } = self
    else {
      return Ok(None);
    };
    let selected = children
      .iter()
      .position(|child| child.contains_terminal(terminal_id))
      .ok_or("resize terminal is absent from split")?;
    let horizontal = *axis == SplitAxis::Horizontal;
    let length = if horizontal { columns } else { rows };
    let extents = split_extents(*axis, children, weights, length)?;
    let child_columns = if horizontal {
      extents[selected]
    } else {
      columns
    };
    let child_rows = if horizontal { rows } else { extents[selected] };
    if let Some(changed) =
      children[selected].move_divider(terminal_id, direction, amount, child_columns, child_rows)?
    {
      return Ok(Some(changed));
    }
    let requested_horizontal = matches!(direction, ResizeDirection::Left | ResizeDirection::Right);
    if horizontal != requested_horizontal || children.len() < 2 {
      return Ok(None);
    }
    let boundary = selected.min(children.len() - 2);
    let delta = if matches!(direction, ResizeDirection::Right | ResizeDirection::Down) {
      i64::from(amount)
    } else {
      -i64::from(amount)
    };
    move_boundary(*axis, children, weights, extents, boundary, delta).map(Some)
  }
}

#[derive(Clone, Copy)]
struct LayoutBounds {
  left: u16,
  top: u16,
  columns: u16,
  rows: u16,
}

fn move_boundary(
  axis: SplitAxis,
  children: &[ViewLayout],
  weights: &mut Vec<u32>,
  mut extents: Vec<u16>,
  boundary: usize,
  delta: i64,
) -> Result<bool, String> {
  let minimum = |child: &ViewLayout| {
    let (columns, rows) = child.minimum_size();
    if axis == SplitAxis::Horizontal {
      columns
    } else {
      rows
    }
  };
  let left = i64::from(extents[boundary]);
  let right = i64::from(extents[boundary + 1]);
  let delta = delta.clamp(
    i64::from(minimum(&children[boundary])) - left,
    right - i64::from(minimum(&children[boundary + 1])),
  );
  if delta == 0 {
    return Ok(false);
  }
  extents[boundary] = u16::try_from(left + delta).map_err(|_| "invalid pane extent")?;
  extents[boundary + 1] = u16::try_from(right - delta).map_err(|_| "invalid pane extent")?;
  *weights = extents.into_iter().map(u32::from).collect();
  Ok(true)
}

fn split_extents(
  axis: SplitAxis,
  children: &[ViewLayout],
  weights: &[u32],
  length: u16,
) -> Result<Vec<u16>, String> {
  let count = u16::try_from(children.len()).map_err(|_| "too many split children")?;
  if count == 0 {
    return Err("empty split".into());
  }
  let available = length.checked_sub(count - 1).ok_or("canvas too small")?;
  if weights.is_empty() {
    return Ok(
      (0..count)
        .map(|index| available / count + u16::from(index < available % count))
        .collect(),
    );
  }
  if weights.len() != children.len() || weights.contains(&0) {
    return Err("split weights must have one positive value per child".into());
  }
  let minima: Vec<_> = children
    .iter()
    .map(|child| {
      let (columns, rows) = child.minimum_size();
      u64::from(if axis == SplitAxis::Horizontal {
        columns
      } else {
        rows
      })
    })
    .collect();
  if minima.iter().sum::<u64>() > u64::from(available) {
    return Err("canvas too small".into());
  }
  let mut extents = vec![0u64; children.len()];
  let mut active: Vec<_> = (0..children.len()).collect();
  let mut remaining = u64::from(available);
  while !active.is_empty() {
    let total = active
      .iter()
      .map(|&index| u64::from(weights[index]))
      .sum::<u64>();
    let clamped: Vec<_> = active
      .iter()
      .copied()
      .filter(|&index| remaining * u64::from(weights[index]) < minima[index] * total)
      .collect();
    if clamped.is_empty() {
      for &index in &active {
        extents[index] = remaining * u64::from(weights[index]) / total;
      }
      let residual = remaining - active.iter().map(|&index| extents[index]).sum::<u64>();
      for &index in active
        .iter()
        .take(usize::try_from(residual).expect("residual bounded by child count"))
      {
        extents[index] += 1;
      }
      break;
    }
    for &index in &clamped {
      extents[index] = minima[index];
      remaining -= minima[index];
    }
    active.retain(|index| !clamped.contains(index));
  }
  extents
    .into_iter()
    .map(|extent| u16::try_from(extent).map_err(|_| "invalid pane extent".into()))
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;
  fn leaf(id: &str) -> ViewLayout {
    ViewLayout::Terminal {
      terminal_id: id.into(),
    }
  }

  #[test]
  fn nested_splits_tile_the_canvas_with_cell_dividers() {
    let layout = ViewLayout::Split {
      axis: SplitAxis::Horizontal,
      weights: Vec::new(),
      children: vec![
        leaf("left"),
        ViewLayout::Split {
          axis: SplitAxis::Vertical,
          weights: Vec::new(),
          children: vec![leaf("top"), leaf("bottom")],
        },
      ],
    };
    let size = TerminalSize {
      columns: 100,
      rows: 40,
      pixel_width: 0,
      pixel_height: 0,
    };
    let panes = layout.pane_geometry(&size).unwrap();
    assert_eq!((panes[0].columns, panes[0].rows), (50, 40));
    assert_eq!(
      (panes[1].left, panes[1].top, panes[1].columns, panes[1].rows),
      (51, 0, 49, 20)
    );
    assert_eq!(
      (panes[2].left, panes[2].top, panes[2].columns, panes[2].rows),
      (51, 21, 49, 19)
    );
    assert_eq!(panes[1].rows + 1 + panes[2].rows, panes[0].rows);
    assert_eq!(panes[0].columns + 1 + panes[1].columns, size.columns);
  }

  #[test]
  fn insufficient_canvas_is_rejected() {
    let layout = ViewLayout::Split {
      axis: SplitAxis::Horizontal,
      weights: Vec::new(),
      children: vec![leaf("a"), leaf("b")],
    };
    assert!(
      layout
        .pane_geometry(&TerminalSize {
          columns: 4,
          ..TerminalSize::default()
        })
        .is_err()
    );
  }

  fn horizontal(ids: &[&str], weights: &[u32]) -> ViewLayout {
    ViewLayout::Split {
      axis: SplitAxis::Horizontal,
      children: ids.iter().map(|id| leaf(id)).collect(),
      weights: weights.to_vec(),
    }
  }

  fn size(columns: u16, rows: u16) -> TerminalSize {
    TerminalSize {
      columns,
      rows,
      pixel_width: 0,
      pixel_height: 0,
    }
  }

  #[test]
  fn weighted_splits_allocate_cells_and_clamp_extreme_ratios() {
    let layout = horizontal(&["a", "b"], &[1, 3]);
    let panes = layout.pane_geometry(&size(100, 20)).unwrap();
    assert_eq!((panes[0].columns, panes[1].columns), (25, 74));
    let extreme = horizontal(&["a", "b"], &[1, u32::MAX]);
    let panes = extreme.pane_geometry(&size(100, 20)).unwrap();
    assert_eq!((panes[0].columns, panes[1].columns), (2, 97));
  }

  #[test]
  fn weighted_minimum_sums_subtree_minima_and_tiles_small_canvases() {
    let layout = ViewLayout::Split {
      axis: SplitAxis::Horizontal,
      children: vec![horizontal(&["a", "b"], &[]), leaf("c"), leaf("d")],
      weights: vec![1, 8, 1],
    };
    assert_eq!(layout.minimum_size(), (11, 1));
    let panes = layout.pane_geometry(&size(20, 10)).unwrap();
    assert_eq!(
      panes.iter().map(|pane| pane.columns).collect::<Vec<_>>(),
      [2, 2, 11, 2]
    );
    assert!(layout.pane_geometry(&size(10, 10)).is_err());
  }

  #[test]
  fn weighted_allocation_is_deterministic_without_overflow_or_missing_cells() {
    for weights in [[1, 1, 1], [1, u32::MAX, 1], [u32::MAX; 3], [2, 7, 3]] {
      let layout = horizontal(&["a", "b", "c"], &weights);
      for columns in 8..120 {
        let canvas = size(columns, 10);
        let panes = layout.pane_geometry(&canvas).unwrap();
        assert_eq!(panes, layout.pane_geometry(&canvas).unwrap());
        assert!(panes.iter().all(|pane| pane.columns >= 2));
        assert_eq!(
          panes.iter().map(|pane| pane.columns).sum::<u16>() + 2,
          columns
        );
        assert!(
          panes
            .windows(2)
            .all(|pair| pair[0].left + pair[0].columns + 1 == pair[1].left)
        );
      }
    }
    for weights in [vec![0, 1], vec![1], vec![1, 2, 3]] {
      assert!(
        horizontal(&["a", "b"], &weights)
          .pane_geometry(&size(80, 24))
          .is_err()
      );
    }
  }

  #[test]
  fn pane_resize_moves_the_tmux_divider_without_changing_other_siblings() {
    let canvas = size(31, 10);
    let mut layout = horizontal(&["a", "b", "c"], &[]);
    assert!(
      layout
        .resize_pane("b", ResizeDirection::Right, 3, &canvas)
        .unwrap()
    );
    let extents = |layout: &ViewLayout| {
      layout
        .pane_geometry(&canvas)
        .unwrap()
        .iter()
        .map(|pane| pane.columns)
        .collect::<Vec<_>>()
    };
    assert_eq!(extents(&layout), [10, 13, 6]);
    assert!(
      layout
        .resize_pane("c", ResizeDirection::Left, 3, &canvas)
        .unwrap()
    );
    assert_eq!(extents(&layout), [10, 10, 9]);
    assert!(
      layout
        .resize_pane("a", ResizeDirection::Left, u16::MAX, &canvas)
        .unwrap()
    );
    assert_eq!(extents(&layout), [2, 18, 9]);
    let unchanged = layout.clone();
    assert!(
      !layout
        .resize_pane("a", ResizeDirection::Left, 1, &canvas)
        .unwrap()
    );
    assert_eq!(layout, unchanged);
    assert!(
      layout
        .resize_pane("absent", ResizeDirection::Right, 1, &canvas)
        .is_err()
    );
    assert!(
      layout
        .resize_pane("a", ResizeDirection::Right, 0, &canvas)
        .is_err()
    );
  }

  #[test]
  fn pane_resize_selects_the_nearest_matching_ancestor_and_retains_nested_ratios() {
    let canvas = size(81, 25);
    let mut layout = ViewLayout::Split {
      axis: SplitAxis::Horizontal,
      children: vec![
        ViewLayout::Split {
          axis: SplitAxis::Vertical,
          children: vec![horizontal(&["a", "b"], &[]), leaf("c")],
          weights: Vec::new(),
        },
        leaf("d"),
      ],
      weights: Vec::new(),
    };
    assert!(
      layout
        .resize_pane("a", ResizeDirection::Right, 5, &canvas)
        .unwrap()
    );
    let panes = layout.pane_geometry(&canvas).unwrap();
    assert_eq!(
      (
        panes[0].columns,
        panes[1].columns,
        panes[2].columns,
        panes[3].columns
      ),
      (25, 14, 40, 40)
    );
    assert!(
      layout
        .resize_pane("a", ResizeDirection::Down, 3, &canvas)
        .unwrap()
    );
    let panes = layout.pane_geometry(&canvas).unwrap();
    assert_eq!((panes[0].rows, panes[1].rows, panes[2].rows), (15, 15, 9));
    assert_eq!((panes[0].columns, panes[1].columns), (25, 14));
    let wide = layout.pane_geometry(&size(161, 25)).unwrap();
    assert_eq!((wide[0].columns, wide[1].columns), (51, 28));
  }

  #[test]
  fn weights_round_trip_and_downgrade_without_changing_terminal_membership() {
    let mut layout = horizontal(&["a", "b"], &[3, 1]);
    let value = serde_json::to_value(&layout).unwrap();
    assert_eq!(value["weights"], serde_json::json!([3, 1]));
    assert_eq!(layout, serde_json::from_value(value).unwrap());
    layout.clear_weights();
    assert!(!layout.has_weights());
    assert!(
      serde_json::to_value(&layout)
        .unwrap()
        .get("weights")
        .is_none()
    );
    assert_eq!(layout, horizontal(&["a", "b"], &[]));
  }

  #[test]
  fn exact_divider_targets_outer_parallel_splits_and_nested_canvas_offsets() {
    let canvas = size(101, 20);
    let mut layout = ViewLayout::Split {
      axis: SplitAxis::Horizontal,
      children: vec![horizontal(&["a", "b"], &[]), horizontal(&["c", "d"], &[])],
      weights: Vec::new(),
    };
    assert!(layout.resize_divider(&[1], 0, 80, &canvas).unwrap());
    let panes = layout.pane_geometry(&canvas).unwrap();
    assert_eq!(
      panes.iter().map(|pane| pane.columns).collect::<Vec<_>>(),
      [25, 24, 29, 20]
    );
    assert_eq!(panes[3].left, 81);
    assert!(layout.resize_divider(&[], 0, 60, &canvas).unwrap());
    let panes = layout.pane_geometry(&canvas).unwrap();
    assert_eq!(
      panes.iter().map(|pane| pane.columns).collect::<Vec<_>>(),
      [30, 29, 24, 15]
    );
    assert_eq!(panes[2].left, 61);
    assert!(matches!(&layout, ViewLayout::Split { children, .. }
      if matches!(&children[1], ViewLayout::Split { weights, .. } if weights == &[29, 20])));
  }

  #[test]
  fn absolute_divider_targets_coalesce_and_recover_from_clamping_without_drift() {
    let canvas = size(101, 20);
    let original = ViewLayout::Split {
      axis: SplitAxis::Horizontal,
      children: vec![horizontal(&["a", "b"], &[]), horizontal(&["c", "d"], &[])],
      weights: Vec::new(),
    };
    let mut sequential = original.clone();
    for position in [51, 55, 60, 65] {
      sequential
        .resize_divider(&[], 0, position, &canvas)
        .unwrap();
    }
    let mut coalesced = original;
    coalesced.resize_divider(&[], 0, 65, &canvas).unwrap();
    assert_eq!(sequential, coalesced);
    sequential
      .resize_divider(&[], 0, u16::MAX, &canvas)
      .unwrap();
    let panes = sequential.pane_geometry(&canvas).unwrap();
    assert_eq!(panes[2].columns + panes[3].columns + 1, 5);
    sequential.resize_divider(&[], 0, 65, &canvas).unwrap();
    assert_eq!(sequential, coalesced);
    assert!(!sequential.resize_divider(&[], 0, 65, &canvas).unwrap());
    assert_eq!(sequential, coalesced);
    for (path, boundary) in [
      (vec![2], 0),
      (vec![0, 0], 0),
      (vec![0; 17], 0),
      (vec![], 1),
      (vec![], u16::MAX),
    ] {
      assert!(
        sequential
          .resize_divider(&path, boundary, 70, &canvas)
          .is_err()
      );
      assert_eq!(sequential, coalesced);
    }
  }

  #[test]
  fn nested_vertical_divider_uses_its_absolute_canvas_row() {
    let canvas = size(80, 41);
    let mut layout = ViewLayout::Split {
      axis: SplitAxis::Vertical,
      children: vec![
        leaf("above"),
        ViewLayout::Split {
          axis: SplitAxis::Vertical,
          children: vec![leaf("middle"), leaf("below")],
          weights: Vec::new(),
        },
      ],
      weights: Vec::new(),
    };
    layout.resize_divider(&[1], 0, 35, &canvas).unwrap();
    let panes = layout.pane_geometry(&canvas).unwrap();
    assert_eq!(
      (
        panes[0].rows,
        panes[1].top,
        panes[1].rows,
        panes[2].top,
        panes[2].rows
      ),
      (20, 21, 14, 36, 5)
    );
  }

  #[test]
  fn legacy_tabs_decode_as_nested_splits_without_losing_terminals() {
    let layout: ViewLayout = serde_json::from_str(
      r#"{
      "kind": "tabs", "children": [
        {"kind": "terminal", "terminal_id": "a"},
        {"kind": "tabs", "children": [
          {"kind": "terminal", "terminal_id": "b"},
          {"kind": "terminal", "terminal_id": "c"}
        ]}
      ]
    }"#,
    )
    .unwrap();
    let panes = layout.pane_geometry(&TerminalSize::default()).unwrap();
    assert_eq!(
      panes
        .iter()
        .map(|pane| pane.terminal_id.as_str())
        .collect::<Vec<_>>(),
      ["a", "b", "c"]
    );
    assert!(
      panes
        .windows(2)
        .all(|pair| pair[0].left + pair[0].columns < pair[1].left)
    );
    let serialized = serde_json::to_string(&layout).unwrap();
    assert!(!serialized.contains("tabs"));
  }
}
