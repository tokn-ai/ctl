mod panel;

use super::{FrameCounts, latency, requests::Traffic};
use ctream_client::media::FrameFormat;
use eframe::egui;
use std::{
  collections::VecDeque,
  time::{Duration, Instant},
};

const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const MAX_PAINT_GAP: Duration = SAMPLE_INTERVAL.saturating_mul(2);
const HISTORY_LIMIT: usize = 60;
const OVERLAY_MARGIN: f32 = 8.0;

#[derive(Clone)]
pub(super) enum Connection {
  Local,
  Ssh(String),
}

pub(super) struct CaptureDetails {
  pub connection: Option<Connection>,
  pub target: String,
  pub window: Option<String>,
  pub driver: Option<String>,
  pub format: FrameFormat,
}

pub(super) struct Stats {
  traffic: Traffic,
  sampler: Sampler,
  latency: latency::Latency,
  details: CaptureDetails,
  expanded: bool,
  history: [VecDeque<Option<f64>>; 4],
}

impl Stats {
  pub(super) fn new(
    traffic: Traffic,
    latency: latency::Latency,
    details: CaptureDetails,
    frames: FrameCounts,
  ) -> Self {
    let sampler = Sampler::new(Instant::now(), traffic.totals(), frames);
    Self {
      traffic,
      sampler,
      latency,
      details,
      expanded: false,
      history: Default::default(),
    }
  }

  /// Returns whether diagnostics owns input and the compact link rect.
  pub(super) fn show(
    &mut self,
    ui: &mut egui::Ui,
    video: egui::Rect,
    frames: FrameCounts,
    size: [usize; 2],
  ) -> (bool, egui::Rect) {
    let now = Instant::now();
    let totals = self.traffic.totals();
    let latency = self.latency.reading();
    let last_sample = self.sampler.last_sample;
    self.sampler.update(now, totals, frames);
    if self.sampler.last_sample != last_sample {
      let point = self.sampler.sample.as_ref().map_or([None; 4], |sample| {
        [
          Some(sample.rates[0]),
          Some(sample.rates[1]),
          match latency {
            latency::Reading::Measured(rtt) => Some(rtt.as_secs_f64() * 1000.0),
            _ => None,
          },
          Some(sample.fps),
        ]
      });
      for (history, value) in self.history.iter_mut().zip(point) {
        if history.len() == HISTORY_LIMIT {
          history.pop_front();
        }
        history.push_back(value);
      }
    }
    ui.ctx()
      .request_repaint_after(SAMPLE_INTERVAL.saturating_sub(now - self.sampler.last_sample));

    let values = ReadingLabels::new(self.sampler.sample.as_ref(), latency);
    if self.expanded {
      if self.show_expanded(ui, video, &values, totals, frames, size) {
        self.expanded = false;
        ui.ctx().request_repaint();
      }
      // Keep this paint local even when it processes a Collapse click.
      return (true, egui::Rect::NOTHING);
    }
    let response = show_compact(ui, video, &values, size);
    if response.clicked() {
      response.surrender_focus();
      self.expanded = true;
      ui.ctx().request_repaint();
    }
    (self.expanded, response.rect)
  }

