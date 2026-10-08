use ctmux_proto::{
  DividerResize, PaneGeometry, PaneResizeOutcome, SplitAxis, ViewInfo, ViewLayout,
};

/// An exact reserved gap in the daemon's split tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Divider {
  pub split_path: Vec<u16>,
  pub boundary: u16,
  pub axis: SplitAxis,
  pub position: u16,
}

impl Divider {
  /// Use the published rectangles rather than recreating the weight allocator.
  /// Parent gaps win at junctions, including parallel nested split boundaries.
  pub fn hit(view: &ViewInfo, point: (u16, u16)) -> Option<Self> {
    if view.zoomed_terminal_id.is_some()
      || view
        .panes
        .iter()
        .any(|pane| Bounds::pane(pane).is_some_and(|bounds| bounds.contains(point)))
    {
      return None;
    }
    Self::find(&view.layout, &view.panes, point, &mut Vec::new())
  }

  fn find(
    layout: &ViewLayout,
    panes: &[PaneGeometry],
    point: (u16, u16),
    path: &mut Vec<u16>,
  ) -> Option<Self> {
    let ViewLayout::Split { axis, children, .. } = layout else {
      return None;
    };
    let bounds = Bounds::layout(layout, panes)?;
    if !bounds.contains(point) {
      return None;
    }
    let child_bounds: Vec<_> = children
      .iter()
      .map(|child| Bounds::layout(child, panes))
      .collect::<Option<_>>()?;
    for (boundary, pair) in child_bounds.windows(2).enumerate() {
      let (position, following, pointer) = match axis {
        SplitAxis::Horizontal => (pair[0].right, pair[1].left, point.0),
        SplitAxis::Vertical => (pair[0].bottom, pair[1].top, point.1),
      };
      if position.checked_add(1) == Some(following) && pointer == position {
        return Some(Self {
          split_path: path.clone(),
          boundary: u16::try_from(boundary).ok()?,
          axis: *axis,
          position,
        });
      }
    }
    for (index, child) in children.iter().enumerate() {
      path.push(u16::try_from(index).ok()?);
      let found = Self::find(child, panes, point, path);
      path.pop();
      if found.is_some() {
        return found;
      }
    }
    None
  }
}

#[derive(Clone, Copy)]
struct Bounds {
  left: u16,
  top: u16,
  right: u16,
  bottom: u16,
}

impl Bounds {
  fn pane(pane: &PaneGeometry) -> Option<Self> {
    Some(Self {
      left: pane.left,
      top: pane.top,
      right: pane.left.checked_add(pane.columns)?,
      bottom: pane.top.checked_add(pane.rows)?,
    })
  }

  fn layout(layout: &ViewLayout, panes: &[PaneGeometry]) -> Option<Self> {
    match layout {
      ViewLayout::Terminal { terminal_id } => {
        Self::pane(panes.iter().find(|pane| &pane.terminal_id == terminal_id)?)
      }
      ViewLayout::Split { children, .. } => {
        let mut children = children.iter();
        let mut bounds = Self::layout(children.next()?, panes)?;
        for child in children {
          let child = Self::layout(child, panes)?;
          bounds.left = bounds.left.min(child.left);
          bounds.top = bounds.top.min(child.top);
          bounds.right = bounds.right.max(child.right);
          bounds.bottom = bounds.bottom.max(child.bottom);
        }
        Some(bounds)
      }
    }
  }

  fn contains(self, point: (u16, u16)) -> bool {
    (self.left..self.right).contains(&point.0) && (self.top..self.bottom).contains(&point.1)
  }
}

/// A captured gesture with one correlated command in flight at a time.
pub(crate) struct Drag {
  pub owner: String,
  divider: Divider,
  confirmed: ViewInfo,
  previous: Option<ViewInfo>,
  offset: (u16, u16),
  desired: u16,
  last_sent: u16,
  released: bool,
  pending: Option<Pending>,
}

struct Pending {
  request_id: String,
  predicted: Option<ViewInfo>,
}

impl Drag {
  pub fn new(view: &ViewInfo, divider: Divider, offset: (u16, u16), owner: String) -> Self {
    let position = divider.position;
    Self {
      owner,
      divider,
      confirmed: view.clone(),
      previous: None,
      offset,
      desired: position,
      last_sent: position,
      released: false,
      pending: None,
    }
  }

  /// Preserve the pointer's original canvas mapping as the panes reflow.
  pub fn viewport_offset(&self) -> (u16, u16) {
    self.offset
  }

  pub fn released(&self) -> bool {
    self.released
  }

  pub fn move_to(&mut self, point: (u16, u16), released: bool) {
    if self.released {
      return;
    }
    self.desired = match self.divider.axis {
      SplitAxis::Horizontal => point.0.saturating_add(self.offset.0),
      SplitAxis::Vertical => point.1.saturating_add(self.offset.1),
    };
    self.released = released;
  }

