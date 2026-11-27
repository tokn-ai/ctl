use crate::view;
use ctream_client::geometry::ImageGeometry;
use ctream_proto::live::{LiveAction, LiveEvent, Participant};
use eframe::egui::{self, Color32, Painter, Pos2, Rect, Stroke};
use std::{
  collections::{BTreeMap, VecDeque},
  time::{Duration, Instant},
};

const PULSE: Duration = Duration::from_millis(600);
const TRAIL: Duration = Duration::from_millis(900);
const ACTIVITY: Duration = Duration::from_secs(2);
const BACKGROUND: Color32 = Color32::from_rgba_premultiplied(17, 23, 31, 226);
type Point = [f32; 2];

struct Member {
  info: Participant,
  point: Option<Point>,
  down: bool,
  trail: VecDeque<Point>,
  last_drag: Option<Instant>,
  pulse: Option<(Point, Instant)>,
  scroll: Option<(Point, Instant)>,
  activity: Option<(&'static str, Instant)>,
}

impl Member {
  fn new(info: Participant) -> Self {
    Self {
      info,
      point: None,
      down: false,
      trail: VecDeque::new(),
      last_drag: None,
      pulse: None,
      scroll: None,
      activity: None,
    }
  }

  fn extend_trail(&mut self, point: Point, now: Instant) {
    if self.trail.back() != Some(&point) {
      if self.trail.len() == 48 {
        self.trail.pop_front();
      }
      self.trail.push_back(point);
    }
    self.last_drag = Some(now);
  }

  fn apply(&mut self, action: LiveAction, now: Instant) {
    let activity = match action {
      LiveAction::Move { point } => {
        self.point = Some(point);
        if self.down {
          self.extend_trail(point, now);
          self.activity = Some(("Dragging", now));
        }
        return;
      }
      LiveAction::Click { point } => {
        self.point = Some(point);
        self.pulse = Some((point, now));
        "Clicked"
      }
      LiveAction::PointerDown { point } => {
        self.point = Some(point);
        self.down = true;
        self.trail.clear();
        self.extend_trail(point, now);
        self.pulse = Some((point, now));
        "Pressed"
      }
      LiveAction::PointerUp { point } => {
        self.point = Some(point);
        self.down = false;
        self.extend_trail(point, now);
        if self.trail.len() > 1 {
          "Dragged"
        } else {
          "Clicked"
        }
      }
      LiveAction::Drag { from, to } => {
        self.trail.clear();
        self.extend_trail(from, now);
        self.extend_trail(to, now);
        self.point = Some(to);
        "Dragged"
      }
      LiveAction::Scroll { point } => {
        self.point = Some(point);
        self.scroll = Some((point, now));
        "Scrolled"
      }
      LiveAction::PressKeys {} => "Keyboard",
      LiveAction::TypeText {} => "Typed text",
      LiveAction::Invoke {} => "Activated control",
      LiveAction::SetText {} => "Edited text",
    };
    self.activity = Some((activity, now));
  }
}

#[derive(Default)]
pub(super) struct Roster {
  own_id: String,
  reconnecting: bool,
  holder: Option<Participant>,
  members: BTreeMap<String, Member>,
}

impl Roster {
  pub(super) fn reconnecting() -> Self {
    Self {
      reconnecting: true,
      ..Self::default()
    }
  }

  pub(super) fn apply(&mut self, event: LiveEvent, now: Instant) {
    match event {
      LiveEvent::Snapshot {
        self_id,
        participants,
      } => {
        self.own_id = self_id;
        self.holder = None;
        self.reconnecting = false;
        self.members = participants
          .into_iter()
          .map(|p| (p.participant_id.clone(), Member::new(p)))
          .collect();
      }
      LiveEvent::Joined { participant } => {
        self
          .members
          .insert(participant.participant_id.clone(), Member::new(participant));
      }
      LiveEvent::Left { participant_id } => {
        self.members.remove(&participant_id);
      }
      LiveEvent::Turn { holder, .. } => self.holder = holder,
      LiveEvent::Action {
        participant_id,
        action,
        ..
      } => {
        if let Some(member) = self.members.get_mut(&participant_id) {
          member.apply(action, now);
        }
      }
    }
  }

