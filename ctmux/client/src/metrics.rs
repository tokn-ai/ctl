use ctmux_proto::SessionHistoryStatistics;
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::time::Instant;

const MAX_OUTSTANDING_HEARTBEATS: usize = 32;

/// Locally observed attachment traffic and presentation progress.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AttachmentStatistics {
  /// Framed application bytes written since the controller started.
  ///
  /// Excludes the initial attach handshake and transport overhead such as SSH
  /// encryption, compression, and TCP framing.
  pub uploaded_bytes: u64,
  /// Framed application bytes read since the controller started.
  ///
  /// Uses the same boundaries as `uploaded_bytes`, including control/history
  /// frames rather than only terminal output.
  pub downloaded_bytes: u64,
  /// Most recently matched heartbeat round trip, measured with a local clock.
  pub round_trip_ms: Option<f64>,
  /// Age of the oldest received presentation awaiting acknowledgement, or the
  /// last completed receive-to-acknowledgement duration when none is pending.
  ///
  /// This includes local queues and renderer work, not time before receipt.
  /// The initial checkpoint is timed from controller construction.
  pub event_lag_ms: Option<f64>,
  /// Received presentation events still awaiting renderer acknowledgement.
  pub pending_events: u64,
  /// Last server-reported retained history total for the complete session.
  /// Older negotiated contracts leave this unavailable.
  pub session_history: Option<SessionHistoryStatistics>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Metrics(Arc<Shared>);

#[derive(Debug, Default)]
struct Shared {
  uploaded_bytes: AtomicU64,
  downloaded_bytes: AtomicU64,
  timing: Mutex<Timing>,
}

#[derive(Debug, Default)]
struct Timing {
  heartbeats: VecDeque<(u64, Instant)>,
  round_trip_ms: Option<f64>,
  completed_lag_ms: Option<f64>,
  next_receipt: u64,
  pending: BTreeMap<u64, Instant>,
  session_history: Option<SessionHistoryStatistics>,
}

impl Metrics {
  fn timing(&self) -> MutexGuard<'_, Timing> {
    self
      .0
      .timing
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
  }

  pub(crate) fn snapshot(&self) -> AttachmentStatistics {
    let timing = self.timing();
    AttachmentStatistics {
      uploaded_bytes: self.0.uploaded_bytes.load(Ordering::Relaxed),
      downloaded_bytes: self.0.downloaded_bytes.load(Ordering::Relaxed),
      round_trip_ms: timing.round_trip_ms,
      event_lag_ms: timing
        .pending
        .first_key_value()
        .map_or(timing.completed_lag_ms, |(_, received)| {
          Some(elapsed_ms(*received))
        }),
      pending_events: timing.pending.len() as u64,
      session_history: timing.session_history.clone(),
    }
  }

  pub(crate) fn heartbeat_sent(&self, nonce: u64) {
    let mut timing = self.timing();
    if timing.heartbeats.len() == MAX_OUTSTANDING_HEARTBEATS {
      timing.heartbeats.pop_front();
    }
    timing.heartbeats.push_back((nonce, Instant::now()));
  }

  pub(crate) fn heartbeat_received(
    &self,
    nonce: u64,
    session_history: Option<&SessionHistoryStatistics>,
  ) {
    let mut timing = self.timing();
    if let Some(index) = timing
      .heartbeats
      .iter()
      .position(|(sent, _)| *sent == nonce)
      && let Some((_, sent)) = timing.heartbeats.remove(index)
    {
      timing.round_trip_ms = Some(elapsed_ms(sent));
    }
    timing.session_history = session_history.cloned();
  }

  pub(crate) fn presentation_received(&self) -> PresentationReceipt {
    let mut timing = self.timing();
    let id = timing.next_receipt;
    timing.next_receipt = id
      .checked_add(1)
      .expect("presentation receipt IDs exhausted");
    timing.pending.insert(id, Instant::now());
    PresentationReceipt {
      metrics: self.clone(),
      id,
    }
  }

  fn remove_receipt(&self, id: u64, applied: bool) {
    let mut timing = self.timing();
    if let Some(received) = timing.pending.remove(&id)
      && applied
    {
      timing.completed_lag_ms = Some(elapsed_ms(received));
    }
  }
}

fn elapsed_ms(start: Instant) -> f64 {
  Instant::now()
    .saturating_duration_since(start)
    .as_secs_f64()
    * 1_000.0
}

/// Queue and ledger ownership bound the number of tracked receipts. Dropping a
/// cancelled, rejected, or stale delivery removes it without claiming it applied.
pub(crate) struct PresentationReceipt {
  metrics: Metrics,
  id: u64,
}

impl PresentationReceipt {
  pub(crate) fn applied(self) {
    self.metrics.remove_receipt(self.id, true);
  }
}

impl Drop for PresentationReceipt {
  fn drop(&mut self) {
    self.metrics.remove_receipt(self.id, false);
  }
}