  pub fn next(&mut self, request_id: String) -> Option<DividerResize> {
    if self.pending.is_some() || self.desired == self.last_sent {
      return None;
    }
    let request = DividerResize {
      view_id: self.confirmed.view_id.clone(),
      expected_revision: self.confirmed.revision,
      split_path: self.divider.split_path.clone(),
      boundary: self.divider.boundary,
      position: self.desired,
    };
    let predicted = self.predict(&request);
    self.pending = Some(Pending {
      request_id,
      predicted,
    });
    // Store the raw target, including positions clamped by the daemon. Using
    // its returned gap instead would repeatedly send the same extreme target.
    self.last_sent = self.desired;
    Some(request)
  }

  fn predict(&self, request: &DividerResize) -> Option<ViewInfo> {
    let mut view = self.confirmed.clone();
    let changed = view
      .layout
      .resize_divider(
        &request.split_path,
        request.boundary,
        request.position,
        &view.canvas_size,
      )
      .ok()?;
    if changed {
      view.revision = view.revision.checked_add(1)?;
      view.panes = view.layout.pane_geometry(&view.canvas_size).ok()?;
      view.zoomed_terminal_id = None;
    }
    Some(view)
  }

  /// A broadcast can precede its command acknowledgement, but unrelated view
  /// changes must never authorize another request against the captured path.
  pub fn accepts_view(&self, view: &ViewInfo) -> bool {
    same_geometry(&self.confirmed, view)
      || self
        .previous
        .as_ref()
        .is_some_and(|previous| same_geometry(previous, view))
      || self
        .pending
        .as_ref()
        .and_then(|pending| pending.predicted.as_ref())
        .is_some_and(|predicted| same_geometry(predicted, view))
  }

  pub fn acknowledge(
    &mut self,
    request_id: &str,
    outcome: &PaneResizeOutcome,
  ) -> Result<bool, String> {
    if self.pending_request_id() != Some(request_id) {
      return Ok(false);
    }
    let pending = self.pending.take().expect("matching pending request");
    match outcome {
      PaneResizeOutcome::Applied { view } => {
        if !pending
          .predicted
          .as_ref()
          .is_some_and(|predicted| same_geometry(predicted, view))
        {
          return Err("View changed during divider drag".into());
        }
        self.previous = Some(std::mem::replace(&mut self.confirmed, *view.clone()));
        Ok(true)
      }
      PaneResizeOutcome::Rejected { message, .. } => Err(message.clone()),
    }
  }

  pub fn pending_request_id(&self) -> Option<&str> {
    self
      .pending
      .as_ref()
      .map(|pending| pending.request_id.as_str())
  }

  pub fn finished(&self) -> bool {
    self.released && self.pending.is_none() && self.desired == self.last_sent
  }
}

fn same_geometry(first: &ViewInfo, second: &ViewInfo) -> bool {
  first.view_id == second.view_id
    && first.session_id == second.session_id
    && first.revision == second.revision
    && first.canvas_size == second.canvas_size
    && first.zoomed_terminal_id == second.zoomed_terminal_id
    && first.layout == second.layout
    && first.panes == second.panes
}

#[cfg(test)]
mod tests {
  use super::*;
  use ctmux_proto::{ErrorCode, TerminalSize};

  fn terminal(id: &str) -> ViewLayout {
    ViewLayout::Terminal {
      terminal_id: id.into(),
    }
  }

  fn split(axis: SplitAxis, children: Vec<ViewLayout>) -> ViewLayout {
    ViewLayout::Split {
      axis,
      children,
      weights: Vec::new(),
    }
  }

  fn view(layout: ViewLayout) -> ViewInfo {
    let canvas_size = TerminalSize {
      columns: 31,
      rows: 15,
      ..TerminalSize::default()
    };
    ViewInfo {
      view_id: "view".into(),
      session_id: "session".into(),
      session_name: "shell".into(),
      revision: 7,
      panes: layout.pane_geometry(&canvas_size).unwrap(),
      canvas_size,
      zoomed_terminal_id: None,
      layout,
      terminals: Vec::new(),
    }
  }

  fn two_panes(axis: SplitAxis) -> ViewInfo {
    view(split(axis, vec![terminal("a"), terminal("b")]))
  }

  fn drag(view: &ViewInfo, point: (u16, u16), offset: (u16, u16)) -> Drag {
    Drag::new(view, Divider::hit(view, point).unwrap(), offset, "a".into())
  }

