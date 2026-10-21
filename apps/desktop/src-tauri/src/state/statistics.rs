//! Sample attachment telemetry independently of terminal presentation progress.
//!
//! A slow renderer must not freeze its own lag indicator or hide heartbeat RTT.

use tauri::ipc::Channel;
use tokio::time::{Interval, timeout_at};

use crate::dto::AttachmentEventDto;
use crate::error::{CommandErrorDto, CommandResult};

use super::AttachmentActor;

pub(super) const INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

pub(super) async fn wait_for_presentation(
  actor: &AttachmentActor,
  channel: &Channel<AttachmentEventDto>,
  interval: &mut Interval,
  final_event: bool,
) -> CommandResult<()> {
  let Some(started_at) = actor
    .pending
    .lock()
    .await
    .as_ref()
    .map(|pending| pending.started_at)
  else {
    return Ok(());
  };
  // Anchor this to publication, including when a completed controller hands
  // the same event over to final draining. Sampling cannot reset its deadline.
  timeout_at(
    started_at + super::PRESENTATION_ACKNOWLEDGEMENT_TIMEOUT,
    async {
      let applied = actor.wait_until_presentation_applied();
      tokio::pin!(applied);
      loop {
        tokio::select! {
          () = &mut applied => return Ok(()),
          _ = interval.tick() => publish(actor, channel).await?,
        }
      }
    },
  )
  .await
  .map_err(|_| {
    CommandErrorDto::new(
      "presentation_acknowledgement_timeout",
      if final_event {
        "the terminal renderer did not acknowledge its final event within 30 seconds"
      } else {
        "the terminal renderer did not acknowledge its event within 30 seconds"
      },
    )
  })?
}

pub(super) async fn publish(
  actor: &AttachmentActor,
  channel: &Channel<AttachmentEventDto>,
) -> CommandResult<()> {
  let mut statistics = actor.control.state().statistics();
  if let Some(pending) = actor.pending.lock().await.as_ref() {
    // The controller releases its receive ledger on exit. Final frames can
    // still be awaiting this bridge's renderer acknowledgement; keep their
    // local waiting time visible while they drain.
    let bridge_lag_ms = pending.started_at.elapsed().as_secs_f64() * 1_000.0;
    statistics.event_lag_ms = Some(if statistics.pending_events == 0 {
      bridge_lag_ms
    } else {
      statistics.event_lag_ms.unwrap_or(0.0).max(bridge_lag_ms)
    });
    statistics.pending_events = statistics.pending_events.max(1);
  }
  let (history_session_id, history_retained_bytes) = match statistics.session_history {
    Some(history) => (
      Some(history.session_id),
      Some(history.retained_bytes.to_string()),
    ),
    None => (None, None),
  };
  channel
    .send(AttachmentEventDto::AttachmentStatistics {
      attachment_id: actor.attachment_id.clone(),
      uploaded_bytes: statistics.uploaded_bytes.to_string(),
      downloaded_bytes: statistics.downloaded_bytes.to_string(),
      round_trip_ms: statistics.round_trip_ms,
      event_lag_ms: statistics.event_lag_ms,
      pending_events: statistics.pending_events,
      history_session_id,
      history_retained_bytes,
    })
    .map_err(|error| CommandErrorDto::new("attachment_channel_closed", error.to_string()))
}