pub(crate) struct MeteredStream<S> {
  stream: S,
  metrics: Metrics,
}

impl<S> MeteredStream<S> {
  pub(crate) fn new(stream: S, metrics: Metrics) -> Self {
    Self { stream, metrics }
  }
}

impl<S: AsyncRead + Unpin> AsyncRead for MeteredStream<S> {
  fn poll_read(
    mut self: Pin<&mut Self>,
    cx: &mut Context<'_>,
    buffer: &mut ReadBuf<'_>,
  ) -> Poll<io::Result<()>> {
    let before = buffer.filled().len();
    let result = Pin::new(&mut self.stream).poll_read(cx, buffer);
    if matches!(result, Poll::Ready(Ok(()))) {
      self
        .metrics
        .0
        .downloaded_bytes
        .fetch_add((buffer.filled().len() - before) as u64, Ordering::Relaxed);
    }
    result
  }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for MeteredStream<S> {
  fn poll_write(
    mut self: Pin<&mut Self>,
    cx: &mut Context<'_>,
    buffer: &[u8],
  ) -> Poll<io::Result<usize>> {
    let result = Pin::new(&mut self.stream).poll_write(cx, buffer);
    if let Poll::Ready(Ok(written)) = result {
      self
        .metrics
        .0
        .uploaded_bytes
        .fetch_add(written as u64, Ordering::Relaxed);
    }
    result
  }

  fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
    Pin::new(&mut self.stream).poll_flush(cx)
  }

  fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
    Pin::new(&mut self.stream).poll_shutdown(cx)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use ctmux_proto::{ClientMessage, ServerMessage, read_frame, write_frame};
  use std::time::Duration;

  #[tokio::test]
  async fn traffic_counts_successful_framed_io_including_partial_writes() {
    let metrics = Metrics::default();
    let (client, mut server) = tokio::io::duplex(7);
    let mut client = MeteredStream::new(client, metrics.clone());
    let input = ClientMessage::Input {
      data: vec![0, 255, 42],
    };
    let output = ServerMessage::Output {
      sequence_start: 0,
      sequence_end: 2,
      data: vec![0, 255],
    };
    let exchange = async {
      assert_eq!(
        read_frame::<_, ClientMessage>(&mut server).await.unwrap(),
        Some(input.clone())
      );
      write_frame(&mut server, &output).await.unwrap();
    };
    let client_exchange = async {
      write_frame(&mut client, &input).await.unwrap();
      assert_eq!(
        read_frame::<_, ServerMessage>(&mut client).await.unwrap(),
        Some(output.clone())
      );
    };
    tokio::join!(exchange, client_exchange);
    let statistics = metrics.snapshot();
    assert_eq!(
      statistics.uploaded_bytes,
      serde_json::to_vec(&input).unwrap().len() as u64 + 4
    );
    assert_eq!(
      statistics.downloaded_bytes,
      serde_json::to_vec(&output).unwrap().len() as u64 + 4
    );
  }

  #[tokio::test(start_paused = true)]
  async fn heartbeat_samples_are_matched_once_and_tracking_is_bounded() {
    let metrics = Metrics::default();
    metrics.heartbeat_sent(10);
    tokio::time::advance(Duration::from_millis(20)).await;
    metrics.heartbeat_received(99, None);
    assert_eq!(metrics.snapshot().round_trip_ms, None);
    metrics.heartbeat_received(10, None);
    assert_eq!(metrics.snapshot().round_trip_ms, Some(20.0));
    tokio::time::advance(Duration::from_secs(1)).await;
    metrics.heartbeat_received(10, None);
    assert_eq!(metrics.snapshot().round_trip_ms, Some(20.0));
    for nonce in 0..=MAX_OUTSTANDING_HEARTBEATS as u64 {
      metrics.heartbeat_sent(nonce);
    }
    assert_eq!(
      metrics.timing().heartbeats.len(),
      MAX_OUTSTANDING_HEARTBEATS
    );
    metrics.heartbeat_received(0, None);
    assert_eq!(metrics.snapshot().round_trip_ms, Some(20.0));
  }

  #[tokio::test(start_paused = true)]
  async fn oldest_pending_receipt_ages_until_applied_and_cancelled_receipts_disappear() {
    let metrics = Metrics::default();
    let first = metrics.presentation_received();
    tokio::time::advance(Duration::from_millis(20)).await;
    let second = metrics.presentation_received();
    tokio::time::advance(Duration::from_millis(10)).await;
    assert_eq!(metrics.snapshot().pending_events, 2);
    assert_eq!(metrics.snapshot().event_lag_ms, Some(30.0));
    first.applied();
    assert_eq!(metrics.snapshot().event_lag_ms, Some(10.0));
    drop(second);
    assert_eq!(metrics.snapshot().pending_events, 0);
    assert_eq!(metrics.snapshot().event_lag_ms, Some(30.0));
  }
}
