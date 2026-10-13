use crate::{
  Result,
  pane::{Pane, ReconnectLeases},
  transport::{LocalTransport, Transport},
};
use ctmux_client::{
  session::{SessionClient, SessionId},
  view::ViewClient,
};
use ctmux_proto::{SessionInfo, SessionStatus, TerminalSize, ViewInfo};
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
  recovery_generation: u64,
  transport: Option<&'a dyn Transport>,
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
  blocked_reason: Option<String>,
  authentication_block: Option<AuthenticationBlock<'a>>,
}

struct AuthenticationBlock<'a> {
  transport: &'a dyn Transport,
  generation: u64,
}

struct Block<'a> {
  reason: String,
  authentication: Option<AuthenticationBlock<'a>>,
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
    self.blocked_reason = None;
    self.authentication_block = None;
  }

  /// Drop an observation that may predate an explicit session mutation without
  /// changing pane recovery, retry deadlines, or authentication blocking.
  pub fn cancel_refresh(&mut self) {
    self.refresh = None;
  }

  pub fn blocked_reason(&self) -> Option<&str> {
    self.blocked_reason.as_deref()
  }

  pub fn is_reconnecting(&self, terminal_id: &str) -> bool {
    self.reconnects.contains_key(terminal_id)
  }

  pub fn refresh(
    &mut self,
    transport: Option<&'a dyn Transport>,
    socket: PathBuf,
    session: Option<String>,
  ) {
    self.resume_authorized();
    if self.blocked_reason.is_some() || self.refresh.is_some() {
      return;
    }
    self.refresh = Some(Pending {
      generation: self.generation,
      recovery_generation: transport.map_or(0, Transport::recovery_generation),
      transport,
      job: Box::pin(async move {
        let local = LocalTransport(socket);
        let transport = transport.unwrap_or(&local);
        let mut sessions = timeout(async {
          let stream = transport.connect().await?;
          Ok(
            SessionClient::new(stream, crate::pane::identity())
              .list()
              .await?,
          )
        })
        .await?;
        sessions.retain(|session| session.status == SessionStatus::Running);
        sessions.sort_by_key(|session| (session.created_at_ms, session.session_id.clone()));
        let view = if let Some(session) = session
          && sessions.iter().any(|root| root.session_id == session)
        {
          Some(
            timeout(async {
              let stream = transport.connect().await?;
              Ok(
                ViewClient::new(stream, crate::pane::identity())
                  .get(SessionId(session))
                  .await?,
              )
            })
            .await?,
          )
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
    self.resume_authorized();
    if self.blocked_reason.is_some()
      || self.reconnects.contains_key(&reconnect.terminal_id)
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
        recovery_generation: transport.map_or(0, Transport::recovery_generation),
        transport,
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
    self.resume_authorized();
    std::future::poll_fn(|context| {
      let mut blocked_reason = None;
      let snapshot =
        self
          .refresh
          .as_mut()
          .and_then(|pending| match pending.job.as_mut().poll(context) {
            Poll::Ready(result) => {
              blocked_reason = blocking_reason(pending, &result);
              Some((pending.generation, result))
            }
            Poll::Pending => None,
          });
      let snapshot = snapshot.and_then(|(generation, result)| {
        self.refresh = None;
        (generation == self.generation).then_some(result)
      });
      let mut panes = Vec::new();
      self.reconnects.retain(|id, pending| {
        if blocked_reason.is_some() {
          return false;
        }
        match pending.job.as_mut().poll(context) {
          Poll::Ready(result) => {
            blocked_reason = blocking_reason(pending, &result);
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
        }
      });
      if let Some(block) = blocked_reason {
        self.blocked_reason = Some(block.reason);
        self.authentication_block = block.authentication;
        self.refresh = None;
        self.reconnects.clear();
        self.retry_after.clear();
      }
      Poll::Ready(Ready { snapshot, panes })
    })
    .await
  }

  fn resume_authorized(&mut self) {
    if self
      .authentication_block
      .as_ref()
      .is_some_and(|block| block.transport.recovery_generation() != block.generation)
    {
      self.blocked_reason = None;
      self.authentication_block = None;
    }
  }
}

fn blocking_reason<'a, T>(pending: &Pending<'a, T>, result: &Result<T>) -> Option<Block<'a>> {
  let error = result.as_ref().err()?;
  pending
    .transport
    .filter(|transport| !transport.is_retryable(error))
    .map(|transport| Block {
      reason: error.to_string(),
      authentication: transport
        .is_authentication_required(error)
        .then_some(AuthenticationBlock {
          transport,
          generation: pending.recovery_generation,
        }),
    })
}

async fn timeout<T>(job: impl Future<Output = Result<T>>) -> Result<T> {
  tokio::time::timeout(REQUEST_TIMEOUT, job).await?
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::transport::{ConnectFuture, Stream};
  use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

  struct FailedTransport {
    calls: AtomicUsize,
    retryable: bool,
    authentication_required: bool,
    recovery_generation: AtomicU64,
  }

  impl Transport for FailedTransport {
    fn label(&self) -> &'static str {
      "fixture"
    }

    fn connect(&self) -> ConnectFuture<'_> {
      self.calls.fetch_add(1, Ordering::Relaxed);
      Box::pin(async {
        Err(
          std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "authentication required",
          )
          .into(),
        )
      })
    }
    fn archive_key(&self) -> String {
      "failed".into()
    }
    fn is_retryable(&self, _: &crate::Error) -> bool {
      self.retryable
    }
    fn is_authentication_required(&self, _: &crate::Error) -> bool {
      self.authentication_required
    }
    fn recovery_generation(&self) -> u64 {
      self.recovery_generation.load(Ordering::Acquire)
    }
  }

  #[tokio::test]
  async fn authentication_failure_stops_refreshes_and_pane_reconnects_until_reset() {
    let transport = FailedTransport {
      calls: AtomicUsize::new(0),
      retryable: false,
      authentication_required: false,
      recovery_generation: AtomicU64::new(0),
    };
    let mut maintenance = Maintenance::default();
    for _ in 0..5 {
      maintenance.refresh(Some(&transport), PathBuf::new(), None);
      maintenance.reconnect(
        Some(&transport),
        PathBuf::new(),
        Reconnect {
          terminal_id: "pane".into(),
          size: TerminalSize::default(),
          leases: ReconnectLeases::new(true, false),
          token: None,
        },
      );
      maintenance.poll().await;
    }
    assert_eq!(transport.calls.load(Ordering::Relaxed), 1);
    assert_eq!(
      maintenance.blocked_reason(),
      Some("authentication required")
    );
    maintenance.cancel();
    assert!(maintenance.blocked_reason().is_none());
    maintenance.reconnect(
      Some(&transport),
      PathBuf::new(),
      Reconnect {
        terminal_id: "pane".into(),
        size: TerminalSize::default(),
        leases: ReconnectLeases::new(true, false),
        token: None,
      },
    );
    assert_eq!(maintenance.poll().await.panes.len(), 1);
    maintenance.refresh(Some(&transport), PathBuf::new(), None);
    maintenance.poll().await;
    assert_eq!(transport.calls.load(Ordering::Relaxed), 2);
  }

  #[tokio::test]
  async fn transient_connection_failures_allow_later_refreshes() {
    let transport = FailedTransport {
      calls: AtomicUsize::new(0),
      retryable: true,
      authentication_required: false,
      recovery_generation: AtomicU64::new(0),
    };
    let mut maintenance = Maintenance::default();
    for _ in 0..3 {
      maintenance.refresh(Some(&transport), PathBuf::new(), None);
      assert!(maintenance.poll().await.snapshot.unwrap().is_err());
      assert!(maintenance.blocked_reason().is_none());
    }
    assert_eq!(transport.calls.load(Ordering::Relaxed), 3);
  }

  #[tokio::test]
  async fn authorization_resumes_only_authentication_blockers_and_only_once_per_event() {
    for authentication_required in [false, true] {
      let transport = FailedTransport {
        calls: AtomicUsize::new(0),
        retryable: false,
        authentication_required,
        recovery_generation: AtomicU64::new(0),
      };
      let mut maintenance = Maintenance::default();
      maintenance.refresh(Some(&transport), PathBuf::new(), None);
      maintenance.poll().await;
      assert!(maintenance.blocked_reason().is_some());
      transport.recovery_generation.store(1, Ordering::Release);
      maintenance.poll().await;
      assert_eq!(
        maintenance.blocked_reason().is_none(),
        authentication_required
      );
      for _ in 0..3 {
        maintenance.refresh(Some(&transport), PathBuf::new(), None);
        maintenance.poll().await;
      }
      assert!(maintenance.blocked_reason().is_some());
      assert_eq!(
        transport.calls.load(Ordering::Relaxed),
        if authentication_required { 2 } else { 1 }
      );
    }
  }

  #[tokio::test]
  async fn authorization_racing_an_older_failure_resumes_the_blocked_attempt() {
    let transport = FailedTransport {
      calls: AtomicUsize::new(0),
      retryable: false,
      authentication_required: true,
      recovery_generation: AtomicU64::new(0),
    };
    let mut maintenance = Maintenance::default();
    maintenance.refresh(Some(&transport), PathBuf::new(), None);
    transport.recovery_generation.store(1, Ordering::Release);
    maintenance.poll().await;
    assert!(maintenance.blocked_reason().is_some());
    maintenance.poll().await;
    assert!(maintenance.blocked_reason().is_none());
  }

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
    fn label(&self) -> &'static str {
      "fixture"
    }

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
