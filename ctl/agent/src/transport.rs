//! Fixed local services share one disposable SSH command connection.

use std::io;

use ctl_proto::transport::{self as multiplex, Service};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt as _};

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
        Service::Files => {
          let identity = config.identity.as_ref().ok_or_else(|| {
            io::Error::new(
              io::ErrorKind::InvalidInput,
              "File service requires account identity",
            )
          })?;
          return serve_files(&mut reader, &mut writer, identity).await;
        }
      }
      crate::connect(reader, writer, &config)
        .await
        .map_err(io::Error::other)
    }
  })
  .await
}

/// Runs the fixed file endpoint on this process's owned SSH descriptors.
///
/// # Errors
/// Returns identity negotiation, descriptor, or file service errors.
pub async fn serve_files_stdio(identity: &ctl_proto::RemoteIdentity) -> io::Result<()> {
  #[cfg(unix)]
  {
    let (reader, writer) = ctl_ipc::stdio::take()?;
    serve_files(reader, writer, identity).await
  }
  #[cfg(not(unix))]
  serve_files(tokio::io::stdin(), tokio::io::stdout(), identity).await
}

async fn serve_files<R, W>(
  mut reader: R,
  mut writer: W,
  identity: &ctl_proto::RemoteIdentity,
) -> io::Result<()>
where
  R: AsyncRead + Unpin,
  W: AsyncWrite + Unpin,
{
  tokio::time::timeout(ctl_proto::files::STARTUP_TIMEOUT, async {
    writer.write_all(ctl_proto::IDENTITY_PREFACE).await?;
    writer.flush().await?;
    ctl_proto::accept_identity_contract_with_client(&mut reader, &mut writer).await?;
    ctl_proto::write_identity(&mut writer, identity).await
  })
  .await
  .map_err(|_| {
    io::Error::new(
      io::ErrorKind::TimedOut,
      "File service identity negotiation timed out",
    )
  })??;
  ctl_files::serve(tokio::io::join(reader, writer)).await
}

#[cfg(test)]
mod tests {
  use super::*;
  use tokio::io::AsyncReadExt as _;

  #[tokio::test]
  async fn file_stream_negotiates_account_identity_before_the_file_contract() {
    let identity = ctl_proto::RemoteIdentity {
      remote_id: "00000000-0000-4000-8000-000000000001".into(),
      agent_version: "fixture".into(),
      build: None,
      ctmux_restart_supported: false,
      bundle: None,
      protocols: ctl_proto::agent_protocols(),
    };
    let expected = identity.clone();
    let (mut client, server) = tokio::io::duplex(8192);
    let (reader, writer) = tokio::io::split(server);
    let server = tokio::spawn(async move { serve_files(reader, writer, &identity).await });
    let mut marker = [0; ctl_proto::IDENTITY_PREFACE.len()];
    client.read_exact(&mut marker).await.unwrap();
    assert_eq!(&marker, ctl_proto::IDENTITY_PREFACE);
    let (mut reader, mut writer) = tokio::io::split(&mut client);
    ctl_proto::negotiate_identity_contract(&mut reader, &mut writer)
      .await
      .unwrap();
    assert_eq!(
      ctl_proto::read_identity(&mut reader).await.unwrap(),
      expected
    );
    let response = ctl_proto::files::request(
      tokio::io::join(reader, writer),
      &ctl_proto::files::Request::Metadata {
        path: "relative-file".into(),
        cwd: None,
      },
    )
    .await
    .unwrap();
    assert!(
      matches!(response, ctl_proto::files::Response::Error { code, .. } if code == "invalid_request")
    );
    server.await.unwrap().unwrap();
  }
}