  pub(super) fn paint(&self, ui: &mut egui::Ui, image: &ImageGeometry, now: Instant) -> Rect {
    let video = view::rect(image.viewport());
    let clip = video.intersect(ui.clip_rect());
    if (self.members.is_empty() && !self.reconnecting) || !clip.is_positive() {
      return Rect::NOTHING;
    }
    let painter = ui.painter().with_clip_rect(clip);
    for (id, member) in &self.members {
      if id != &self.own_id {
        paint_cursor(ui.ctx(), &painter, video, member, now);
      }
    }

    let margin = (clip.width().min(clip.height()) * 0.25).min(5.0);
    let width = (clip.width() - margin * 2.0).clamp(1.0, 185.0);
    let available = (clip.height() - margin * 2.0).max(1.0);
    let expanded = ui.scope_id().with("live_participants_expanded");
    let open = ui
      .data(|data| data.get_temp::<bool>(expanded))
      .unwrap_or(true);
    let rows_height = if open {
      (self.members.len() as f32 * 20.0 + 2.0).min(available * 0.35)
    } else {
      0.0
    };
    let turn_height = if self.holder.is_some() { 21.0 } else { 0.0 };
    let height = (24.0 + turn_height + rows_height).min(available);
    let card = Rect::from_min_size(
      clip.right_bottom() - egui::vec2(width + margin, height + margin),
      egui::vec2(width, height),
    );
    painter.rect_filled(card, 5.0, BACKGROUND);
    let header = Rect::from_min_max(
      card.min,
      egui::pos2(card.right(), (card.top() + 24.0).min(card.bottom())),
    );
    painter.text(
      header.left_center() + egui::vec2(8.0, 0.0),
      egui::Align2::LEFT_CENTER,
      if self.reconnecting {
        "Reconnecting participants…".into()
      } else {
        format!("Participants · {}", self.members.len())
      },
      egui::FontId::proportional(12.0),
      Color32::WHITE,
    );
    if self.reconnecting {
      return Rect::NOTHING;
    }
    if ui
      .interact(header, expanded, egui::Sense::click())
      .clicked()
    {
      ui.data_mut(|data| data.insert_temp(expanded, !open));
      ui.ctx().request_repaint();
    }
    let sign = if open { -1.0 } else { 1.0 };
    let c = header.right_center() - egui::vec2(12.0, 0.0);
    painter.add(egui::Shape::line(
      vec![
        c + egui::vec2(-3.0, -sign * 1.5),
        c + egui::vec2(0.0, sign * 1.5),
        c + egui::vec2(3.0, -sign * 1.5),
      ],
      Stroke::new(1.5, Color32::WHITE),
    ));
    if let Some(holder) = &self.holder {
      let rect = Rect::from_min_max(
        egui::pos2(card.left() + 7.0, header.bottom()),
        egui::pos2(
          card.right() - 4.0,
          (header.bottom() + turn_height).min(card.bottom()),
        ),
      );
      painter.with_clip_rect(rect).text(
        rect.left_center(),
        egui::Align2::LEFT_CENTER,
        format!("Has turn · {}", holder.agent_id),
        egui::FontId::proportional(12.0),
        Color32::WHITE,
      );
    }
    if open && width > 16.0 && height > header.height() + turn_height + 2.0 {
      let rows = Rect::from_min_max(
        egui::pos2(card.left() + 7.0, header.bottom() + turn_height),
        card.max - egui::vec2(5.0, 2.0),
      );
      let mut list = ui.new_child(
        egui::UiBuilder::new()
          .id_salt("live_participants")
          .max_rect(rows)
          .layout(egui::Layout::top_down(egui::Align::Min)),
      );
      list.set_clip_rect(rows.intersect(clip));
      list.spacing_mut().item_spacing.y = 0.0;
      egui::ScrollArea::vertical()
        .max_height(rows.height())
        .auto_shrink([false, false])
        .show(&mut list, |list| {
          for (id, member) in &self.members {
            let (row, _) = list.allocate_exact_size(
              egui::vec2(list.available_width(), 20.0),
              egui::Sense::hover(),
            );
            let color = color(id);
            list
              .painter()
              .circle_filled(row.left_center() + egui::vec2(4.0, 0.0), 3.5, color);
            let activity = member.activity.and_then(|(label, start)| {
              let remaining = ACTIVITY.checked_sub(now.saturating_duration_since(start))?;
              ui.ctx().request_repaint_after(remaining);
              Some(label)
            });
            let own = id == &self.own_id;
            let name = if own {
              format!("{} (you)", member.info.agent_id)
            } else {
              member.info.agent_id.clone()
            };
            let text = match activity {
              Some(activity) => format!("{name} · {activity}"),
              None => name,
            };
            let galley = list.painter().layout_no_wrap(
              text.clone(),
              egui::FontId::proportional(12.0),
              Color32::WHITE,
            );
            list.painter().galley(
              egui::pos2(row.left() + 13.0, row.center().y - galley.size().y / 2.0),
              galley,
              Color32::WHITE,
            );
            list
              .interact(row, list.scope_id().with(id), egui::Sense::hover())
              .on_hover_text(text);
          }
        });
    }
    card
  }
}

fn color(id: &str) -> Color32 {
  let hash = id.bytes().fold(2_166_136_261u32, |hash, byte| {
    (hash ^ u32::from(byte)).wrapping_mul(16_777_619)
  });
  egui::ecolor::Hsva::new((hash % 360) as f32 / 360.0, 0.58, 1.0, 1.0).into()
}

fn pos(rect: Rect, [x, y]: Point) -> Pos2 {
  rect.min + egui::vec2(x * rect.width(), y * rect.height())
}

fn paint_cursor(ctx: &egui::Context, painter: &Painter, rect: Rect, member: &Member, now: Instant) {
  let Some(point) = member.point else { return };
  let tip = pos(rect, point);
  let color = color(&member.info.participant_id);
  if let Some(elapsed) = member.last_drag.map(|at| now.saturating_duration_since(at))
    && member.trail.len() > 1
    && (member.down || elapsed < TRAIL)
  {
    let alpha = if member.down {
      1.0
    } else {
      1.0 - elapsed.as_secs_f32() / TRAIL.as_secs_f32()
    };
    let points = member
      .trail
      .iter()
      .map(|p| pos(rect, *p))
      .collect::<Vec<_>>();
    painter.add(egui::Shape::line(
      points.clone(),
      Stroke::new(4.0, Color32::BLACK.gamma_multiply(alpha)),
    ));
    painter.add(egui::Shape::line(
      points,
      Stroke::new(2.0, color.gamma_multiply(alpha)),
    ));
    if !member.down {
      ctx.request_repaint_after(Duration::from_millis(33));
    }
  }
  if let Some((point, at)) = member.pulse
    && now.saturating_duration_since(at) < PULSE
  {
    let elapsed = now.saturating_duration_since(at);
    let t = elapsed.as_secs_f32() / PULSE.as_secs_f32();
    painter.circle_stroke(
      pos(rect, point),
      5.0 + 17.0 * t,
      Stroke::new(2.0, color.gamma_multiply(1.0 - t)),
    );
    ctx.request_repaint_after(Duration::from_millis(33));
  }
  if let Some((point, at)) = member.scroll
    && let Some(remaining) = TRAIL.checked_sub(now.saturating_duration_since(at))
  {
    // The wire reports scrolling without direction or wheel deltas.
    let wheel = Rect::from_center_size(
      pos(rect, point) - egui::vec2(9.0, 9.0),
      egui::vec2(11.0, 16.0),
    );
    painter.rect(
      wheel,
      5.0,
      BACKGROUND,
      Stroke::new(1.5, color),
      egui::StrokeKind::Inside,
    );
    painter.line_segment(
      [wheel.center_top() + egui::vec2(0.0, 3.0), wheel.center()],
      Stroke::new(2.0, color),
    );
    ctx.request_repaint_after(remaining);
  }
  painter.add(egui::Shape::convex_polygon(
    vec![
      tip,
      tip + egui::vec2(3.0, 15.0),
      tip + egui::vec2(6.0, 10.0),
      tip + egui::vec2(13.0, 9.0),
    ],
    color,
    Stroke::new(1.3, Color32::BLACK),
  ));
  let text = painter.layout_no_wrap(
    member.info.agent_id.clone(),
    egui::FontId::proportional(12.0),
    color,
  );
  let size = text.size() + egui::vec2(8.0, 4.0);
  let label = Rect::from_min_size(
    (tip + egui::vec2(9.0, 12.0))
      .min(rect.max - size)
      .max(rect.min),
    size,
  );
  painter.rect_filled(label, 3.0, BACKGROUND);
  painter.galley(label.min + egui::vec2(4.0, 2.0), text, color);
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn turn_owner_can_come_from_another_window_and_new_snapshots_clear_it() {
    let now = Instant::now();
    let mut roster = Roster::default();
    let local = Participant {
      participant_id: "here".into(),
      agent_id: "reader".into(),
    };
    let elsewhere = Participant {
      participant_id: "elsewhere".into(),
      agent_id: "writer".into(),
    };
    let snapshot = LiveEvent::Snapshot {
      self_id: "here".into(),
      participants: vec![local],
    };
    roster.apply(snapshot.clone(), now);
    roster.apply(
      LiveEvent::Turn {
        holder: Some(elsewhere.clone()),
        expires_at_ms: Some(42),
      },
      now,
    );
    assert_eq!(roster.holder.as_ref(), Some(&elsewhere));
    assert_eq!(roster.members.len(), 1);
    roster.apply(snapshot, now);
    assert!(roster.holder.is_none());
  }

  #[test]
  fn effects_keep_their_original_position_and_each_participant_has_its_own_gesture() {
    let person = |id: &str| Participant {
      participant_id: id.into(),
      agent_id: "shared-agent".into(),
    };
    let now = Instant::now();
    let mut roster = Roster::default();
    roster.apply(
      LiveEvent::Snapshot {
        self_id: "local".into(),
        participants: vec![person("local"), person("a"), person("b")],
      },
      now,
    );
    let mut send = |id: &str, action| {
      roster.apply(
        LiveEvent::Action {
          participant_id: id.into(),
          action,
          action_id: None,
        },
        now,
      )
    };
    let start = [0.1, 0.2];
    let end = [0.75, 0.8];
    send("a", LiveAction::PointerDown { point: start });
    send("a", LiveAction::Move { point: [0.2, 0.3] });
    send("b", LiveAction::Click { point: start });
    send("b", LiveAction::Scroll { point: end });
    send("a", LiveAction::PointerUp { point: end });
    send("a", LiveAction::Move { point: [0.0, 0.0] });
    send("b", LiveAction::SetText {});
    let a = &roster.members["a"];
    assert_eq!(a.point, Some([0.0, 0.0]));
    assert_eq!(a.trail, [start, [0.2, 0.3], end]);
    assert!(!a.down);
    let b = &roster.members["b"];
    assert_eq!(b.pulse.unwrap().0, start);
    assert_eq!(b.scroll.unwrap().0, end);
    assert_eq!(b.activity.unwrap().0, "Edited text");
    assert!(b.trail.is_empty());
    assert_ne!(color("a"), color("b"));
    roster.apply(
      LiveEvent::Left {
        participant_id: "a".into(),
      },
      now,
    );
    assert!(!roster.members.contains_key("a"));
  }
}