  fn apply(view: &ViewInfo, request: &DividerResize) -> ViewInfo {
    let mut view = view.clone();
    let changed = view
      .layout
      .resize_divider(
        &request.split_path,
        request.boundary,
        request.position,
        &view.canvas_size,
      )
      .unwrap();
    if changed {
      view.revision += 1;
      view.panes = view.layout.pane_geometry(&view.canvas_size).unwrap();
    }
    view
  }

  fn applied(view: ViewInfo) -> PaneResizeOutcome {
    PaneResizeOutcome::Applied {
      view: Box::new(view),
    }
  }

  #[test]
  fn hit_uses_authoritative_rectangles_and_excludes_panes_and_canvas_edges() {
    let mut view = two_panes(SplitAxis::Horizontal);
    // A published unequal grid is authoritative even if the accompanying
    // weights are unavailable to a viewer using an older contract.
    view.panes[0].columns = 9;
    view.panes[1].left = 10;
    view.panes[1].columns = 21;
    let divider = Divider::hit(&view, (9, 8)).unwrap();
    assert_eq!(divider.position, 9);
    assert_eq!(divider.split_path, Vec::<u16>::new());
    assert!(Divider::hit(&view, (8, 8)).is_none());
    assert!(Divider::hit(&view, (10, 8)).is_none());
    assert!(Divider::hit(&view, (9, 15)).is_none());
    assert!(Divider::hit(&view, (31, 8)).is_none());
    view.zoomed_terminal_id = Some("a".into());
    assert!(Divider::hit(&view, (9, 8)).is_none());
  }

  #[test]
  fn parent_divider_wins_junctions_and_nested_axes_remain_exact() {
    let view = view(split(
      SplitAxis::Horizontal,
      vec![
        split(SplitAxis::Vertical, vec![terminal("a"), terminal("b")]),
        split(SplitAxis::Vertical, vec![terminal("c"), terminal("d")]),
      ],
    ));
    let parent = Divider::hit(&view, (15, 7)).unwrap();
    assert_eq!(parent.split_path, Vec::<u16>::new());
    assert_eq!(parent.axis, SplitAxis::Horizontal);
    let left = Divider::hit(&view, (3, 7)).unwrap();
    assert_eq!(left.split_path, vec![0]);
    assert_eq!(left.axis, SplitAxis::Vertical);
    assert_eq!(Divider::hit(&view, (24, 7)).unwrap().split_path, vec![1]);
  }

  #[test]
  fn parallel_nested_splits_distinguish_outer_and_inner_gaps() {
    let view = view(split(
      SplitAxis::Horizontal,
      vec![
        split(SplitAxis::Horizontal, vec![terminal("a"), terminal("b")]),
        terminal("c"),
      ],
    ));
    let outer_position = view.panes[2].left - 1;
    assert_eq!(
      Divider::hit(&view, (outer_position, 2)).unwrap().split_path,
      Vec::<u16>::new()
    );
    let inner_position = view.panes[0].columns;
    let inner = Divider::hit(&view, (inner_position, 2)).unwrap();
    assert_eq!(inner.split_path, vec![0]);
    assert_eq!(inner.boundary, 0);
  }

  #[test]
  fn one_request_in_flight_coalesces_motion_and_flushes_mouseup() {
    let view = two_panes(SplitAxis::Horizontal);
    let mut drag = drag(&view, (15, 3), (0, 0));
    assert_eq!(drag.owner, "a");
    assert!(drag.next("unchanged".into()).is_none());
    drag.move_to((18, 3), false);
    let first = drag.next("first".into()).unwrap();
    drag.move_to((19, 3), false);
    drag.move_to((22, 3), true);
    assert!(drag.released());
    assert!(drag.next("blocked".into()).is_none());
    assert!(!drag.finished());
    let confirmed = apply(&view, &first);
    assert!(
      drag
        .acknowledge("first", &applied(confirmed.clone()))
        .unwrap()
    );
    let final_request = drag.next("last".into()).unwrap();
    assert_eq!(final_request.position, 22);
    assert_eq!(final_request.expected_revision, confirmed.revision);
    assert!(!drag.finished());
    let final_view = apply(&confirmed, &final_request);
    assert!(drag.acknowledge("last", &applied(final_view)).unwrap());
    assert!(drag.finished());
    assert!(drag.next("duplicate".into()).is_none());
  }

  #[test]
  fn clamped_targets_are_not_resent_and_reverse_motion_is_absolute() {
    let view = two_panes(SplitAxis::Horizontal);
    let mut drag = drag(&view, (15, 3), (0, 0));
    drag.move_to((0, 3), false);
    let first = drag.next("minimum".into()).unwrap();
    let minimum = apply(&view, &first);
    assert_eq!(minimum.panes[0].columns, 2);
    drag
      .acknowledge("minimum", &applied(minimum.clone()))
      .unwrap();
    drag.move_to((0, 3), false);
    assert!(drag.next("same".into()).is_none());
    drag.move_to((15, 3), true);
    let reverse = drag.next("reverse".into()).unwrap();
    let restored = apply(&minimum, &reverse);
    assert_eq!(restored.panes, view.panes);
    drag.acknowledge("reverse", &applied(restored)).unwrap();
    assert!(drag.finished());
  }

