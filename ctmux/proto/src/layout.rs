use crate::{PaneGeometry, SplitAxis, TerminalSize, ViewLayout};

impl ViewLayout {
  /// Minimum canvas that leaves every PTY at least two columns and one row.
  #[must_use]
  pub fn minimum_size(&self) -> (u32, u32) {
    match self {
      Self::Terminal { .. } => (2, 1),
      Self::Split { axis, children } => {
        let count = u32::try_from(children.len()).unwrap_or(u32::MAX);
        let largest = children
          .iter()
          .map(Self::minimum_size)
          .fold((2, 1), |a, b| (a.0.max(b.0), a.1.max(b.1)));
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

  /// Allocates equal splits deterministically; remainder cells go to earlier children.
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
      Self::Split { axis, children } => {
        let count = u16::try_from(children.len()).map_err(|_| "too many split children")?;
        if count == 0 {
          return Err("empty split".into());
        }
        let horizontal = *axis == SplitAxis::Horizontal;
        let length = if horizontal { columns } else { rows };
        let available = length.checked_sub(count - 1).ok_or("canvas too small")?;
        let mut offset = 0;
        for (index, child) in children.iter().enumerate() {
          let extent = available / count + u16::from(index < usize::from(available % count));
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
      children: vec![
        leaf("left"),
        ViewLayout::Split {
          axis: SplitAxis::Vertical,
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
