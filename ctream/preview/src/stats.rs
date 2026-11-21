use super::{latency, requests::Traffic};
use eframe::egui;
use std::time::{Duration, Instant};

const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

pub(super) struct Stats {
  traffic: Traffic,
  sampler: Sampler,
  latency: latency::Latency,
}

impl Stats {
  pub(super) fn new(traffic: Traffic, latency: latency::Latency) -> Self {
    let sampler = Sampler::new(Instant::now(), traffic.totals());
    Self {
      traffic,
      sampler,
      latency,
    }
  }

  pub(super) fn paint(
    &mut self,
    painter: &egui::Painter,
    video: egui::Rect,
    frame: u64,
    [width, height]: [usize; 2],
  ) {
    let now = Instant::now();
    self.sampler.update(now, self.traffic.totals(), frame);
    painter.ctx().request_repaint_after_for(
      SAMPLE_INTERVAL.saturating_sub(now - self.sampler.last_sample),
      egui::ViewportId::ROOT,
    );

    let latency = match self.latency.reading() {
      latency::Reading::Pending => "…".into(),
      latency::Reading::Measured(rtt) => format!("{:.1} ms", rtt.as_secs_f64() * 1000.0),
      latency::Reading::Unavailable => "N/A".into(),
    };
    let text = format!(
      "Download  {}\nUpload    {}\nLatency   {latency}\nFPS       {:.1}\nFrame     {width}×{height} px",
      rate(self.sampler.rates[0]),
      rate(self.sampler.rates[1]),
      self.sampler.fps,
    );
    let painter = painter.with_clip_rect(video);
    let margin = 8.0;
    let padding = egui::vec2(8.0, 6.0);
    let text = painter.layout(
      text,
      egui::FontId::monospace(12.0),
      egui::Color32::WHITE,
      (video.width() - 2.0 * (margin + padding.x)).max(1.0),
    );
    let panel_size = text.size() + padding * 2.0;
    let origin = egui::pos2(
      video.left() + margin,
      (video.bottom() - panel_size.y - margin).max(video.top()),
    );
    painter.rect_filled(
      egui::Rect::from_min_size(origin, panel_size),
      4.0,
      egui::Color32::from_black_alpha(200),
    );
    painter.galley(origin + padding, text, egui::Color32::WHITE);
  }
}

fn rate(bytes_per_second: f64) -> String {
  const KIB: f64 = 1024.0;
  const MIB: f64 = KIB * KIB;
  if bytes_per_second >= MIB {
    format!("{:.1} MiB/s", bytes_per_second / MIB)
  } else if bytes_per_second >= KIB {
    format!("{:.1} KiB/s", bytes_per_second / KIB)
  } else {
    format!("{bytes_per_second:.0} B/s")
  }
}

struct Sampler {
  last_sample: Instant,
  last_totals: [u64; 2],
  last_frame: Option<u64>,
  fresh_frames: u64,
  rates: [f64; 2],
  fps: f64,
}

impl Sampler {
  fn new(now: Instant, totals: [u64; 2]) -> Self {
    Self {
      last_sample: now,
      last_totals: totals,
      last_frame: None,
      fresh_frames: 0,
      rates: [0.0; 2],
      fps: 0.0,
    }
  }

  fn update(&mut self, now: Instant, totals: [u64; 2], frame: u64) {
    if self.last_frame != Some(frame) {
      self.last_frame = Some(frame);
      self.fresh_frames += 1;
    }
    let elapsed = now - self.last_sample;
    if elapsed < SAMPLE_INTERVAL {
      return;
    }

    let seconds = elapsed.as_secs_f64();
    self.rates = std::array::from_fn(|index| {
      totals[index].saturating_sub(self.last_totals[index]) as f64 / seconds
    });
    self.fps = self.fresh_frames as f64 / seconds;
    self.last_sample = now;
    self.last_totals = totals;
    self.fresh_frames = 0;
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn rates_use_elapsed_time_and_fps_counts_only_new_drawn_frames() {
    let start = Instant::now();
    let mut sampler = Sampler::new(start, [900, 95]);
    sampler.update(start, [900, 95], 1);
    sampler.update(start + Duration::from_millis(700), [1500, 115], 1);
    sampler.update(start + Duration::from_secs(2), [2900, 495], 6);
    assert_eq!(sampler.rates, [1000.0, 200.0]);
    assert_eq!(sampler.fps, 1.0);

    sampler.update(start + Duration::from_secs(3), [2900, 495], 6);
    assert_eq!(sampler.rates, [0.0; 2]);
    assert_eq!(sampler.fps, 0.0);
  }
}
