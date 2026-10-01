//! Coalesced positive session observations, independent of terminal output.

use std::time::{Duration, Instant};

use ctmux_client::AttachmentEvent;

use crate::dto::valid_observation_timestamp;

const PUBLISH_INTERVAL: Duration = Duration::from_secs(15);

#[derive(Default)]
pub(super) struct Observations {
  latest: Option<u64>,
  published: Option<u64>,
  published_at: Option<Instant>,
}

impl Observations {
  pub(super) fn record(
    &mut self,
    event: &AttachmentEvent,
    received_at_ms: Option<u64>,
    now: Instant,
  ) -> Option<u64> {
    // Exited is synthesized locally, including timeout and failed transport.
    // Every other event comes from a positive incoming daemon message.
    if matches!(event, AttachmentEvent::Exited { .. }) {
      return None;
    }
    let received_at_ms = received_at_ms.filter(|value| valid_observation_timestamp(*value))?;
    self.latest = Some(
      self
        .latest
        .map_or(received_at_ms, |last| last.max(received_at_ms)),
    );
    // Geometry must carry its own current observation. Otherwise a newer
    // inspection could make the frontend discard it as an older saved size.
    let immediate = matches!(
      event,
      AttachmentEvent::Checkpoint { .. }
        | AttachmentEvent::PtyGeometryChanged { .. }
        | AttachmentEvent::SessionEnded { .. }
        | AttachmentEvent::ServerError { .. }
    );
    if (immediate
      || self
        .published_at
        .is_none_or(|last| now.saturating_duration_since(last) >= PUBLISH_INTERVAL))
      && let Some(latest) = self.flush()
    {
      self.published_at = Some(now);
      return Some(latest);
    }
    None
  }

  pub(super) fn flush(&mut self) -> Option<u64> {
    let latest = self.latest?;
    if self.published == Some(latest) {
      return None;
    }
    self.published = Some(latest);
    Some(latest)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use ctmux_client::{AttachExit, AttachExitReason};

  #[test]
  fn geometry_and_checkpoints_publish_their_observation_before_the_next_heartbeat_interval() {
    let size = ctmux_proto::TerminalSize::default();
    let events = [
      AttachmentEvent::PtyGeometryChanged {
        terminal_size: size.clone(),
        observed_sequence: 0,
      },
      AttachmentEvent::Checkpoint {
        checkpoint: ctmux_proto::TerminalCheckpoint {
          format: ctmux_proto::TERMINAL_CHECKPOINT_FORMAT.into(),
          format_version: ctmux_proto::TERMINAL_CHECKPOINT_FORMAT_VERSION,
          sequence: 0,
          terminal_size: size,
          payload: Vec::new(),
          input_prefix: Vec::new(),
        },
        history: ctmux_proto::TerminalHistorySnapshot {
          format: ctmux_proto::TERMINAL_HISTORY_FORMAT.into(),
          format_version: ctmux_proto::TERMINAL_HISTORY_FORMAT_VERSION,
          sequence: 0,
          generation: 0,
          revision: 0,
          retained_bytes: 0,
          truncated: false,
          lines: Vec::new(),
        },
        history_gap: false,
      },
    ];
    for event in events {
      let mut observations = Observations::default();
      let now = Instant::now();
      assert_eq!(
        observations.record(
          &AttachmentEvent::HeartbeatAck { nonce: 1 },
          Some(1_000),
          now
        ),
        Some(1_000),
      );
      // An independent inspection at T10 must not make this T11 size appear to
      // come from the last published stream observation at T0.
      assert_eq!(
        observations.record(&event, Some(12_000), now + Duration::from_secs(11)),
        Some(12_000),
      );
      assert_eq!(observations.flush(), None);
    }
  }

  #[test]
  fn quiet_heartbeats_publish_immediately_then_coalesce_until_the_interval() {
    let mut observations = Observations::default();
    let now = Instant::now();
    let heartbeat = AttachmentEvent::HeartbeatAck { nonce: 1 };
    assert_eq!(
      observations.record(&heartbeat, Some(1_000), now),
      Some(1_000)
    );
    assert_eq!(
      observations.record(&heartbeat, Some(6_000), now + Duration::from_secs(5)),
      None
    );
    assert_eq!(
      observations.record(&heartbeat, Some(11_000), now + Duration::from_secs(10)),
      None
    );
    assert_eq!(
      observations.record(&heartbeat, Some(16_000), now + Duration::from_secs(15)),
      Some(16_000)
    );
    assert_eq!(observations.flush(), None);
  }

  #[test]
  fn closure_flushes_the_last_received_time_without_advancing_to_disconnect_time() {
    let mut observations = Observations::default();
    let now = Instant::now();
    let heartbeat = AttachmentEvent::HeartbeatAck { nonce: 1 };
    assert_eq!(
      observations.record(&heartbeat, Some(1_000), now),
      Some(1_000)
    );
    assert_eq!(
      observations.record(&heartbeat, Some(6_000), now + Duration::from_secs(5)),
      None
    );
    let closed = AttachmentEvent::Exited {
      exit: AttachExit {
        reason: AttachExitReason::ConnectionClosed,
        next_sequence: None,
        received_sequence: 0,
      },
    };
    assert_eq!(
      observations.record(&closed, Some(60_000), now + Duration::from_secs(59)),
      None
    );
    assert_eq!(observations.flush(), Some(6_000));
    assert_eq!(observations.flush(), None);
    assert_eq!(Observations::default().flush(), None);
  }

  #[test]
  fn reordered_or_invalid_wall_clock_values_never_regress_or_invent_an_observation() {
    let mut observations = Observations::default();
    let now = Instant::now();
    let heartbeat = AttachmentEvent::HeartbeatAck { nonce: 1 };
    for timestamp in [None, Some(0), Some(u64::MAX)] {
      assert_eq!(observations.record(&heartbeat, timestamp, now), None);
    }
    assert_eq!(observations.flush(), None);
    assert_eq!(
      observations.record(&heartbeat, Some(10_000), now),
      Some(10_000)
    );
    assert_eq!(
      observations.record(&heartbeat, Some(9_000), now + PUBLISH_INTERVAL),
      None
    );
    assert_eq!(observations.flush(), None);
    assert_eq!(
      observations.record(&heartbeat, Some(11_000), now + PUBLISH_INTERVAL),
      Some(11_000)
    );
  }
}
