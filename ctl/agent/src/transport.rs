//! Fixed local services share one disposable SSH command connection.

use std::io;

use ctl_proto::transport::{self as multiplex, Service};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::ConnectConfig;

/// Serves logical channels on the process's owned SSH descriptors.
///
/// # Errors
/// Returns descriptor, negotiation, or shared transport errors.
pub async fn serve_stdio(config: &ConnectConfig) -> io::Result<()> {
  #[cfg(unix)]
  {
    let (reader, writer) = ctl_ipc::stdio::take()?;
    serve(reader, writer, config).await
  }
  #[cfg(not(unix))]
  serve(tokio::io::stdin(), tokio::io::stdout(), config).await
}

/// Relays independent logical channels to existing fixed service endpoints.
///
/// Each channel retains its service's identity and framing negotiation. EOF
/// on the shared connection closes the relays while daemon-owned work persists.
///
/// # Errors
/// Returns shared negotiation and transport errors. An individual service's
/// error closes its logical stream without terminating sibling streams.
pub async fn serve<R, W>(reader: R, writer: W, config: &ConnectConfig) -> io::Result<()>
where
  R: AsyncRead + Unpin + Send + 'static,
  W: AsyncWrite + Unpin + Send + 'static,
{
  let config = config.clone();
  let udp_budget = crate::forwarding::UdpBudget::shared();
  multiplex::serve(tokio::io::join(reader, writer), move |service, stream| {
    let mut config = config.clone();
    let udp_budget = udp_budget.clone();
    async move {
      let (mut reader, mut writer) = tokio::io::split(stream);
      match service {
        Service::Ctmux => config.service = crate::Service::Ctmux,
        Service::Task => config.service = crate::Service::Task,
        Service::Udp => {
          return crate::forwarding::serve_with_budget(&mut reader, &mut writer, udp_budget).await;
        }
      }
      crate::connect(reader, writer, &config)
        .await
        .map_err(io::Error::other)
    }
  })
  .await
}
