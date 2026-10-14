//! Connection preparation and ctmux exchanges have independent time budgets.

use crate::{
  Result,
  transport::{Stream, Transport},
};
use std::{fmt, future::Future, time::Duration};

pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
pub(crate) enum ConnectionMode {
  // Startup authentication and route preparation retain the connector's policy.
  Startup,
  // Quiet runtime work remains bounded and can be cancelled by UI maintenance.
  Runtime,
}

#[derive(Debug)]
struct DeadlineExceeded {
  host: String,
  operation: String,
  phase: &'static str,
}

impl fmt::Display for DeadlineExceeded {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(
      formatter,
      "{} on {} timed out after {}s ({})",
      self.operation,
      self.host,
      REQUEST_TIMEOUT.as_secs(),
      self.phase
    )
  }
}

impl std::error::Error for DeadlineExceeded {}

pub(crate) async fn connect(
  transport: &dyn Transport,
  operation: &str,
  mode: ConnectionMode,
) -> Result<Stream> {
  match mode {
    ConnectionMode::Startup => transport.connect().await,
    ConnectionMode::Runtime => {
      bounded(
        transport,
        operation,
        "connection preparation",
        transport.connect(),
      )
      .await
    }
  }
}

pub(crate) fn request<'a, T: 'a>(
  transport: &'a dyn Transport,
  operation: &'a str,
  job: impl Future<Output = Result<T>> + 'a,
) -> impl Future<Output = Result<T>> + 'a {
  bounded(
    transport,
    operation,
    "ctmux protocol handshake/response",
    job,
  )
}

pub(crate) fn checkpoint<'a, T: 'a>(
  transport: &'a dyn Transport,
  operation: &'a str,
  job: impl Future<Output = Result<T>> + 'a,
) -> impl Future<Output = Result<T>> + 'a {
  bounded(
    transport,
    operation,
    "initial checkpoint and lease confirmation",
    job,
  )
}

pub(crate) fn confirmation<'a, T: 'a>(
  transport: &'a dyn Transport,
  operation: &'a str,
  job: impl Future<Output = Result<T>> + 'a,
) -> impl Future<Output = Result<T>> + 'a {
  bounded(transport, operation, "view confirmation", job)
}

fn bounded<'a, T: 'a>(
  transport: &'a dyn Transport,
  operation: &'a str,
  phase: &'static str,
  job: impl Future<Output = Result<T>> + 'a,
) -> impl Future<Output = Result<T>> + 'a {
  // Box before constructing the timer future so network state does not inflate
  // the UI's action futures. Cancellation still drops the owned phase directly.
  let job = Box::pin(job);
  async move {
    // Preserve connector/protocol error types used by retry/authentication policy.
    tokio::time::timeout(REQUEST_TIMEOUT, job)
      .await
      .map_err(|_| -> crate::Error {
        Box::new(DeadlineExceeded {
          host: transport.label().into(),
          operation: operation.into(),
          phase,
        })
      })?
  }
}
