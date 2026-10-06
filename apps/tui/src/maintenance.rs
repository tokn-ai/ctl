use crate::{
  Result,
  pane::{Pane, ReconnectLeases},
  transport::{LocalTransport, Transport},
};
use ctmux_proto::{
  ClientMessage, ServerMessage, SessionInfo, SessionStatus, TerminalSize, ViewInfo,
};
use std::{
  collections::BTreeMap, future::Future, path::PathBuf, pin::Pin, task::Poll, time::Duration,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
type Job<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

pub struct Snapshot {
  pub sessions: Vec<SessionInfo>,
  pub view: Option<ViewInfo>,
}

pub struct Reconnect {
  pub terminal_id: String,
  pub size: TerminalSize,
  pub leases: ReconnectLeases,
  pub token: Option<String>,
}

struct Pending<'a, T> {
  generation: u64,
  job: Job<'a, T>,
}

/// Network maintenance is polled beside rendering and local input. Dropping a
/// generation cancels its streams without detached tasks that could publish a
/// connection after the user has switched sessions.
#[derive(Default)]
pub struct Maintenance<'a> {
  generation: u64,
  refresh: Option<Pending<'a, Snapshot>>,
  reconnects: BTreeMap<String, Pending<'a, Pane>>,
  retry_after: BTreeMap<String, tokio::time::Instant>,
}

pub struct Ready {
  pub snapshot: Option<Result<Snapshot>>,
  pub panes: Vec<(String, Result<Pane>)>,
}

impl<'a> Maintenance<'a> {
  pub fn cancel(&mut self) {
    self.generation = self.generation.wrapping_add(1);
    self.refresh = None;
    self.reconnects.clear();
    self.retry_after.clear();
  }

  pub fn refresh(
    &mut self,
    transport: Option<&'a dyn Transport>,
    socket: PathBuf,
    session: Option<String>,
  ) {
    if self.refresh.is_some() {
      return;
    }
    self.refresh = Some(Pending {
      generation: self.generation,
      job: Box::pin(async move {
        let local = LocalTransport(socket);
        let transport = transport.unwrap_or(&local);
        let ServerMessage::SessionList { mut sessions } =
          request(transport, ClientMessage::ListSessions).await?
        else {
          return Err("expected session list".into());
        };
        sessions.retain(|session| session.status == SessionStatus::Running);
        sessions.sort_by_key(|session| (session.created_at_ms, session.session_id.clone()));
        let view = if let Some(session) = session
          && sessions.iter().any(|root| root.session_id == session)
        {
          let ServerMessage::ViewSnapshot { view } =
            request(transport, ClientMessage::GetView { session }).await?
          else {
            return Err("expected view snapshot".into());
          };
          Some(view)
        } else {
          None
        };
        Ok(Snapshot { sessions, view })
      }),
    });
  }

  pub fn reconnect(
    &mut self,
    transport: Option<&'a dyn Transport>,
    socket: PathBuf,
    reconnect: Reconnect,
  ) {
    if self.reconnects.contains_key(&reconnect.terminal_id)
      || self
        .retry_after
        .get(&reconnect.terminal_id)
        .is_some_and(|retry| *retry > tokio::time::Instant::now())
    {
      return;
    }
    let id = reconnect.terminal_id.clone();
    self.reconnects.insert(
      reconnect.terminal_id,
      Pending {
        generation: self.generation,
        job: Box::pin(async move {
          timeout(Box::pin(async move {
            let local = LocalTransport(socket);
            Pane::open(
              transport.unwrap_or(&local),
              &id,
              reconnect.size,
              reconnect.leases,
              reconnect.token,
            )
            .await
          }))
          .await
        }),
      },
    );
  }

  pub fn retain_panes(&mut self, ids: &[String]) {
    self.reconnects.retain(|id, _| ids.contains(id));
    self.retry_after.retain(|id, _| ids.contains(id));
  }