  #[test]
  fn no_op_clamp_keeps_revision_and_a_click_finishes_without_a_request() {
    let mut view = two_panes(SplitAxis::Vertical);
    let request = DividerResize {
      view_id: view.view_id.clone(),
      expected_revision: view.revision,
      split_path: vec![],
      boundary: 0,
      position: 0,
    };
    view = apply(&view, &request);
    let mut click = drag(&view, (3, 1), (0, 0));
    click.move_to((3, 1), true);
    assert!(click.finished());
    let mut drag = drag(&view, (3, 1), (0, 0));
    drag.move_to((3, 0), true);
    let request = drag.next("clamped".into()).unwrap();
    let unchanged = apply(&view, &request);
    assert_eq!(unchanged.revision, view.revision);
    drag.acknowledge("clamped", &applied(unchanged)).unwrap();
    assert!(drag.finished());
  }

  #[test]
  fn frozen_offsets_map_both_axes_without_accumulating_geometry_changes() {
    for axis in [SplitAxis::Horizontal, SplitAxis::Vertical] {
      let view = two_panes(axis);
      let point = if axis == SplitAxis::Horizontal {
        (15, 5)
      } else {
        (5, 7)
      };
      let mut drag = drag(&view, point, (4, 3));
      assert_eq!(drag.viewport_offset(), (4, 3));
      drag.move_to((9, 6), false);
      let request = drag.next("offset".into()).unwrap();
      assert_eq!(
        request.position,
        if axis == SplitAxis::Horizontal { 13 } else { 9 }
      );
      let confirmed = apply(&view, &request);
      drag.acknowledge("offset", &applied(confirmed)).unwrap();
      drag.move_to((9, 6), true);
      assert!(drag.next("same-pointer".into()).is_none());
      assert!(drag.finished());
    }
  }

  #[test]
  fn broadcasts_can_precede_correlated_acknowledgements() {
    let view = two_panes(SplitAxis::Horizontal);
    let mut drag = drag(&view, (15, 3), (0, 0));
    drag.move_to((20, 3), false);
    let request = drag.next("expected".into()).unwrap();
    let predicted = apply(&view, &request);
    assert!(drag.accepts_view(&view));
    assert!(drag.accepts_view(&predicted));
    assert!(
      !drag
        .acknowledge("other", &applied(predicted.clone()))
        .unwrap()
    );
    assert_eq!(drag.pending_request_id(), Some("expected"));
    assert!(drag.next("blocked".into()).is_none());
    drag
      .acknowledge("expected", &applied(predicted.clone()))
      .unwrap();
    assert!(drag.accepts_view(&predicted));
    assert!(drag.accepts_view(&view));
  }

  #[test]
  fn unrelated_view_changes_cancel_while_metadata_changes_are_accepted() {
    let view = two_panes(SplitAxis::Horizontal);
    let drag = drag(&view, (15, 3), (0, 0));
    let mut renamed = view.clone();
    renamed.session_name = "new name".into();
    assert!(drag.accepts_view(&renamed));
    let mut changes = vec![view.clone(); 7];
    changes[0].view_id = "other".into();
    changes[1].session_id = "other".into();
    changes[2].revision += 1;
    changes[3].canvas_size.columns += 1;
    changes[4].zoomed_terminal_id = Some("a".into());
    changes[5].panes[0].columns -= 1;
    changes[6].layout = split(SplitAxis::Vertical, vec![terminal("a"), terminal("b")]);
    for changed in changes {
      assert!(!drag.accepts_view(&changed));
    }
  }

  #[test]
  fn acknowledgements_reject_unpredicted_geometry_and_report_denial() {
    let view = two_panes(SplitAxis::Horizontal);
    let mut drag = drag(&view, (15, 3), (0, 0));
    drag.move_to((20, 3), false);
    let request = drag.next("wrong".into()).unwrap();
    let mut wrong = apply(&view, &request);
    wrong.panes[0].rows -= 1;
    assert!(drag.acknowledge("wrong", &applied(wrong)).is_err());
    drag.move_to((21, 3), false);
    drag.next("denied".into()).unwrap();
    let denied = PaneResizeOutcome::Rejected {
      code: ErrorCode::LayoutLeaseRequired,
      message: "Another client holds resize control".into(),
    };
    assert_eq!(
      drag.acknowledge("denied", &denied).unwrap_err(),
      "Another client holds resize control"
    );
  }
}