  fn show_expanded(
    &mut self,
    ui: &mut egui::Ui,
    video: egui::Rect,
    values: &ReadingLabels,
    totals: [u64; 2],
    frames: FrameCounts,
    [width, height]: [usize; 2],
  ) -> bool {
    let resolution = format!("{width} × {height} px");
    let received = frames.received.to_string();
    let preview = frames.uploaded.to_string();
    let superseded = frames.superseded.to_string();
    let downloaded = bytes(totals[0] as f64);
    let uploaded = bytes(totals[1] as f64);
    let frame_format = match self.details.format {
      FrameFormat::FixedRgb { .. } => "RGB24 (fixed size)",
      FrameFormat::Ppm => "RGB24 (PPM / P6)",
    };
    let mut rows = vec![
      (
        "Capture driver",
        self.details.driver.as_deref().unwrap_or("Agent default"),
      ),
      ("Target", self.details.target.as_str()),
    ];
    if let Some(window) = &self.details.window {
      rows.push(("Window", window));
    }
    match &self.details.connection {
      Some(Connection::Local) => rows.push(("Transport", "Local stdio")),
      Some(Connection::Ssh(host)) => rows.extend([("Transport", "SSH"), ("Host", host.as_str())]),
      None => {}
    }
    rows.extend([
      ("Frame format", frame_format),
      ("Resolution", &resolution),
      ("Received FPS", &values.received_fps),
      ("Frames received", &received),
      ("Preview frames", &preview),
      ("Superseded frames", &superseded),
      ("Payload received", &downloaded),
      ("Payload sent", &uploaded),
    ]);
    let [download, upload, latency, fps] = self
      .history
      .each_mut()
      .map(|series| series.make_contiguous());
    let graphs = [
      panel::Graph {
        title: "Download",
        value: &values.download,
        samples: download,
        color: egui::Color32::from_rgb(110, 210, 215),
      },
      panel::Graph {
        title: "Upload",
        value: &values.upload,
        samples: upload,
        color: egui::Color32::from_rgb(142, 189, 249),
      },
      panel::Graph {
        title: "Latency",
        value: &values.latency,
        samples: latency,
        color: egui::Color32::from_rgb(232, 189, 113),
      },
      panel::Graph {
        title: "Preview FPS",
        value: &values.fps,
        samples: fps,
        color: egui::Color32::from_rgb(156, 218, 160),
      },
    ];
    panel::show(ui, video, &rows, &graphs)
  }
}

struct ReadingLabels {
  download: String,
  upload: String,
  latency: String,
  fps: String,
  received_fps: String,
}

impl ReadingLabels {
  fn new(sample: Option<&Sample>, latency: latency::Reading) -> Self {
    let latency = match latency {
      latency::Reading::Pending => "…".into(),
      latency::Reading::Measured(rtt) => format!("{:.1} ms", rtt.as_secs_f64() * 1000.0),
      latency::Reading::Unavailable => "N/A".into(),
    };
    let [download, upload, fps, received_fps] = match sample {
      Some(sample) => [
        format!("{}/s", bytes(sample.rates[0])),
        format!("{}/s", bytes(sample.rates[1])),
        format!("{:.1}", sample.fps),
        format!("{:.1}", sample.received_fps),
      ],
      None => std::array::from_fn(|_| "…".into()),
    };
    Self {
      download,
      upload,
      latency,
      fps,
      received_fps,
    }
  }
}

fn show_compact(
  ui: &mut egui::Ui,
  video: egui::Rect,
  values: &ReadingLabels,
  [width, height]: [usize; 2],
) -> egui::Response {
  let text = format!(
    "Download  {}\nUpload    {}\nLatency   {}\nFPS       {}\nFrame     {width}×{height} px",
    values.download, values.upload, values.latency, values.fps,
  );
  let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(video));
  ui.set_clip_rect(video.intersect(ui.clip_rect()));
  ui.style_mut().interaction.selectable_labels = false;
  let painter = ui.painter();
  let margin = OVERLAY_MARGIN;
  let padding = egui::vec2(8.0, 6.0);
  let font = egui::FontId::monospace(12.0);
  let text = painter.layout(
    text,
    font.clone(),
    egui::Color32::WHITE,
    (video.width() - 2.0 * (margin + padding.x)).max(1.0),
  );
  let link = painter.layout_no_wrap("[expand]".into(), font, ui.visuals().hyperlink_color);
  let panel_size = egui::vec2(
    text.size().x.max(link.size().x) + 2.0 * padding.x,
    text.size().y + link.size().y + 3.0 * padding.y,
  );
  let origin = egui::pos2(
    video.left() + margin,
    (video.bottom() - panel_size.y - margin).max(video.top()),
  );
  painter.rect_filled(
    egui::Rect::from_min_size(origin, panel_size),
    4.0,
    egui::Color32::from_black_alpha(200),
  );
  let link_origin = origin + padding + egui::vec2(0.0, text.size().y + padding.y);
  painter.galley(origin + padding, text, egui::Color32::WHITE);
  ui.put(
    egui::Rect::from_min_size(link_origin, link.size()),
    egui::Link::new(link),
  )
}

fn bytes(count: f64) -> String {
  for (unit, factor) in [("GiB", 1 << 30), ("MiB", 1 << 20), ("KiB", 1 << 10)] {
    if count >= f64::from(factor) {
      return format!("{:.1} {unit}", count / f64::from(factor));
    }
  }
  format!("{count:.0} B")
}