  pub fn cancel_reconnects(&mut self) {
    self.reconnects.clear();
    self.retry_after.clear();
  }

  pub async fn poll(&mut self) -> Ready {
    std::future::poll_fn(|context| {
      let snapshot =
        self
          .refresh
          .as_mut()
          .and_then(|pending| match pending.job.as_mut().poll(context) {
            Poll::Ready(result) => Some((pending.generation, result)),
            Poll::Pending => None,
          });
      let snapshot = snapshot.and_then(|(generation, result)| {
        self.refresh = None;
        (generation == self.generation).then_some(result)
      });
      let mut panes = Vec::new();
      self
        .reconnects
        .retain(|id, pending| match pending.job.as_mut().poll(context) {
          Poll::Ready(result) => {
            if result.is_err() {
              self.retry_after.insert(
                id.clone(),
                tokio::time::Instant::now() + Duration::from_secs(2),
              );
            }
            if pending.generation == self.generation {
              panes.push((id.clone(), result));
            }
            false
          }
          Poll::Pending => true,
        });
      Poll::Ready(Ready { snapshot, panes })
    })
    .await
  }
}

async fn timeout<T>(job: impl Future<Output = Result<T>>) -> Result<T> {
  tokio::time::timeout(REQUEST_TIMEOUT, job).await?
}

async fn request(transport: &dyn Transport, message: ClientMessage) -> Result<ServerMessage> {
  timeout(async {
    let stream = transport.connect().await?;
    Ok(ctmux_client::request(stream, &crate::pane::identity(), message).await?)
  })
  .await
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::transport::{ConnectFuture, Stream};
  use std::sync::atomic::{AtomicUsize, Ordering};

  #[derive(Default)]
  struct BlockedTransport {
    calls: AtomicUsize,
    cancelled: AtomicUsize,
  }

  struct Blocked<'a>(&'a AtomicUsize);

  impl Future for Blocked<'_> {
    type Output = Result<Stream>;

    fn poll(self: Pin<&mut Self>, _: &mut std::task::Context<'_>) -> Poll<Self::Output> {
      Poll::Pending
    }
  }

  impl Drop for Blocked<'_> {
    fn drop(&mut self) {
      self.0.fetch_add(1, Ordering::Relaxed);
    }
  }

  impl Transport for BlockedTransport {
    fn connect(&self) -> ConnectFuture<'_> {
      self.calls.fetch_add(1, Ordering::Relaxed);
      Box::pin(Blocked(&self.cancelled))
    }

    fn archive_key(&self) -> String {
      "blocked".into()
    }
  }

  #[tokio::test]
  async fn switching_generations_cancels_each_attempt_without_blocking_poll() {
    let transport = BlockedTransport::default();
    let mut maintenance = Maintenance::default();
    maintenance.refresh(Some(&transport), PathBuf::new(), Some("old-session".into()));
    for id in ["first", "second", "first"] {
      maintenance.reconnect(
        Some(&transport),
        PathBuf::new(),
        Reconnect {
          terminal_id: id.into(),
          size: TerminalSize::default(),
          leases: ReconnectLeases::new(true, false),
          token: None,
        },
      );
    }
    let ready = tokio::time::timeout(Duration::from_millis(100), maintenance.poll())
      .await
      .expect("blocked network maintenance must yield to the UI");
    assert!(ready.snapshot.is_none() && ready.panes.is_empty());
    assert_eq!(transport.calls.load(Ordering::Relaxed), 3);
    maintenance.cancel();
    assert_eq!(transport.cancelled.load(Ordering::Relaxed), 3);
    maintenance.refresh(Some(&transport), PathBuf::new(), Some("new-session".into()));
    let ready = maintenance.poll().await;
    assert!(ready.snapshot.is_none() && ready.panes.is_empty());
    assert_eq!(transport.calls.load(Ordering::Relaxed), 4);
    maintenance.cancel();
    assert_eq!(transport.cancelled.load(Ordering::Relaxed), 4);
  }
}