struct Sampler {
  last_paint: Instant,
  last_sample: Instant,
  last_totals: [u64; 2],
  last_frame: u64,
  last_received: u64,
  fresh_frames: u64,
  sample: Option<Sample>,
}

#[derive(Debug, PartialEq)]
struct Sample {
  rates: [f64; 2],
  fps: f64,
  received_fps: f64,
}

impl Sampler {
  fn new(now: Instant, totals: [u64; 2], frames: FrameCounts) -> Self {
    Self {
      last_paint: now,
      last_sample: now,
      last_totals: totals,
      last_frame: frames.uploaded,
      last_received: frames.received,
      fresh_frames: 0,
      sample: None,
    }
  }

  fn update(&mut self, now: Instant, totals: [u64; 2], frames: FrameCounts) {
    if now.duration_since(self.last_paint) > MAX_PAINT_GAP {
      // The reader may have kept draining traffic while painting was paused.
      // Start a new observation interval from this paint and its byte totals.
      self.last_sample = now;
      self.last_totals = totals;
      self.last_frame = frames.uploaded;
      self.last_received = frames.received;
      self.fresh_frames = 0;
      self.sample = None;
    }
    self.last_paint = now;
    if self.last_frame != frames.uploaded {
      self.last_frame = frames.uploaded;
      self.fresh_frames += 1;
    }
    let elapsed = now - self.last_sample;
    if elapsed < SAMPLE_INTERVAL {
      return;
    }

    let seconds = elapsed.as_secs_f64();
    self.sample = Some(Sample {
      rates: std::array::from_fn(|index| {
        totals[index].saturating_sub(self.last_totals[index]) as f64 / seconds
      }),
      fps: self.fresh_frames as f64 / seconds,
      received_fps: frames.received.saturating_sub(self.last_received) as f64 / seconds,
    });
    self.last_sample = now;
    self.last_totals = totals;
    self.last_received = frames.received;
    self.fresh_frames = 0;
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn frames(uploaded: u64, received: u64) -> FrameCounts {
    FrameCounts {
      uploaded,
      received,
      superseded: received.saturating_sub(uploaded),
    }
  }

  #[test]
  fn rates_use_elapsed_time_and_fps_counts_only_new_drawn_frames() {
    let start = Instant::now();
    let mut sampler = Sampler::new(start, [900, 95], FrameCounts::default());
    sampler.update(start, [900, 95], frames(1, 1));
    sampler.update(
      start + Duration::from_millis(700),
      [1500, 115],
      frames(1, 6),
    );
    sampler.update(start + Duration::from_secs(2), [2900, 495], frames(6, 18));
    assert_eq!(
      sampler.sample,
      Some(Sample {
        rates: [1000.0, 200.0],
        fps: 1.0,
        received_fps: 9.0,
      })
    );

    sampler.update(start + Duration::from_secs(3), [2900, 495], frames(6, 18));
    assert_eq!(
      sampler.sample,
      Some(Sample {
        rates: [0.0; 2],
        fps: 0.0,
        received_fps: 0.0,
      })
    );
  }

  #[test]
  fn resume_starts_a_fresh_sample_without_background_traffic() {
    let start = Instant::now();
    let mut sampler = Sampler::new(start, [0, 0], FrameCounts::default());
    sampler.update(start, [0, 0], frames(1, 2));
    sampler.update(start + SAMPLE_INTERVAL, [100, 50], frames(2, 5));
    assert!(sampler.sample.is_some());

    let resumed = start + Duration::from_secs(10);
    sampler.update(resumed, [10_000, 500], frames(90, 135));
    assert!(sampler.sample.is_none());
    sampler.update(
      resumed + Duration::from_millis(900),
      [10_450, 520],
      frames(90, 144),
    );
    assert!(sampler.sample.is_none());

    sampler.update(resumed + SAMPLE_INTERVAL, [10_500, 525], frames(91, 145));
    assert_eq!(
      sampler.sample,
      Some(Sample {
        rates: [500.0, 25.0],
        fps: 1.0,
        received_fps: 10.0,
      })
    );
  }
}
